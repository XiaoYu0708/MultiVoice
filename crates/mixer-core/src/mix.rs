//! 混音：多個說話者的解碼結果相加，最後做柔性限幅避免爆音。

/// `out += src × gain`
pub fn mix_into(out: &mut [f32], src: &[f32], gain: f32) {
    for (o, &s) in out.iter_mut().zip(src) {
        *o += s * gain;
    }
}

/// 立體聲：`out`（左右交錯）+= mono `src` × gain，依 pan 分配左右（見 [`crate::route::pan_gains`]）
pub fn mix_into_stereo(out: &mut [f32], src: &[f32], gain: f32, pan: f32) {
    let (gl, gr) = crate::route::pan_gains(pan);
    let (gl, gr) = (gl * gain, gr * gain);
    for (o, &s) in out.chunks_exact_mut(2).zip(src) {
        o[0] += s * gl;
        o[1] += s * gr;
    }
}

/// 柔性限幅：|x| ≤ 0.8 時不變，超過後平滑壓縮，輸出永遠在 (-1, 1) 內
pub fn soft_clip(buf: &mut [f32]) {
    const KNEE: f32 = 0.8;
    for x in buf {
        let a = x.abs();
        if a > KNEE {
            let over = a - KNEE;
            let y = KNEE + (1.0 - KNEE) * (over / (1.0 - KNEE)).tanh();
            *x = y.copysign(*x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_and_clipping() {
        let mut out = vec![0.0; 4];
        mix_into(&mut out, &[0.5, -0.5, 0.9, 0.1], 1.0);
        mix_into(&mut out, &[0.5, -0.5, 0.9, 0.1], 0.5);
        for (got, want) in out.iter().zip([0.75, -0.75, 1.35, 0.15]) {
            assert!((got - want).abs() < 1e-6, "{out:?}");
        }
        soft_clip(&mut out);
        assert_eq!(out[0], 0.75, "膝點以下不變");
        assert!(out[2] < 1.0 && out[2] > 0.8, "超過膝點被壓縮：{}", out[2]);
        assert_eq!(out[1], -0.75);
    }

    #[test]
    fn stereo_pan() {
        let mut out = vec![0.0; 4];
        mix_into_stereo(&mut out, &[0.5, 0.5], 1.0, -1.0);
        assert_eq!(out, vec![0.5, 0.0, 0.5, 0.0], "全左");
        mix_into_stereo(&mut out, &[0.5, 0.5], 0.5, 0.0);
        assert_eq!(out, vec![0.75, 0.25, 0.75, 0.25], "置中疊加");
    }
}
