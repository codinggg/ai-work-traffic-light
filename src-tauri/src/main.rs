// AI Work Traffic Light — Tauri 入口。
// 组装：透明置顶悬浮窗 + 托盘(提示音/自启开关、安装/卸载 hooks、退出)，
// 本地状态端点(U3) + 状态机(U4)，红灯通知(U7)，任务栏定位(U6)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod installer;
mod server;
mod state;
#[cfg(windows)]
mod taskbar;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// Claude Code 的 hook 把事件 POST 到这个本地端口（U3 监听 / U5 安装器写入）。
pub const STATE_PORT: u16 = 48756;

/// 跨线程共享状态：会话状态机 + 上次聚合状态(红灯进入检测) + 声音开关
/// + 位置锁定 + 是否已自动定位过(避免每次显示都把用户拖动的位置拽回去)。
pub struct Shared {
    pub store: Mutex<state::Store>,
    pub last_status: Mutex<String>,
    pub sound_enabled: AtomicBool,
    pub locked: AtomicBool,
    pub positioned: AtomicBool,
    pub manual_show: AtomicBool,
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

/// 托盘"锁定位置"勾选项句柄（放入 managed state，供命令同步勾选态）。
struct LockToggle(tauri::menu::CheckMenuItem<tauri::Wry>);

/// 前端右键灯调用：锁定/解锁。锁定 = 窗口点击穿透、不可选中/拖动。
#[tauri::command]
fn set_locked(
    app: AppHandle,
    shared: tauri::State<'_, Arc<Shared>>,
    lock_toggle: tauri::State<'_, LockToggle>,
    locked: bool,
) {
    shared.locked.store(locked, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("light") {
        let _ = win.set_ignore_cursor_events(locked);
    }
    let _ = lock_toggle.0.set_checked(locked);
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
        .invoke_handler(tauri::generate_handler![set_locked])
        .setup(|app| {
            use tauri_plugin_autostart::ManagerExt;

            // 共享状态：状态机 + 上次聚合状态(红灯进入检测) + 声音开关。
            let shared = Arc::new(Shared {
                store: Mutex::new(state::Store::default()),
                last_status: Mutex::new("none".to_string()),
                sound_enabled: AtomicBool::new(true),
                locked: AtomicBool::new(false),
                positioned: AtomicBool::new(false),
                manual_show: AtomicBool::new(false),
            });

            // 托盘菜单：提示音 / 开机自启(勾选) + 安装/卸载 hooks + 退出。
            let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
            let sound_item = CheckMenuItem::with_id(
                app,
                "toggle_sound",
                "提示音",
                true,
                shared.sound_enabled.load(Ordering::Relaxed),
                None::<&str>,
            )?;
            let autostart_item = CheckMenuItem::with_id(
                app,
                "toggle_autostart",
                "开机自启",
                true,
                autostart_on,
                None::<&str>,
            )?;
            // 锁定位置：默认不勾选(可拖动)；勾选后窗口点击穿透、不可选中。
            let lock_item =
                CheckMenuItem::with_id(app, "toggle_lock", "锁定位置", true, false, None::<&str>)?;
            let install =
                MenuItem::with_id(app, "install_hooks", "安装 hooks", true, None::<&str>)?;
            let uninstall =
                MenuItem::with_id(app, "uninstall_hooks", "卸载 hooks", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[
                    &sound_item,
                    &autostart_item,
                    &lock_item,
                    &sep1,
                    &install,
                    &uninstall,
                    &sep2,
                    &quit,
                ],
            )?;

            // 克隆给菜单回调，用于翻转开关并更新勾选态。
            let shared_menu = shared.clone();
            let sound_check = sound_item.clone();
            let autostart_check = autostart_item.clone();
            let lock_check = lock_item.clone();
            let shared_tray = shared.clone();

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("AI Work Traffic Light")
                .on_tray_icon_event(move |tray, event| {
                    // 左键托盘图标：手动显示/隐藏灯（空闲时也能召唤出来）。
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let next = !shared_tray.manual_show.load(Ordering::Relaxed);
                        shared_tray.manual_show.store(next, Ordering::Relaxed);
                        server::refresh(tray.app_handle(), &shared_tray);
                    }
                })
                .on_menu_event(move |app, event| {
                    let id = event.id();
                    if id == "toggle_sound" {
                        let next = !shared_menu.sound_enabled.load(Ordering::Relaxed);
                        shared_menu.sound_enabled.store(next, Ordering::Relaxed);
                        let _ = sound_check.set_checked(next);
                    } else if id == "toggle_autostart" {
                        let on = app.autolaunch().is_enabled().unwrap_or(false);
                        let _ = if on {
                            app.autolaunch().disable()
                        } else {
                            app.autolaunch().enable()
                        };
                        let _ = autostart_check.set_checked(!on);
                    } else if id == "toggle_lock" {
                        let next = !shared_menu.locked.load(Ordering::Relaxed);
                        shared_menu.locked.store(next, Ordering::Relaxed);
                        if let Some(win) = app.get_webview_window("light") {
                            // 锁定 = 点击穿透，不可选中/拖动。
                            let _ = win.set_ignore_cursor_events(next);
                        }
                        let _ = lock_check.set_checked(next);
                    } else if id == "install_hooks" {
                        notify_result(app, installer::install());
                    } else if id == "uninstall_hooks" {
                        notify_result(app, installer::uninstall());
                    } else if id == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            // 供前端 set_locked 命令使用的 managed state。
            app.manage(shared.clone());
            app.manage(LockToggle(lock_item.clone()));

            // 本地状态端点(U3) + 状态机(U4) + 红灯通知(U7)。
            server::start(app.handle().clone(), shared, STATE_PORT);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Work Traffic Light");
}
