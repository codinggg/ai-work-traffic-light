//! macOS dock 图标：提醒(红/黄)时在 dock 显示并闪烁红绿灯。
//!
//! Windows/Linux 没有“dock”概念(用任务栏/托盘)，故本模块只在 macOS 编译。
//! 通过 objc2 的 `msg_send!` 直接调 AppKit：
//!   `[[NSApplication sharedApplication] setApplicationIconImage: img]`
//! 传 nil 时恢复 bundle 自带的默认 dock 图标。
//!
//! 用运行时取类(`class!`)+消息发送，而不是 objc2-app-kit 的类型绑定 —— 这样不依赖
//! objc2-app-kit 的任何 feature，跨版本最稳(和 tao 的 cursor.rs / progress_bar.rs 同一套写法)。
//! 图标从红绿灯 RGBA 先编成 PNG，再 `[[NSImage alloc] initWithData:]` 构造，避开
//! NSBitmapImageRep 的十余参构造器。
//!
//! 注意：`set_dock_image` 操作 AppKit UI，必须在主线程调用 —— 调用方(main.rs 动画线程)
//! 用 `AppHandle::run_on_main_thread` 投递过去。

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;

type Id = *mut AnyObject;

/// 把红绿灯 `Image`(RGBA8) 编码成 PNG 字节，供 `[NSImage initWithData:]` 使用。
/// 编码很轻(64x64)，可在后台线程做；失败返回 None。
pub fn encode_png(img: &tauri::image::Image) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.width(), img.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(img.rgba()).ok()?;
    }
    Some(out)
}

/// 设置 dock 图标。`Some(png)` 用该 PNG 当图标；`None` 恢复默认 dock 图标(传 nil)。
///
/// # Safety / 线程
/// 必须在主线程调用(AppKit 要求)。内部全是 objc 消息：
/// - `[NSData dataWithBytes:length:]` 复制字节并返回 autorelease 的 NSData(我们不持有所有权)；
/// - `[[NSImage alloc] initWithData:]` 返回 +1 引用，`autorelease` 抵消，`setApplicationIconImage:`
///   会 retain，运行循环的 autorelease pool 排空后净由 AppKit 持有 —— 不泄漏。
pub fn set_dock_image(png: Option<Vec<u8>>) {
    unsafe {
        let ns_app: Id = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            return;
        }
        let image: Id = match png.as_deref() {
            Some(bytes) if !bytes.is_empty() => {
                let data: Id = msg_send![
                    class!(NSData),
                    dataWithBytes: bytes.as_ptr() as *const c_void,
                    length: bytes.len(),
                ];
                if data.is_null() {
                    return;
                }
                let img: Id = msg_send![class!(NSImage), alloc];
                let img: Id = msg_send![img, initWithData: data];
                if img.is_null() {
                    // 数据无效：按 Cocoa 约定 init 已释放 self，直接返回(不改 dock)。
                    return;
                }
                let _: Id = msg_send![img, autorelease];
                img
            }
            // None / 空 -> nil：恢复 bundle 默认 dock 图标。
            _ => std::ptr::null_mut(),
        };
        let _: () = msg_send![ns_app, setApplicationIconImage: image];
    }
}
