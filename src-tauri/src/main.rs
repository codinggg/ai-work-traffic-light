// AI Work Traffic Light — Tauri 入口。
// 本单元(U2)：透明/无边框/置顶/不进任务栏的悬浮窗 + 托盘(含退出)。
// 状态推送(U3/U4)、Win32 定位(U6)、通知(U7)、设置(U8) 后续单元接入。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod installer;
mod server;
mod state;
#[cfg(windows)]
mod taskbar;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle,
};

/// Claude Code 的 hook 把事件 POST 到这个本地端口（U3 监听 / U5 安装器写入）。
pub const STATE_PORT: u16 = 48756;

/// 跨线程共享状态：会话状态机 + 上次聚合状态(红灯进入检测) + 声音开关。
pub struct Shared {
    pub store: Mutex<state::Store>,
    pub last_status: Mutex<String>,
    pub sound_enabled: AtomicBool,
}

/// 弹个原生消息框反馈安装/卸载结果。
fn notify_result(app: &AppHandle, result: Result<String, String>) {
    use tauri_plugin_dialog::DialogExt;
    let (title, body) = match result {
        Ok(msg) => ("AI Work Traffic Light".to_string(), msg),
        Err(err) => ("出错了".to_string(), err),
    };
    app.dialog().message(body).title(title).show(|_| {});
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // 开机自启插件。默认启用/关闭的策略放到 U8(设置/托盘菜单)里接。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .setup(|app| {
            // 托盘菜单：安装/卸载 hooks + 退出。（U8 再加声音/自启开关）
            let install =
                MenuItem::with_id(app, "install_hooks", "安装 hooks", true, None::<&str>)?;
            let uninstall =
                MenuItem::with_id(app, "uninstall_hooks", "卸载 hooks", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&install, &uninstall, &sep, &quit])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("AI Work Traffic Light")
                .on_menu_event(|app, event| {
                    let id = event.id();
                    if id == "install_hooks" {
                        notify_result(app, installer::install());
                    } else if id == "uninstall_hooks" {
                        notify_result(app, installer::uninstall());
                    } else if id == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            // U3/U4 状态机 + U7 通知所需的共享状态。
            let shared = Arc::new(Shared {
                store: Mutex::new(state::Store::default()),
                last_status: Mutex::new("none".to_string()),
                sound_enabled: AtomicBool::new(true),
            });
            server::start(app.handle().clone(), shared, STATE_PORT);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Work Traffic Light");
}
