//! Hot kernels: packed GEMM/GEMV and GRU/tanh activations.
//! AVX2+FMA (plus F16C for mixed precision) is selected at construction.
//! WebAssembly uses SIMD128 (MultiVoice addition, see MULTIVOICE.md); other CPUs use the scalar path.

use crate::weights::{Data, Mat};

#[derive(Clone, Copy)]
pub(crate) struct Simd {
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    avx2: bool,
}

impl Simd {
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        let avx2 = is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("fma")
            && is_x86_feature_detected!("f16c");
        #[cfg(not(target_arch = "x86_64"))]
        let avx2 = false;
        Self { avx2 }
    }

    /// `out[r][..n] = relu?(a[r] · w + bias + (acc ? out[r] : 0))` for `r < m`.
    /// `m` must be 1 or a multiple of 4; all `w.n` padded columns are written.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm(
        self,
        m: usize,
        a: &[f32],
        lda: usize,
        w: &Mat,
        bias: Option<&[f32]>,
        acc: bool,
        relu: bool,
        out: &mut [f32],
        ldo: usize,
    ) {
        assert!(m == 1 || m.is_multiple_of(4));
        assert!(a.len() >= (m - 1) * lda + w.k && out.len() >= (m - 1) * ldo + w.n);
        assert!(w.w.len() == w.k * w.n && bias.is_none_or(|b| b.len() >= w.n));
        macro_rules! run {
            ($weights:expr, $expand:expr, $half:literal) => {{
                let weights = $weights;
                #[cfg(target_arch = "x86_64")]
                if self.avx2 {
                    // SAFETY: CPU support checked at construction; bounds asserted above.
                    return unsafe {
                        x86::gemm::<$half>(
                            m,
                            a,
                            lda,
                            w.k,
                            w.n,
                            weights.as_ptr().cast(),
                            bias,
                            acc,
                            relu,
                            out,
                            ldo,
                        )
                    };
                }
                #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
                // SAFETY: bounds asserted above; the module is built with simd128.
                return unsafe {
                    wasm::gemm::<$half>(
                        m,
                        a,
                        lda,
                        w.k,
                        w.n,
                        weights.as_ptr().cast(),
                        bias,
                        acc,
                        relu,
                        out,
                        ldo,
                    )
                };
                #[allow(unreachable_code)]
                for r in 0..m {
                    for n in 0..w.n {
                        let col = &weights[n / 16 * w.k * 16 + n % 16..];
                        let mut sum = 0.0;
                        for i in 0..w.k {
                            sum += a[r * lda + i] * $expand(col[i * 16]);
                        }
                        let o = &mut out[r * ldo + n];
                        sum += bias.map_or(0.0, |b| b[n]) + if acc { *o } else { 0.0 };
                        *o = if relu { sum.max(0.0) } else { sum };
                    }
                }
            }};
        }
        match w.w {
            Data::F32(weights) => run!(weights, std::convert::identity, false),
            Data::F16(weights) => run!(weights, crate::weights::expand, true),
        }
    }

    /// GRU update with reset applied after the recurrent projection.
    /// Packed pre-activations `gi = W x + b_w`, `gh = R h + b_r` use
    /// z (update), r (reset), n (candidate) order.
    pub fn gru_gates(self, gi: &[f32; 768], gh: &[f32; 768], h: &mut [f32; 256]) {
        #[cfg(target_arch = "x86_64")]
        if self.avx2 {
            // SAFETY: CPU support checked at construction; fixed sizes.
            return unsafe { x86::gru_gates(gi, gh, h) };
        }
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        return wasm::gru_gates(gi, gh, h);
        #[allow(unreachable_code)]
        for j in 0..256 {
            let z = sigmoid(gi[j] + gh[j]);
            let r = sigmoid(gi[256 + j] + gh[256 + j]);
            let n = (gi[512 + j] + r * gh[512 + j]).tanh();
            h[j] = n + z * (h[j] - n);
        }
    }

    pub fn tanh(self, x: &mut [f32]) {
        #[cfg(target_arch = "x86_64")]
        if self.avx2 {
            // SAFETY: CPU support checked at construction.
            return unsafe { x86::tanh_slice(x) };
        }
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        return wasm::tanh_slice(x);
        #[allow(unreachable_code)]
        x.iter_mut().for_each(|v| *v = v.tanh());
    }
}

pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*;

    /// One register tile: `R` rows x `T` 16-column tiles (R * T * 2 <= 8 accumulators).
    #[target_feature(enable = "avx2,fma,f16c")]
    #[allow(clippy::too_many_arguments)]
    unsafe fn tile<const R: usize, const T: usize, const HALF: bool>(
        k: usize,
        a: *const f32,
        lda: usize,
        w: *const u8,
        bias: *const f32,
        acc: bool,
        relu: bool,
        out: *mut f32,
        ldo: usize,
    ) {
        unsafe {
            let mut sum = [[[_mm256_setzero_ps(); 2]; T]; R];
            for i in 0..k {
                for t in 0..T {
                    let p = w.add((t * k + i) * 16 * if HALF { 2 } else { 4 });
                    let v = if HALF {
                        [
                            _mm256_cvtph_ps(_mm_load_si128(p.cast())),
                            _mm256_cvtph_ps(_mm_load_si128(p.add(16).cast())),
                        ]
                    } else {
                        [_mm256_load_ps(p.cast()), _mm256_load_ps(p.add(32).cast())]
                    };
                    for (r, row) in sum.iter_mut().enumerate() {
                        let x = _mm256_set1_ps(*a.add(r * lda + i));
                        row[t][0] = _mm256_fmadd_ps(x, v[0], row[t][0]);
                        row[t][1] = _mm256_fmadd_ps(x, v[1], row[t][1]);
                    }
                }
            }
            for (r, row) in sum.iter().enumerate() {
                for (t, s) in row.iter().enumerate() {
                    for (h, &v) in s.iter().enumerate() {
                        let o = out.add(r * ldo + t * 16 + h * 8);
                        let mut v = v;
                        if !bias.is_null() {
                            v = _mm256_add_ps(v, _mm256_loadu_ps(bias.add(t * 16 + h * 8)));
                        }
                        if acc {
                            v = _mm256_add_ps(v, _mm256_loadu_ps(o));
                        }
                        if relu {
                            v = _mm256_max_ps(v, _mm256_setzero_ps());
                        }
                        _mm256_storeu_ps(o, v);
                    }
                }
            }
        }
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn gemm<const HALF: bool>(
        m: usize,
        a: &[f32],
        lda: usize,
        k: usize,
        n: usize,
        wp: *const u8,
        bias: Option<&[f32]>,
        acc: bool,
        relu: bool,
        out: &mut [f32],
        ldo: usize,
    ) {
        let tiles = n / 16;
        let bytes = if HALF { 2 } else { 4 };
        let (a, o) = (a.as_ptr(), out.as_mut_ptr());
        let bias = |t: usize| bias.map_or(std::ptr::null(), |b| b[t * 16..].as_ptr());
        unsafe {
            if m == 1 {
                let mut t = 0;
                while t + 4 <= tiles {
                    tile::<1, 4, HALF>(
                        k,
                        a,
                        0,
                        wp.add(t * k * 16 * bytes),
                        bias(t),
                        acc,
                        relu,
                        o.add(t * 16),
                        0,
                    );
                    t += 4;
                }
                while t + 2 <= tiles {
                    tile::<1, 2, HALF>(
                        k,
                        a,
                        0,
                        wp.add(t * k * 16 * bytes),
                        bias(t),
                        acc,
                        relu,
                        o.add(t * 16),
                        0,
                    );
                    t += 2;
                }
                if t < tiles {
                    tile::<1, 1, HALF>(
                        k,
                        a,
                        0,
                        wp.add(t * k * 16 * bytes),
                        bias(t),
                        acc,
                        relu,
                        o.add(t * 16),
                        0,
                    );
                }
            } else {
                for t in 0..tiles {
                    for r in (0..m).step_by(4) {
                        let (a, o) = (a.add(r * lda), o.add(r * ldo + t * 16));
                        tile::<4, 1, HALF>(
                            k,
                            a,
                            lda,
                            wp.add(t * k * 16 * bytes),
                            bias(t),
                            acc,
                            relu,
                            o,
                            ldo,
                        );
                    }
                }
            }
        }
    }

    /// Cephes-style exp, valid after clamping to the finite f32 range.
    #[target_feature(enable = "avx2,fma")]
    fn exp(x: __m256) -> __m256 {
        let x = _mm256_min_ps(
            _mm256_max_ps(x, _mm256_set1_ps(-87.0)),
            _mm256_set1_ps(88.0),
        );
        let n = _mm256_round_ps::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(
            _mm256_mul_ps(x, _mm256_set1_ps(std::f32::consts::LOG2_E)),
        );
        let f = _mm256_fnmadd_ps(n, _mm256_set1_ps(0.693_359_4), x);
        let f = _mm256_fnmadd_ps(n, _mm256_set1_ps(-2.121_944_4e-4), f);
        let mut p = _mm256_set1_ps(1.987_569_1e-4);
        for c in [
            1.398_199_9e-3,
            8.333_452e-3,
            4.166_579_6e-2,
            1.666_666_5e-1,
            0.5,
        ] {
            p = _mm256_fmadd_ps(p, f, _mm256_set1_ps(c));
        }
        let y = _mm256_fmadd_ps(
            p,
            _mm256_mul_ps(f, f),
            _mm256_add_ps(f, _mm256_set1_ps(1.0)),
        );
        let scale = _mm256_slli_epi32::<23>(_mm256_add_epi32(
            _mm256_cvtps_epi32(n),
            _mm256_set1_epi32(127),
        ));
        _mm256_mul_ps(y, _mm256_castsi256_ps(scale))
    }

    #[target_feature(enable = "avx2,fma")]
    fn sigmoid(x: __m256) -> __m256 {
        let one = _mm256_set1_ps(1.0);
        _mm256_div_ps(
            one,
            _mm256_add_ps(one, exp(_mm256_sub_ps(_mm256_setzero_ps(), x))),
        )
    }

    #[target_feature(enable = "avx2,fma")]
    fn tanh(x: __m256) -> __m256 {
        let two = _mm256_set1_ps(2.0);
        _mm256_fmsub_ps(two, sigmoid(_mm256_mul_ps(two, x)), _mm256_set1_ps(1.0))
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn gru_gates(gi: &[f32; 768], gh: &[f32; 768], h: &mut [f32; 256]) {
        let (gi, gh, hp) = (gi.as_ptr(), gh.as_ptr(), h.as_mut_ptr());
        for j in (0..256).step_by(8) {
            unsafe {
                let ld = |p: *const f32, o: usize| _mm256_loadu_ps(p.add(o + j));
                let z = sigmoid(_mm256_add_ps(ld(gi, 0), ld(gh, 0)));
                let r = sigmoid(_mm256_add_ps(ld(gi, 256), ld(gh, 256)));
                let n = tanh(_mm256_fmadd_ps(r, ld(gh, 512), ld(gi, 512)));
                let old = _mm256_loadu_ps(hp.add(j));
                _mm256_storeu_ps(hp.add(j), _mm256_fmadd_ps(z, _mm256_sub_ps(old, n), n));
            }
        }
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn tanh_slice(x: &mut [f32]) {
        let (chunks, tail) = x.as_chunks_mut::<8>();
        for c in chunks {
            unsafe { _mm256_storeu_ps(c.as_mut_ptr(), tanh(_mm256_loadu_ps(c.as_ptr()))) };
        }
        tail.iter_mut().for_each(|v| *v = v.tanh());
    }
}

/// WebAssembly SIMD128 kernels (added by MultiVoice). Same packed layout as the x86 path:
/// 16-column tiles; FP16 weights are expanded in registers.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod wasm {
    use core::arch::wasm32::*;

    /// 8 halves (one v128) -> two f32x4. Exact for normal and subnormal FP16
    /// (the packed weights hold no inf/NaN).
    #[inline(always)]
    fn halves(v: v128) -> [v128; 2] {
        let cvt = |u: v128| {
            let sign = i32x4_shl(v128_and(u, u32x4_splat(0x8000)), 16);
            let mag = i32x4_shl(v128_and(u, u32x4_splat(0x7fff)), 13);
            // magnitude bits reinterpreted as f32, times 2^112, re-biases the exponent
            v128_or(sign, f32x4_mul(mag, f32x4_splat(f32::from_bits(0x7780_0000))))
        };
        [cvt(u32x4_extend_low_u16x8(v)), cvt(u32x4_extend_high_u16x8(v))]
    }

    /// One register tile: `R` rows x `T` 16-column tiles (R * T * 4 accumulators).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    unsafe fn tile<const R: usize, const T: usize, const HALF: bool>(
        k: usize,
        a: *const f32,
        lda: usize,
        w: *const u8,
        bias: *const f32,
        acc: bool,
        relu: bool,
        out: *mut f32,
        ldo: usize,
    ) {
        unsafe {
            let mut sum = [[[f32x4_splat(0.0); 4]; T]; R];
            for i in 0..k {
                for t in 0..T {
                    let p = w.add((t * k + i) * 16 * if HALF { 2 } else { 4 });
                    let v = if HALF {
                        let [a0, a1] = halves(v128_load(p.cast()));
                        let [b0, b1] = halves(v128_load(p.add(16).cast()));
                        [a0, a1, b0, b1]
                    } else {
                        [
                            v128_load(p.cast()),
                            v128_load(p.add(16).cast()),
                            v128_load(p.add(32).cast()),
                            v128_load(p.add(48).cast()),
                        ]
                    };
                    for (r, row) in sum.iter_mut().enumerate() {
                        let x = f32x4_splat(*a.add(r * lda + i));
                        for q in 0..4 {
                            row[t][q] = f32x4_add(row[t][q], f32x4_mul(x, v[q]));
                        }
                    }
                }
            }
            for (r, row) in sum.iter().enumerate() {
                for (t, s) in row.iter().enumerate() {
                    for (q, &v) in s.iter().enumerate() {
                        let o = out.add(r * ldo + t * 16 + q * 4);
                        let mut v = v;
                        if !bias.is_null() {
                            v = f32x4_add(v, v128_load(bias.add(t * 16 + q * 4).cast()));
                        }
                        if acc {
                            v = f32x4_add(v, v128_load(o.cast()));
                        }
                        if relu {
                            v = f32x4_max(v, f32x4_splat(0.0));
                        }
                        v128_store(o.cast(), v);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn gemm<const HALF: bool>(
        m: usize,
        a: &[f32],
        lda: usize,
        k: usize,
        n: usize,
        wp: *const u8,
        bias: Option<&[f32]>,
        acc: bool,
        relu: bool,
        out: &mut [f32],
        ldo: usize,
    ) {
        let tiles = n / 16;
        let bytes = if HALF { 2 } else { 4 };
        let (a, o) = (a.as_ptr(), out.as_mut_ptr());
        let bias = |t: usize| bias.map_or(std::ptr::null(), |b| b[t * 16..].as_ptr());
        unsafe {
            if m == 1 {
                let mut t = 0;
                while t + 2 <= tiles {
                    let w = wp.add(t * k * 16 * bytes);
                    tile::<1, 2, HALF>(k, a, 0, w, bias(t), acc, relu, o.add(t * 16), 0);
                    t += 2;
                }
                if t < tiles {
                    let w = wp.add(t * k * 16 * bytes);
                    tile::<1, 1, HALF>(k, a, 0, w, bias(t), acc, relu, o.add(t * 16), 0);
                }
            } else {
                for t in 0..tiles {
                    for r in (0..m).step_by(2) {
                        let (a, o) = (a.add(r * lda), o.add(r * ldo + t * 16));
                        let w = wp.add(t * k * 16 * bytes);
                        tile::<2, 1, HALF>(k, a, lda, w, bias(t), acc, relu, o, ldo);
                    }
                }
            }
        }
    }

    /// Cephes-style exp (same constants as the x86 path).
    #[inline(always)]
    fn exp(x: v128) -> v128 {
        let x = f32x4_min(f32x4_max(x, f32x4_splat(-87.0)), f32x4_splat(88.0));
        let n = f32x4_nearest(f32x4_mul(x, f32x4_splat(std::f32::consts::LOG2_E)));
        let f = f32x4_sub(x, f32x4_mul(n, f32x4_splat(0.693_359_4)));
        let f = f32x4_sub(f, f32x4_mul(n, f32x4_splat(-2.121_944_4e-4)));
        let mut p = f32x4_splat(1.987_569_1e-4);
        for c in [1.398_199_9e-3, 8.333_452e-3, 4.166_579_6e-2, 1.666_666_5e-1, 0.5] {
            p = f32x4_add(f32x4_mul(p, f), f32x4_splat(c));
        }
        let y = f32x4_add(f32x4_mul(p, f32x4_mul(f, f)), f32x4_add(f, f32x4_splat(1.0)));
        let scale = i32x4_shl(i32x4_add(i32x4_trunc_sat_f32x4(n), i32x4_splat(127)), 23);
        f32x4_mul(y, scale)
    }

    #[inline(always)]
    fn sigmoid(x: v128) -> v128 {
        let one = f32x4_splat(1.0);
        f32x4_div(one, f32x4_add(one, exp(f32x4_neg(x))))
    }

    #[inline(always)]
    fn tanh(x: v128) -> v128 {
        let two = f32x4_splat(2.0);
        f32x4_sub(f32x4_mul(two, sigmoid(f32x4_mul(two, x))), f32x4_splat(1.0))
    }

    pub(super) fn gru_gates(gi: &[f32; 768], gh: &[f32; 768], h: &mut [f32; 256]) {
        let (gi, gh, hp) = (gi.as_ptr(), gh.as_ptr(), h.as_mut_ptr());
        for j in (0..256).step_by(4) {
            // SAFETY: fixed sizes, j + 4 <= 256.
            unsafe {
                let ld = |p: *const f32, o: usize| v128_load(p.add(o + j).cast());
                let z = sigmoid(f32x4_add(ld(gi, 0), ld(gh, 0)));
                let r = sigmoid(f32x4_add(ld(gi, 256), ld(gh, 256)));
                let n = tanh(f32x4_add(f32x4_mul(r, ld(gh, 512)), ld(gi, 512)));
                let old = v128_load(hp.add(j).cast());
                v128_store(hp.add(j).cast(), f32x4_add(f32x4_mul(z, f32x4_sub(old, n)), n));
            }
        }
    }

    pub(super) fn tanh_slice(x: &mut [f32]) {
        let (chunks, tail) = x.as_chunks_mut::<4>();
        for c in chunks {
            // SAFETY: 4 floats per chunk.
            unsafe { v128_store(c.as_mut_ptr().cast(), tanh(v128_load(c.as_ptr().cast()))) };
        }
        tail.iter_mut().for_each(|v| *v = v.tanh());
    }
}
