//! 圖示的形狀（純函式，不依賴 egui）：應用程式圖示（視窗、build.rs 產生的 exe 圖示 .ico，跟網頁版 icon.svg 同圖案）
//! 與系統匣的麥克風狀態圖示

/// 品牌紫：系統匣「在房間裡」的底色，也是應用程式圖示漸層的起點色
pub const BRAND: [u8; 3] = [124, 58, 237];

/// 圓角方塊（底色 `bg`）上的白色麥克風，`n`×`n` 的 RGBA。系統匣用不同底色表示狀態；
/// `slash` = 加一條斜線（靜音，色弱也分得出來）
pub fn icon_rgba(n: usize, bg: [u8; 3], slash: bool) -> Vec<u8> {
    let mut rgba = vec![0u8; n * n * 4];
    // 以 64×64 的座標設計，依 n 縮放；每個像素取 4×4 子取樣做反鋸齒
    let k = 64.0 / n as f32;
    for y in 0..n {
        for x in 0..n {
            let (mut inside, mut white) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = (x as f32 + (sx as f32 + 0.5) / 4.0) * k;
                    let fy = (y as f32 + (sy as f32 + 0.5) / 4.0) * k;
                    match sample(fx, fy, slash) {
                        0 => {}
                        1 => inside += 1,
                        _ => {
                            inside += 1;
                            white += 1;
                        }
                    }
                }
            }
            if inside == 0 {
                continue;
            }
            let t = white as f32 / inside as f32;
            let px = &mut rgba[(y * n + x) * 4..][..4];
            for c in 0..3 {
                px[c] = (bg[c] as f32 * (1.0 - t) + 255.0 * t).round() as u8;
            }
            px[3] = (inside * 255 / 16) as u8;
        }
    }
    rgba
}

/// 應用程式圖示（exe、視窗）：跟網頁版 `crates/web/www/icon.svg` 同一個圖案——
/// 紫→洋紅斜向漸層的圓角方塊上四條白色聲波直條。`n`×`n` 的 RGBA。
/// 座標是把 SVG 的 512×512 除以 8；改 icon.svg 時這裡要跟著改
pub fn app_icon_rgba(n: usize) -> Vec<u8> {
    const FROM: [f32; 3] = [124.0, 58.0, 237.0]; // #7c3aed（= BRAND）
    const TO: [f32; 3] = [162.0, 28.0, 175.0]; // #a21caf
    // (x, 上端 y, 下端 y)；線寬 34/8、圓頭
    const BARS: [(f32, f32, f32); 4] = [(18.25, 24.5, 39.5), (27.0, 18.25, 45.75), (35.75, 23.25, 40.75), (44.5, 27.0, 37.0)];
    const HALF_W: f32 = 34.0 / 8.0 / 2.0;
    let mut rgba = vec![0u8; n * n * 4];
    let k = 64.0 / n as f32;
    // 小尺寸（< 64 px）直條只有 1～3 px 寬，照比例畫會落在半個像素上糊掉：
    // 改成對齊整數像素的方塊（寬度、間距取整後整組置中），以像素座標表示 (左, 右, 上, 下)
    let hinted: Option<Vec<(f32, f32, f32, f32)>> = (n < 64).then(|| {
        let w = (2.0 * HALF_W / k).round().max(1.0);
        let step = ((BARS[1].0 - BARS[0].0) / k).round().max(w + 1.0);
        let center = (BARS[0].0 + BARS[3].0) / 2.0 / k;
        let left = (center - (3.0 * step + w) / 2.0).round();
        BARS.iter()
            .enumerate()
            .map(|(i, &(_, y0, y1))| {
                let l = left + i as f32 * step;
                (l, l + w, ((y0 - HALF_W) / k).round(), ((y1 + HALF_W) / k).round())
            })
            .collect()
    });
    for y in 0..n {
        for x in 0..n {
            let (mut inside, mut white) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = (x as f32 + (sx as f32 + 0.5) / 4.0) * k;
                    let fy = (y as f32 + (sy as f32 + 0.5) / 4.0) * k;
                    // 圓角方塊（rx 112/8 = 14，填滿整個 64×64）
                    let r = 14.0;
                    let dx = ((fx - 32.0).abs() - (32.0 - r)).max(0.0);
                    let dy = ((fy - 32.0).abs() - (32.0 - r)).max(0.0);
                    if dx * dx + dy * dy > r * r {
                        continue;
                    }
                    inside += 1;
                    let on_bar = match &hinted {
                        Some(bars) => {
                            let (px, py) = (fx / k, fy / k);
                            bars.iter().any(|&(l, r, t, b)| (l..r).contains(&px) && (t..b).contains(&py))
                        }
                        None => BARS.iter().any(|&(bx, y0, y1)| {
                            let (ddx, ddy) = (fx - bx, fy - fy.clamp(y0, y1));
                            ddx * ddx + ddy * ddy <= HALF_W * HALF_W
                        }),
                    };
                    if on_bar {
                        white += 1;
                    }
                }
            }
            if inside == 0 {
                continue;
            }
            // 漸層：SVG 的 x1=0 y1=0 x2=1 y2=1（正方形上就是沿對角線）
            let cx = (x as f32 + 0.5) * k;
            let cy = (y as f32 + 0.5) * k;
            let g = ((cx + cy) / 128.0).clamp(0.0, 1.0);
            let t = white as f32 / inside as f32;
            let px = &mut rgba[(y * n + x) * 4..][..4];
            for c in 0..3 {
                let bg = FROM[c] + (TO[c] - FROM[c]) * g;
                px[c] = (bg * (1.0 - t) + 255.0 * t).round() as u8;
            }
            px[3] = (inside * 255 / 16) as u8;
        }
    }
    rgba
}

/// 64×64 座標上的一點：0 = 外面、1 = 底色、2 = 麥克風（白）
pub fn sample(fx: f32, fy: f32, slash: bool) -> u8 {
    // 圓角方塊背景
    let r = 14.0;
    let dx = ((fx - 32.0).abs() - (30.0 - r)).max(0.0);
    let dy = ((fy - 32.0).abs() - (30.0 - r)).max(0.0);
    if dx * dx + dy * dy > r * r {
        return 0;
    }
    // 麥克風頭：圓角膠囊
    let cap = {
        let cy = fy.clamp(19.0, 29.0);
        let (ddx, ddy) = (fx - 32.0, fy - cy);
        ddx * ddx + ddy * ddy <= 7.5 * 7.5
    };
    // 支架：下半圓弧
    let arc = {
        let (ddx, ddy) = (fx - 32.0, fy - 29.0);
        let d = (ddx * ddx + ddy * ddy).sqrt();
        fy >= 29.0 && (11.0..=14.0).contains(&d)
    };
    if slash {
        // 左上到右下的斜線，兩側留一圈底色把麥克風切開
        let d = (fx - fy).abs() / std::f32::consts::SQRT_2;
        let on = (12.0..=52.0).contains(&fx);
        if on && d <= 2.6 {
            return 2;
        }
        if on && d <= 5.2 {
            return 1;
        }
    }
    let stem = (30.5..=33.5).contains(&fx) && (42.0..=49.0).contains(&fy);
    let base = (24.0..=40.0).contains(&fx) && (48.0..=51.0).contains(&fy);
    if cap || arc || stem || base { 2 } else { 1 }
}
