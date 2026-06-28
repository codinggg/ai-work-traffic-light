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
/// （单灯版：已被 traffic_light_image「灯框+3灯」取代，保留备用。）
#[allow(dead_code)]
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

/// 红绿灯三个灯之一。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lamp {
    Red,
    Yellow,
    Green,
}

/// 外壳颜色(略比纯黑亮，暗任务栏上也能看出灯框)。
const HOUSING: [u8; 3] = [0x20, 0x24, 0x2b];

/// 画「灯框 + 3 个灯」的完整红绿灯图标（极简模式：各平台托盘 + mac/Linux dock 用）。
/// active = 当前点亮哪个灯(None=全灭/中性)；lit = 该灯亮(true)/灭(false，用于闪烁的灭相)。
/// 非当前灯一律暗。竖向排布(红上/黄中/绿下)，和桌面红绿灯一致。
pub fn traffic_light_image(active: Option<Lamp>, lit: bool) -> Image<'static> {
    let mut buf = vec![0u8; (S * S * 4) as usize];
    // 外壳：竖向圆角矩形
    let (x0, y0, x1, y1, rad) = (18.0_f32, 2.0, 46.0, 62.0, 10.0);
    for y in 0..S {
        for x in 0..S {
            let a = rrect_alpha(x as f32 + 0.5, y as f32 + 0.5, x0, y0, x1, y1, rad);
            if a > 0.0 {
                let i = ((y * S + x) * 4) as usize;
                buf[i] = HOUSING[0];
                buf[i + 1] = HOUSING[1];
                buf[i + 2] = HOUSING[2];
                buf[i + 3] = (a * 255.0) as u8;
            }
        }
    }
    // 三个灯(红上/黄中/绿下)
    for (lamp, rgb, cy) in [
        (Lamp::Red, RED, 15.5_f32),
        (Lamp::Yellow, YELLOW, 32.0),
        (Lamp::Green, GREEN, 48.5),
    ] {
        let on = active == Some(lamp) && lit;
        draw_lamp(&mut buf, 32.0, cy, 9.0, rgb, on);
    }
    Image::new_owned(buf, S, S)
}

/// 圆角矩形在 (px,py) 的覆盖率(抗锯齿)：内部=1，边缘 1px 渐变，外部=0。
fn rrect_alpha(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, rad: f32) -> f32 {
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let hx = (x1 - x0) / 2.0 - rad;
    let hy = (y1 - y0) / 2.0 - rad;
    let qx = ((px - cx).abs() - hx).max(0.0);
    let qy = ((py - cy).abs() - hy).max(0.0);
    let d = (qx * qx + qy * qy).sqrt() - rad; // <0 在内部
    (0.5 - d).clamp(0.0, 1.0)
}

/// 在 buf 上(叠加到外壳之上)画一个灯：on=亮(饱满+高光)，否则暗(熄灭本色)。
fn draw_lamp(buf: &mut [u8], cx: f32, cy: f32, r: f32, rgb: [u8; 3], on: bool) {
    let xa = (cx - r - 2.0).floor().max(0.0) as u32;
    let xb = (cx + r + 2.0).ceil().min(S as f32) as u32;
    let ya = (cy - r - 2.0).floor().max(0.0) as u32;
    let yb = (cy + r + 2.0).ceil().min(S as f32) as u32;
    for y in ya..yb {
        for x in xa..xb {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let a = (r - dist + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let (mut cr, mut cg, mut cb) = (rgb[0] as f32, rgb[1] as f32, rgb[2] as f32);
            if on {
                let hx = dx + r * 0.3;
                let hy = dy + r * 0.3;
                let hl = (1.0 - (hx * hx + hy * hy).sqrt() / (r * 1.3)).max(0.0);
                let glow = 80.0 * hl;
                let edge = 1.0 - 0.4 * (dist / r) * (dist / r);
                cr = (cr * edge + glow).min(255.0);
                cg = (cg * edge + glow).min(255.0);
                cb = (cb * edge + glow).min(255.0);
            } else {
                // 熄灭：暗的本色(想更暗/更黑就改这里的系数)
                cr *= 0.22;
                cg *= 0.22;
                cb *= 0.22;
            }
            // 叠加到外壳上(alpha 合成)
            let i = ((y * S + x) * 4) as usize;
            let ea = buf[i + 3] as f32 / 255.0;
            let oa = a + ea * (1.0 - a);
            if oa <= 0.0 {
                continue;
            }
            buf[i] = ((cr * a + buf[i] as f32 * ea * (1.0 - a)) / oa).min(255.0) as u8;
            buf[i + 1] = ((cg * a + buf[i + 1] as f32 * ea * (1.0 - a)) / oa).min(255.0) as u8;
            buf[i + 2] = ((cb * a + buf[i + 2] as f32 * ea * (1.0 - a)) / oa).min(255.0) as u8;
            buf[i + 3] = (oa * 255.0).min(255.0) as u8;
        }
    }
}
