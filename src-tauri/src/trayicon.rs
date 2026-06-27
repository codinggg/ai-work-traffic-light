//! 极简模式的托盘图标。
//!
//! 托盘图标只能用【图片】(PNG/ICO/原始 RGBA)，不能用 CSS/HTML —— 所以这里在 Rust 里手绘
//! 一个对应颜色的小灯(圆形)。按状态切换图标、闪烁状态由定时器在 lit(亮)↔暗 之间切换。
//! 透明底，64x64（OS 自行缩放到托盘尺寸）。非工作态(neutral)直接用应用自带的红绿灯图标，
//! 不在这里画。

use tauri::image::Image;

const S: u32 = 64;

/// 极简模式各状态的灯色（绿/黄/红）。
pub const GREEN: [u8; 3] = [0x2e, 0xd1, 0x4e];
pub const YELLOW: [u8; 3] = [0xf5, 0xb3, 0x1e];
pub const RED: [u8; 3] = [0xf5, 0x3b, 0x30];

/// 画一个指定颜色的小灯图标。lit=true 饱满+高光；false 熄灭(暗色本色)。
/// 透明底，圆形，带 1px 抗锯齿边。返回拥有所有权的 Image，可安全跨线程长期持有。
pub fn lamp_image(rgb: [u8; 3], lit: bool) -> Image<'static> {
    let s = S as f32;
    let cx = s / 2.0;
    let cy = s / 2.0;
    let r = s / 2.0 - 3.0; // 留点边
    let mut buf = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = (r - dist + 0.5).clamp(0.0, 1.0); // 圆内=1，边缘 1px 渐变(抗锯齿)
            if alpha <= 0.0 {
                continue;
            }
            let (mut cr, mut cg, mut cb) = (rgb[0] as f32, rgb[1] as f32, rgb[2] as f32);
            if lit {
                // 左上高光亮斑
                let hx = dx + r * 0.3;
                let hy = dy + r * 0.3;
                let hl = (1.0 - (hx * hx + hy * hy).sqrt() / (r * 1.3)).max(0.0);
                let glow = 95.0 * hl;
                // 边缘略暗，显出球面感
                let edge = 1.0 - 0.4 * (dist / r) * (dist / r);
                cr = (cr * edge + glow).min(255.0);
                cg = (cg * edge + glow).min(255.0);
                cb = (cb * edge + glow).min(255.0);
            } else {
                // 熄灭(关灯)：接近黑的暗圆，和亮色交替形成明显闪烁(红↔黑 / 黄↔黑)。
                cr = 34.0;
                cg = 36.0;
                cb = 42.0;
            }
            let i = ((y * S + x) * 4) as usize;
            buf[i] = cr as u8;
            buf[i + 1] = cg as u8;
            buf[i + 2] = cb as u8;
            buf[i + 3] = (alpha * 255.0) as u8;
        }
    }
    Image::new_owned(buf, S, S)
}
