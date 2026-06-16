// AI Work Traffic Light — Tauri 入口。
// 组装：透明置顶悬浮窗 + 托盘(提示音/自启开关、安装/卸载 hooks、退出)，
// 本地状态端点(U3) + 状态机(U4)，红灯通知(U7)，任务栏定位(U6)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod installer;
mod server;
mod state;
#[cfg(windows)]
mod taskbar;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

/// Claude Code 的 hook 把事件 POST 到这个本地端口（U3 监听 / U5 安装器写入）。
pub const STATE_PORT: u16 = 48756;

/// 跨线程共享状态：会话状态机 + 上次聚合状态(红灯进入检测) + 声音开关
/// + 位置锁定 + 是否已自动定位过(避免每次显示都把用户拖动的位置拽回去)
/// + 最近窗口位置(用于持久化)。
pub struct Shared {
    pub store: Mutex<state::Store>,
    pub last_status: Mutex<String>,
    pub sound_enabled: AtomicBool,
    pub locked: AtomicBool,
    pub positioned: AtomicBool,
    pub manual_show: AtomicBool,
    /// 最近一次窗口位置(物理像素)；窗口 Moved 时更新，定时器节流落盘到 config.json。
    pub last_pos: Mutex<Option<(i32, i32)>>,
    /// 自定义提示音(.wav)路径：普通切换用 / 红灯用。可在启动时从配置读入，也可托盘里改。
    pub sound_file: Mutex<Option<String>>,
    pub sound_urgent_file: Mutex<Option<String>>,
}

/// 把当前可持久化设置(提示音/锁定/位置/自定义音)写回 exe 同目录的 config.json。
fn persist(shared: &Shared) {
    config::save(&config::Config {
        sound_enabled: shared.sound_enabled.load(Ordering::Relaxed),
        locked: shared.locked.load(Ordering::Relaxed),
        pos: *shared.last_pos.lock().unwrap(),
        sound_file: shared.sound_file.lock().unwrap().clone(),
        sound_urgent_file: shared.sound_urgent_file.lock().unwrap().clone(),
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
        .invoke_handler(tauri::generate_handler![set_locked])
        .setup(|app| {
            use tauri_plugin_autostart::ManagerExt;

            // 读取上次保存的设置(提示音/锁定/位置)；首次运行回落默认值。
            let cfg = config::load();
            // 首次运行(exe 同目录还没有 config.json)就建一个，方便用户看到/手改。
            config::ensure(&cfg);

            // 共享状态：状态机 + 上次聚合状态(红灯进入检测) + 声音/锁定(从配置恢复)。
            let shared = Arc::new(Shared {
                store: Mutex::new(state::Store::default()),
                last_status: Mutex::new("none".to_string()),
                sound_enabled: AtomicBool::new(cfg.sound_enabled),
                locked: AtomicBool::new(cfg.locked),
                positioned: AtomicBool::new(false),
                // 启动即显示灯：manual_show 默认开，无会话时也以 neutral 灰态常驻。
                manual_show: AtomicBool::new(true),
                last_pos: Mutex::new(cfg.pos),
                sound_file: Mutex::new(cfg.sound_file.clone()),
                sound_urgent_file: Mutex::new(cfg.sound_urgent_file.clone()),
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
                    &sound_menu,
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
                            let _ = win.set_ignore_cursor_events(next);
                        }
                        let _ = lock_check.set_checked(next);
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
                                let s = pb.to_string_lossy().to_string();
                                if urgent {
                                    *sh.sound_urgent_file.lock().unwrap() = Some(s.clone());
                                } else {
                                    *sh.sound_file.lock().unwrap() = Some(s.clone());
                                }
                                persist(&sh);
                                server::preview_sound(&s); // 试听
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
                    }
                })
                .build(app)?;

            // 供前端 set_locked 命令使用的 managed state。
            app.manage(shared.clone());
            app.manage(LockToggle(lock_item.clone()));

            // 启动时：应用恢复的锁定态、还原上次窗口位置，并监听拖动以记录新位置。
            if let Some(win) = app.get_webview_window("light") {
                if shared.locked.load(Ordering::Relaxed) {
                    let _ = win.set_ignore_cursor_events(true);
                }
                if let Some((x, y)) = cfg.pos {
                    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                    // 已有保存位置 -> 别再自动贴任务栏，尊重用户上次拖到的位置。
                    shared.positioned.store(true, Ordering::Relaxed);
                }
                let shared_move = shared.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::Moved(pos) = event {
                        *shared_move.last_pos.lock().unwrap() = Some((pos.x, pos.y));
                    }
                });
            }

            // 启动即显示灯：刷新一次把窗口摆到位并显示（无会话时为 neutral 灰态）。
            // 前端会在此时播放"红→黄→绿依次亮一下"的开场动画。
            server::refresh(app.handle(), &shared);

            // 置顶/焦点检测线程要用的克隆（下面 server::start 会拿走 shared 本体）。
            #[cfg(windows)]
            let shared_timer = shared.clone();

            // 本地状态端点(U3) + 状态机(U4) + 红灯通知(U7)。
            server::start(app.handle().clone(), shared, STATE_PORT);

            // 后台维护线程（仅 Windows）。每 ~0.8s：
            //   1) 把灯顶回最前——点任务栏会把任务栏抢到置顶最前、盖住灯；
            //   2) 检测前台是否「工作窗口」——是则通知前端灯常亮，否则闪烁；
            //   3) 窗口位置变化则节流落盘到 config.json。
            #[cfg(windows)]
            {
                let app_handle = app.handle().clone();
                let mut saved_pos = *shared_timer.last_pos.lock().unwrap();
                let mut last_at_work: Option<bool> = None; // None = 还没通知过前端
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(800));

                    if let Some(win) = app_handle.get_webview_window("light") {
                        if win.is_visible().unwrap_or(false) {
                            taskbar::reassert_topmost(&win);
                        }
                    }

                    let at_work = taskbar::foreground_is_work_window();
                    if last_at_work != Some(at_work) {
                        last_at_work = Some(at_work);
                        let _ = app_handle.emit("focus-changed", at_work);
                    }

                    let cur = *shared_timer.last_pos.lock().unwrap();
                    if cur != saved_pos {
                        saved_pos = cur;
                        persist(&shared_timer);
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Work Traffic Light");
}
