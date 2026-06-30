// AI Work Traffic Light — Tauri 入口。
// 组装：透明置顶悬浮窗 + 托盘(提示音/自启开关、安装/卸载 hooks、退出)，
// 本地状态端点(U3) + 状态机(U4)，红灯通知(U7)，任务栏定位(U6)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod antigravity;
mod codex;
mod config;
// macOS dock 图标(提醒时闪红/黄灯)；其它平台没有 dock 概念，不编译。
#[cfg(target_os = "macos")]
mod dockicon;
mod installer;
mod platform;
mod server;
mod state;
mod trayicon;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

/// Claude Code 的 hook 把事件 POST 到这个本地端口（U3 监听 / U5 安装器写入）。
pub const STATE_PORT: u16 = 48756;

/// 红灯(一轮结束)在你切到对应窗口、常亮这么多秒后自动熄灭(隐藏)，直到下一轮。
const RED_AUTO_OFF_SECS: u64 = 2;

/// 跨线程共享状态：会话状态机 + 上次聚合状态(红灯进入检测) + 声音开关
/// + 位置锁定 + 是否已自动定位过(避免每次显示都把用户拖动的位置拽回去)
/// + 最近窗口位置(用于持久化)。
pub struct Shared {
    pub store: Mutex<state::Store>,
    pub last_status: Mutex<String>,
    pub sound_enabled: AtomicBool,
    pub locked: AtomicBool,
    pub vertical_layout: AtomicBool,
    pub positioned: AtomicBool,
    pub manual_show: AtomicBool,
    /// 最近一次窗口位置(物理像素)；窗口 Moved 时更新，定时器节流落盘到 config.json。
    pub last_pos: Mutex<Option<(i32, i32)>>,
    pub horizontal_size: Mutex<Option<(f64, f64)>>,
    pub vertical_size: Mutex<Option<(f64, f64)>>,
    /// 自定义提示音(.wav)路径：普通切换用 / 红灯用。可在启动时从配置读入，也可托盘里改。
    pub sound_file: Mutex<Option<String>>,
    pub sound_urgent_file: Mutex<Option<String>>,
    /// 红灯(一轮结束)被你切到窗口看过、常亮 RED_AUTO_OFF_SECS 秒后置位 -> 灯隐藏；
    /// 收到下一个 hook 事件即清零，重新显示。
    pub auto_off: AtomicBool,
    /// 极简模式：不显示桌面灯，只用托盘图标反映状态(按状态切换/闪烁托盘图标)。
    pub minimal: AtomicBool,
    /// 当前显示状态(working/idle/blocked/error/neutral/none) + 是否已查看(常亮)；
    /// 由 apply_effective 写入，供极简模式的托盘动画线程读取。
    pub cur_status: Mutex<String>,
    pub cur_ack: AtomicBool,
}

/// 把当前可持久化设置(提示音/锁定/位置/自定义音)写回 exe 同目录的 config.json。
fn persist(shared: &Shared) {
    config::save(&config::Config {
        sound_enabled: shared.sound_enabled.load(Ordering::Relaxed),
        locked: shared.locked.load(Ordering::Relaxed),
        vertical_layout: shared.vertical_layout.load(Ordering::Relaxed),
        pos: *shared.last_pos.lock().unwrap(),
        horizontal_size: *shared.horizontal_size.lock().unwrap(),
        vertical_size: *shared.vertical_size.lock().unwrap(),
        sound_file: shared.sound_file.lock().unwrap().clone(),
        sound_urgent_file: shared.sound_urgent_file.lock().unwrap().clone(),
        minimal: shared.minimal.load(Ordering::Relaxed),
    });
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

// ===== 「关于」对话框内容 —— 想改 app 信息(标题/版本/作者/说明)就改这里 =====
const ABOUT_TITLE: &str = "关于";
/// 版本号取自 Cargo.toml；其余文字直接在这里改。
fn about_body() -> String {
    format!(
        "AI 工作红绿灯 (AI Work Traffic Light)\n\
         版本 {ver}\n\
         \n\
         用红绿灯显示 Claude Code，codex，antigravity 的工作状态，需要你时及时提醒：\n\
         🟢 工作中　🟡 等你确认/选择　🔴 一轮结束(该你了)\n\
         \n\
         作者：alex\n\
         技术：Tauri (Rust) + Claude Code hooks",
        ver = env!("CARGO_PKG_VERSION"),
    )
}

/// 弹「关于」对话框（信息见 about_body）。
fn show_about(app: &AppHandle) {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .message(about_body())
        .title(ABOUT_TITLE)
        .show(|_| {});
}

/// 托盘"锁定位置"勾选项句柄（放入 managed state，供命令同步勾选态）。
struct LockToggle(tauri::menu::CheckMenuItem<tauri::Wry>);

const HORIZONTAL_SIZE: (f64, f64) = (99.0, 33.0);
const VERTICAL_SIZE: (f64, f64) = (62.0, 166.0);
const OLD_HORIZONTAL_SIZE: (f64, f64) = (205.0, 80.0);
const OLD_VERTICAL_SIZE: (f64, f64) = (80.0, 205.0);
const MIN_SIZE_SCALE: f64 = 0.6;
const MAX_SIZE_SCALE: f64 = 5.0;

#[derive(Serialize)]
struct LightWindowGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn size_tuple(vertical: bool) -> (f64, f64) {
    if vertical {
        VERTICAL_SIZE
    } else {
        HORIZONTAL_SIZE
    }
}

fn min_size_tuple(vertical: bool) -> (f64, f64) {
    let base = size_tuple(vertical);
    (base.0 * MIN_SIZE_SCALE, base.1 * MIN_SIZE_SCALE)
}

fn normalize_light_size(vertical: bool, size: (f64, f64)) -> (f64, f64) {
    let fallback = size_tuple(vertical);
    if !size.0.is_finite() || !size.1.is_finite() {
        return fallback;
    }

    let old_default = if vertical {
        OLD_VERTICAL_SIZE
    } else {
        OLD_HORIZONTAL_SIZE
    };
    if (size.0 - old_default.0).abs() < 1.0 && (size.1 - old_default.1).abs() < 1.0 {
        return fallback;
    }

    let scale = (size.0 / fallback.0)
        .min(size.1 / fallback.1)
        .clamp(MIN_SIZE_SCALE, MAX_SIZE_SCALE);
    (fallback.0 * scale, fallback.1 * scale)
}

fn stored_light_size(shared: &Shared, vertical: bool) -> Option<(f64, f64)> {
    if vertical {
        *shared.vertical_size.lock().unwrap()
    } else {
        *shared.horizontal_size.lock().unwrap()
    }
}

fn set_stored_light_size(shared: &Shared, vertical: bool, size: (f64, f64)) {
    let size = normalize_light_size(vertical, size);
    if vertical {
        *shared.vertical_size.lock().unwrap() = Some(size);
    } else {
        *shared.horizontal_size.lock().unwrap() = Some(size);
    }
}

fn current_light_size(shared: &Shared, vertical: bool) -> tauri::LogicalSize<f64> {
    let (width, height) = stored_light_size(shared, vertical)
        .map(|size| normalize_light_size(vertical, size))
        .unwrap_or_else(|| size_tuple(vertical));
    tauri::LogicalSize::new(width, height)
}

pub(crate) fn resize_light_window(
    win: &tauri::WebviewWindow<tauri::Wry>,
    shared: &Shared,
    vertical: bool,
) -> tauri::Result<()> {
    win.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize::new(
        min_size_tuple(vertical).0,
        min_size_tuple(vertical).1,
    ))))?;
    win.set_size(tauri::Size::Logical(current_light_size(shared, vertical)))
}

fn apply_lock_state(win: &tauri::WebviewWindow<tauri::Wry>, locked: bool) {
    let _ = win.set_ignore_cursor_events(locked);
    let _ = win.set_resizable(false);
}

fn apply_light_layout(app: &AppHandle, shared: &Shared, vertical: bool, keep_bottom: bool) {
    let previous = shared.vertical_layout.swap(vertical, Ordering::Relaxed);

    if let Some(win) = app.get_webview_window("light") {
        let old_size = win.outer_size().ok();
        let old_pos = win.outer_position().ok();
        let _ = resize_light_window(&win, shared, vertical);

        if keep_bottom && previous != vertical {
            if let (Some(old_size), Some(old_pos)) = (old_size, old_pos) {
                let scale_factor = win.scale_factor().unwrap_or(1.0);
                let new_size = current_light_size(shared, vertical).to_physical::<u32>(scale_factor);
                let y = (old_pos.y + old_size.height as i32 - new_size.height as i32).max(0);
                let pos = tauri::PhysicalPosition::new(old_pos.x, y);
                let _ = win.set_position(pos);
                *shared.last_pos.lock().unwrap() = Some((pos.x, pos.y));
            }
        }
    }

    let _ = app.emit("layout-changed", vertical);
}

#[tauri::command]
fn get_light_layout(shared: tauri::State<'_, Arc<Shared>>) -> bool {
    shared.vertical_layout.load(Ordering::Relaxed)
}

#[tauri::command]
fn get_locked(shared: tauri::State<'_, Arc<Shared>>) -> bool {
    shared.locked.load(Ordering::Relaxed)
}

#[tauri::command]
fn set_light_layout_size(
    app: AppHandle,
    shared: tauri::State<'_, Arc<Shared>>,
    vertical: bool,
) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("light") {
        resize_light_window(&win, &shared, vertical).map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_light_window_geometry(
    app: AppHandle,
    shared: tauri::State<'_, Arc<Shared>>,
) -> Result<LightWindowGeometry, String> {
    let Some(win) = app.get_webview_window("light") else {
        return Err("light window not found".to_string());
    };
    let scale_factor = win.scale_factor().map_err(|err| err.to_string())?;
    let pos = win.outer_position().map_err(|err| err.to_string())?;
    let size = win.outer_size().map_err(|err| err.to_string())?;
    let logical_size = size.to_logical::<f64>(scale_factor);
    let vertical = shared.vertical_layout.load(Ordering::Relaxed);
    let normalized = normalize_light_size(vertical, (logical_size.width, logical_size.height));

    Ok(LightWindowGeometry {
        x: pos.x as f64 / scale_factor,
        y: pos.y as f64 / scale_factor,
        width: normalized.0,
        height: normalized.1,
    })
}

#[tauri::command]
fn set_light_window_geometry(
    app: AppHandle,
    shared: tauri::State<'_, Arc<Shared>>,
    vertical: bool,
    width: f64,
    height: f64,
    x: f64,
    y: f64,
) -> Result<(), String> {
    let Some(win) = app.get_webview_window("light") else {
        return Err("light window not found".to_string());
    };
    let size = normalize_light_size(vertical, (width, height));
    set_stored_light_size(&shared, vertical, size);

    let scale_factor = win.scale_factor().unwrap_or(1.0);
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    let pos = tauri::LogicalPosition::new(x, y);
    let physical_pos = pos.to_physical::<i32>(scale_factor);

    win.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
        size.0, size.1,
    )))
    .map_err(|err| err.to_string())?;
    win.set_position(tauri::Position::Logical(pos))
        .map_err(|err| err.to_string())?;
    *shared.last_pos.lock().unwrap() = Some((physical_pos.x, physical_pos.y));

    Ok(())
}

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
        apply_lock_state(&win, locked);
    }
    let _ = lock_toggle.0.set_checked(locked);
    let _ = app.emit("locked-changed", locked);
    persist(&shared);
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
        .invoke_handler(tauri::generate_handler![
            set_locked,
            get_light_layout,
            get_locked,
            set_light_layout_size,
            get_light_window_geometry,
            set_light_window_geometry
        ])
        .setup(|app| {
            use tauri_plugin_autostart::ManagerExt;

            // 读取上次保存的设置(提示音/锁定/位置)；首次运行回落默认值。
            let cfg = config::load();
            // 首次运行(exe 同目录还没有 config.json)就建一个，方便用户看到/手改。
            config::ensure(&cfg);
            // 确保 audio/ 文件夹存在：自定义提示音放这里，config 里只记文件名。
            config::ensure_audio_dir();

            // 共享状态：状态机 + 上次聚合状态(红灯进入检测) + 声音/锁定(从配置恢复)。
            let shared = Arc::new(Shared {
                store: Mutex::new(state::Store::default()),
                last_status: Mutex::new("none".to_string()),
                sound_enabled: AtomicBool::new(cfg.sound_enabled),
                locked: AtomicBool::new(cfg.locked),
                vertical_layout: AtomicBool::new(cfg.vertical_layout),
                positioned: AtomicBool::new(false),
                // 启动即显示灯：manual_show 默认开，无会话时也以 neutral 灰态常驻。
                manual_show: AtomicBool::new(true),
                last_pos: Mutex::new(cfg.pos),
                horizontal_size: Mutex::new(cfg.horizontal_size),
                vertical_size: Mutex::new(cfg.vertical_size),
                sound_file: Mutex::new(cfg.sound_file.clone()),
                sound_urgent_file: Mutex::new(cfg.sound_urgent_file.clone()),
                auto_off: AtomicBool::new(false),
                minimal: AtomicBool::new(cfg.minimal),
                cur_status: Mutex::new("none".to_string()),
                cur_ack: AtomicBool::new(false),
            });

            // 托盘菜单：提示音(子菜单) / 开机自启(勾选) + 安装/卸载 hooks + 退出。
            let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
            // 「提示音」子菜单：启用开关 + 选择普通/红灯自定义音 + 恢复默认。
            let sound_item = CheckMenuItem::with_id(
                app,
                "toggle_sound",
                "启用提示音",
                true,
                shared.sound_enabled.load(Ordering::Relaxed),
                None::<&str>,
            )?;
            let pick_normal =
                MenuItem::with_id(app, "pick_sound", "选择提示音(普通)…", true, None::<&str>)?;
            let pick_urgent =
                MenuItem::with_id(app, "pick_sound_urgent", "选择提示音(红灯)…", true, None::<&str>)?;
            let reset_sound =
                MenuItem::with_id(app, "reset_sound", "恢复默认提示音", true, None::<&str>)?;
            let sound_menu = Submenu::with_items(
                app,
                "提示音",
                true,
                &[&sound_item, &pick_normal, &pick_urgent, &reset_sound],
            )?;
            let autostart_item = CheckMenuItem::with_id(
                app,
                "toggle_autostart",
                "开机自启",
                true,
                autostart_on,
                None::<&str>,
            )?;
            // 锁定位置：从配置恢复勾选态；勾选后窗口点击穿透、不可选中。
            let lock_item = CheckMenuItem::with_id(
                app,
                "toggle_lock",
                "锁定位置",
                true,
                shared.locked.load(Ordering::Relaxed),
                None::<&str>,
            )?;
            let vertical_item = CheckMenuItem::with_id(
                app,
                "toggle_vertical_layout",
                "竖向红绿灯",
                true,
                shared.vertical_layout.load(Ordering::Relaxed),
                None::<&str>,
            )?;
            let minimal_item = CheckMenuItem::with_id(
                app,
                "toggle_minimal",
                "极简模式(仅托盘图标)",
                true,
                shared.minimal.load(Ordering::Relaxed),
                None::<&str>,
            )?;
            let install =
                MenuItem::with_id(app, "install_hooks", "安装 hooks", true, None::<&str>)?;
            let uninstall =
                MenuItem::with_id(app, "uninstall_hooks", "卸载 hooks", true, None::<&str>)?;
            let about = MenuItem::with_id(app, "about", "关于", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let sep3 = PredefinedMenuItem::separator(app)?;
            let sep4 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[
                    &sound_menu,
                    &autostart_item,
                    &lock_item,
                    &vertical_item,
                    &minimal_item,
                    &sep1,
                    &install,
                    &uninstall,
                    &sep2,
                    &about,
                    &sep3,
                    &quit,
                    &sep4,
                ],
            )?;

            // 克隆给菜单回调，用于翻转开关并更新勾选态。
            let shared_menu = shared.clone();
            let sound_check = sound_item.clone();
            let autostart_check = autostart_item.clone();
            let lock_check = lock_item.clone();
            let vertical_check = vertical_item.clone();
            let minimal_check = minimal_item.clone();
            let shared_tray = shared.clone();

            let _tray = TrayIconBuilder::with_id("main")
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
                        // 用户主动召唤 -> 清掉自动灭标记，确保能显示。
                        shared_tray.auto_off.store(false, Ordering::Relaxed);
                        server::refresh(tray.app_handle(), &shared_tray);
                    }
                })
                .on_menu_event(move |app, event| {
                    let id = event.id();
                    if id == "toggle_sound" {
                        let next = !shared_menu.sound_enabled.load(Ordering::Relaxed);
                        shared_menu.sound_enabled.store(next, Ordering::Relaxed);
                        let _ = sound_check.set_checked(next);
                        persist(&shared_menu);
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
                            apply_lock_state(&win, next);
                        }
                        let _ = lock_check.set_checked(next);
                        let _ = app.emit("locked-changed", next);
                        persist(&shared_menu);
                    } else if id == "toggle_vertical_layout" {
                        let next = !shared_menu.vertical_layout.load(Ordering::Relaxed);
                        apply_light_layout(app, &shared_menu, next, true);
                        let _ = vertical_check.set_checked(next);
                        persist(&shared_menu);
                    } else if id == "toggle_minimal" {
                        let next = !shared_menu.minimal.load(Ordering::Relaxed);
                        shared_menu.minimal.store(next, Ordering::Relaxed);
                        let _ = minimal_check.set_checked(next);
                        // 开极简：立即隐藏桌面灯；关极简：刷新一次重新显示。
                        // 托盘图标由动画线程按 minimal/cur_status 自动切换。
                        if next {
                            if let Some(win) = app.get_webview_window("light") {
                                let _ = win.hide();
                            }
                        } else {
                            server::refresh(app, &shared_menu);
                        }
                        persist(&shared_menu);
                    } else if id == "pick_sound" || id == "pick_sound_urgent" {
                        // 弹文件框选 .wav；选完写入配置并试听一次。
                        use tauri_plugin_dialog::DialogExt;
                        let urgent = id == "pick_sound_urgent";
                        let sh = shared_menu.clone();
                        app.dialog()
                            .file()
                            .add_filter("音频 (*.wav)", &["wav"])
                            .set_title(if urgent {
                                "选择红灯提示音 (.wav)"
                            } else {
                                "选择普通提示音 (.wav)"
                            })
                            .pick_file(move |path| {
                                let Some(fp) = path else { return };
                                let Ok(pb) = fp.into_path() else { return };
                                // 复制进 audio/，config 里只存文件名 -> 整个程序目录可整体搬走。
                                let Some(name) = config::import_sound(&pb) else {
                                    return;
                                };
                                if urgent {
                                    *sh.sound_urgent_file.lock().unwrap() = Some(name.clone());
                                } else {
                                    *sh.sound_file.lock().unwrap() = Some(name.clone());
                                }
                                persist(&sh);
                                server::preview_sound(&name); // 试听
                            });
                    } else if id == "reset_sound" {
                        *shared_menu.sound_file.lock().unwrap() = None;
                        *shared_menu.sound_urgent_file.lock().unwrap() = None;
                        persist(&shared_menu);
                    } else if id == "install_hooks" {
                        notify_result(app, installer::install());
                    } else if id == "uninstall_hooks" {
                        notify_result(app, installer::uninstall());
                    } else if id == "quit" {
                        app.exit(0);
                    } else if id == "about" {
                        show_about(app);
                    }
                })
                .build(app)?;

            // 供前端 set_locked 命令使用的 managed state。
            app.manage(shared.clone());
            app.manage(LockToggle(lock_item.clone()));

            apply_light_layout(app.handle(), &shared, cfg.vertical_layout, false);

            // 启动时：应用恢复的锁定态、还原上次窗口位置，并监听拖动以记录新位置。
            if let Some(win) = app.get_webview_window("light") {
                apply_lock_state(&win, shared.locked.load(Ordering::Relaxed));
                if let Some((x, y)) = cfg.pos {
                    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                    // 已有保存位置 -> 别再自动贴任务栏，尊重用户上次拖到的位置。
                    shared.positioned.store(true, Ordering::Relaxed);
                }
                let shared_move = shared.clone();
                let win_resize = win.clone();
                win.on_window_event(move |event| {
                    match event {
                        tauri::WindowEvent::Moved(pos) => {
                            *shared_move.last_pos.lock().unwrap() = Some((pos.x, pos.y));
                        }
                        tauri::WindowEvent::Resized(size) => {
                            let scale_factor = win_resize.scale_factor().unwrap_or(1.0);
                            let logical = size.to_logical::<f64>(scale_factor);
                            let vertical = shared_move.vertical_layout.load(Ordering::Relaxed);
                            set_stored_light_size(
                                &shared_move,
                                vertical,
                                (logical.width, logical.height),
                            );
                        }
                        _ => {}
                    }
                });

                // 关掉 WebView2 默认右键菜单(返回/刷新/打印…)：引擎级，打包版同样生效。
                #[cfg(windows)]
                {
                    let _ = win.with_webview(|webview| unsafe {
                        let controller = webview.controller();
                        if let Ok(core) = controller.CoreWebView2() {
                            if let Ok(settings) = core.Settings() {
                                let _ = settings.SetAreDefaultContextMenusEnabled(false);
                            }
                        }
                    });
                }
            }

            // 启动即显示灯：刷新一次把窗口摆到位并显示（无会话时为 neutral 灰态）。
            // 前端会在此时播放"红→黄→绿依次亮一下"的开场动画。
            server::refresh(app.handle(), &shared);

            // 置顶/焦点检测线程要用的克隆（下面 server::start 会拿走 shared 本体）。
            let shared_timer = shared.clone();
            // 极简模式托盘动画线程要用的克隆。
            let shared_tray_anim = shared.clone();
            // 极简模式 neutral 用应用自带的红绿灯图标(转成 owned，可跨线程长期持有)。
            let default_tray_icon = {
                let di = app.default_window_icon().unwrap();
                tauri::image::Image::new_owned(di.rgba().to_vec(), di.width(), di.height())
            };

            // 本地状态端点(U3) + 状态机(U4) + 红灯通知(U7)。
            server::start(app.handle().clone(), shared, STATE_PORT);

            // 后台维护线程（全平台）。每 ~0.8s：
            //   1) 仅 Windows：把灯顶回最前（点任务栏会把任务栏抢到置顶最前、盖住灯）；
            //   2) 聚焦常亮：前台是当前催你来源对应的窗口则常亮，否则闪；
            //   3) 窗口位置变化则节流落盘到 config.json。
            {
                let app_handle = app.handle().clone();
                let mut saved_pos = *shared_timer.last_pos.lock().unwrap();
                let mut saved_horizontal_size = *shared_timer.horizontal_size.lock().unwrap();
                let mut saved_vertical_size = *shared_timer.vertical_size.lock().unwrap();
                let mut last_ack: Option<bool> = None; // None = 还没通知过前端
                let mut red_focus_since: Option<std::time::Instant> = None; // 红灯被聚焦的起点
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(800));

                    #[cfg(windows)]
                    if let Some(win) = app_handle.get_webview_window("light") {
                        if win.is_visible().unwrap_or(false) {
                            platform::reassert_topmost(&win);
                        }
                    }

                    // "精确到窗口"的停闪：取当前在催你的来源(claude/codex)，
                    // 只有前台是该来源对应的窗口才算已查看(常亮 is-ack)，否则继续闪。
                    let agg = shared_timer.store.lock().unwrap().aggregate();
                    let ack = platform::foreground_matches_window(&agg.source, &agg.session_label);
                    // 供极简模式托盘动画用的实时焦点(apply_effective 只在状态变化时写，会过时 ->
                    // 托盘红灯切走后不闪。这里每拍刷新，让托盘按实时焦点闪/常亮)。
                    shared_timer.cur_ack.store(ack, Ordering::Relaxed);
                    if last_ack != Some(ack) {
                        last_ack = Some(ack);
                        // 开发模式打印：前台进程名 + 标题 + 来源 + 项目 + 是否已查看（排查多窗口停闪）。
                        #[cfg(debug_assertions)]
                        eprintln!(
                            "[traffic-light] 焦点变化: 前台={:?} 标题={:?} 来源={:?} 项目={:?} 已查看={}",
                            platform::foreground_token(),
                            platform::foreground_title(),
                            agg.source,
                            agg.session_label,
                            ack
                        );
                        let _ = app_handle.emit("focus-changed", ack);
                    }

                    // 红灯(一轮结束)自动灭：你切到对应窗口(ack)、红灯常亮 RED_AUTO_OFF_SECS 秒后
                    // 隐藏灯；没切过去就一直闪不熄。收到下一个 hook 事件会清零 auto_off 重新显示。
                    if agg.status == "blocked" && ack {
                        match red_focus_since {
                            None => red_focus_since = Some(std::time::Instant::now()),
                            Some(t) => {
                                if t.elapsed() >= std::time::Duration::from_secs(RED_AUTO_OFF_SECS)
                                    && !shared_timer.auto_off.load(Ordering::Relaxed)
                                {
                                    shared_timer.auto_off.store(true, Ordering::Relaxed);
                                    // 灯灭=只剩外壳不亮灯(neutral)：刷新一次让 apply_effective 切到 neutral。
                                    server::refresh(&app_handle, &shared_timer);
                                    red_focus_since = None;
                                }
                            }
                        }
                    } else {
                        red_focus_since = None;
                    }

                    let cur_pos = *shared_timer.last_pos.lock().unwrap();
                    let cur_horizontal_size = *shared_timer.horizontal_size.lock().unwrap();
                    let cur_vertical_size = *shared_timer.vertical_size.lock().unwrap();
                    if cur_pos != saved_pos
                        || cur_horizontal_size != saved_horizontal_size
                        || cur_vertical_size != saved_vertical_size
                    {
                        saved_pos = cur_pos;
                        saved_horizontal_size = cur_horizontal_size;
                        saved_vertical_size = cur_vertical_size;
                        persist(&shared_timer);
                    }
                });
            }

            // 极简模式：托盘图标动画线程。每 ~450ms 按当前状态刷新托盘图标：
            //   working->绿(常亮) / idle|error->黄 / blocked->红；红黄未查看时闪(亮↔暗交替)；
            //   neutral/none -> 应用自带的红绿灯图标。非极简模式则保持默认应用图标。
            // macOS 额外：把同一状态投到 dock 图标(见下方 #[cfg(target_os = "macos")] 块)——
            //   极简模式 dock 镜像托盘(全状态)；普通模式 dock 只在提醒(红/黄)时闪、其它恢复默认图标。
            {
                let app_handle = app.handle().clone();
                let shared = shared_tray_anim;
                let default_icon = default_tray_icon;
                let mut phase = false;
                let mut last_tray_key = String::new();
                // dock 图标上次设置的 key(去重，避免每拍重复设置)；仅 macOS 用。
                #[cfg(target_os = "macos")]
                let mut last_dock_key = String::new();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(450));
                    phase = !phase;
                    let minimal = shared.minimal.load(Ordering::Relaxed);
                    let status = shared.cur_status.lock().unwrap().clone();
                    // 状态 -> 点亮哪个灯 + 是否闪。红/黄闪(亮↔暗交替)；绿常亮；其它全灭(只灯框)。
                    // 不做焦点门控(否则你在工作窗口时看不到闪)；停闪靠 auto_off：切到对应窗口
                    // 看过几秒 -> 状态变 neutral -> 自动停。
                    let (active, blink) = match status.as_str() {
                        "working" => (Some(trayicon::Lamp::Green), false),
                        "idle" | "error" => (Some(trayicon::Lamp::Yellow), true),
                        "blocked" => (Some(trayicon::Lamp::Red), true),
                        _ => (None, false), // neutral/none -> 全灭红绿灯
                    };
                    // 闪烁态把 phase 编进 lit -> 每拍交替亮灭；常亮/neutral 恒亮。
                    let lit = if blink { phase } else { true };

                    // ---- 托盘图标：仅极简模式按状态切换/闪烁；非极简保持默认应用图标 ----
                    if let Some(tray) = app_handle.tray_by_id("main") {
                        if !minimal {
                            if last_tray_key != "default" {
                                let _ = tray.set_icon(Some(default_icon.clone()));
                                last_tray_key = "default".to_string();
                            }
                        } else {
                            // 极简模式：托盘显示「灯框+3灯」的完整红绿灯
                            // (mac 菜单栏 / Linux 指示器 / Windows 托盘)。
                            let key = format!("{status}-{lit}");
                            if key != last_tray_key {
                                last_tray_key = key;
                                let _ = tray.set_icon(Some(trayicon::traffic_light_image(active, lit)));
                            }
                        }
                    }

                    // ---- macOS dock 图标 ----
                    // 极简模式：dock 镜像托盘(全状态都画灯框+灯，neutral=只灯框)，和桌面灯功能一致；
                    // 普通模式：dock 只在提醒(idle/error/blocked = 黄/红)时显示并闪，其它恢复默认 dock 图标。
                    // setApplicationIconImage: 是 AppKit UI 调用 -> 必须主线程，故 run_on_main_thread。
                    #[cfg(target_os = "macos")]
                    {
                        let dock_show =
                            minimal || matches!(status.as_str(), "idle" | "error" | "blocked");
                        let dock_key = if dock_show {
                            format!("{status}-{lit}")
                        } else {
                            "default".to_string()
                        };
                        if dock_key != last_dock_key {
                            last_dock_key = dock_key;
                            // PNG 编码在本线程做(轻量)；objc 设置图标投到主线程。
                            let payload = if dock_show {
                                dockicon::encode_png(&trayicon::traffic_light_image(active, lit))
                            } else {
                                None // 恢复默认 dock 图标
                            };
                            let _ = app_handle
                                .run_on_main_thread(move || dockicon::set_dock_image(payload));
                        }
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Work Traffic Light");
}
