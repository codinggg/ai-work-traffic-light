// AI Work Traffic Light — Tauri 入口。
// 本单元(U2)：透明/无边框/置顶/不进任务栏的悬浮窗 + 托盘(含退出)。
// 状态推送(U3/U4)、Win32 定位(U6)、通知(U7)、设置(U8) 后续单元接入。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;
mod state;

use std::sync::{Arc, Mutex};

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

/// Claude Code 的 hook 把事件 POST 到这个本地端口（U3 监听 / U5 安装器写入）。
pub const STATE_PORT: u16 = 48756;

fn main() {
    tauri::Builder::default()
        // 开机自启插件。默认启用/关闭的策略放到 U8(设置/托盘菜单)里接。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .setup(|app| {
            // 托盘图标 + 最小菜单（退出）。后续 U8 扩展设置项。
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("AI Work Traffic Light")
                .on_menu_event(|app, event| {
                    if event.id() == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            // U3/U4：本地状态接入端点 + 状态机/多会话聚合。
            let store = Arc::new(Mutex::new(state::Store::default()));
            server::start(app.handle().clone(), store, STATE_PORT);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Work Traffic Light");
}
