// U3: 本地状态接入端点。U7: 红灯进入时的系统通知 + 可选提示音。
//
// 在 127.0.0.1:<port> 起极简 HTTP 服务，hook 把事件 POST 到 /event/<EventName>，
// body 是 hook 原始 JSON 负载。收到后：更新状态机(U4) -> 算聚合 -> 检测是否
// "刚进入红灯" -> 通知/提示音(U7) -> 推 state-changed 给前端 -> 显隐窗口。
// 立即回 204，让 hook 端 fire-and-forget。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::state::Aggregate;
use crate::Shared;

pub fn start(app: AppHandle, shared: Arc<Shared>, port: u16) {
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[traffic-light] 监听 127.0.0.1:{port} 失败: {e}");
                return;
            }
        };

        for mut req in server.incoming_requests() {
            let event = req.url().strip_prefix("/event/").map(|s| s.to_string());

            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let _ = req.respond(tiny_http::Response::empty(204));

            let Some(event) = event else { continue };
            let (session_id, cwd) = parse_payload(&body);

            let agg = {
                let mut store = shared.store.lock().unwrap();
                store.apply(&event, &session_id, cwd.as_deref());
                store.aggregate()
            };

            // 检测状态变化：进入红灯弹通知；任意状态切换(若开启)播放声音。
            let (changed, entered_blocked) = {
                let mut last = shared.last_status.lock().unwrap();
                let changed = *last != agg.status;
                let entered = agg.status == "blocked" && *last != "blocked";
                *last = agg.status.clone();
                (changed, entered)
            };
            if entered_blocked {
                notify_blocked(&app, &agg.session_label);
            }
            if changed && shared.sound_enabled.load(Ordering::Relaxed) {
                play_sound(entered_blocked);
            }

            apply_effective(&app, &shared, agg);
        }
    });
}

/// 重新计算并应用当前应显示的状态（供托盘左键手动显隐时调用）。
pub fn refresh(app: &AppHandle, shared: &Shared) {
    let agg = shared.store.lock().unwrap().aggregate();
    apply_effective(app, shared, agg);
}

/// 把"真实聚合 + 手动显示"折算成最终显示，并显隐窗口。
/// 无会话(none)时：手动显示 -> 灰色中性灯；否则隐藏。
fn apply_effective(app: &AppHandle, shared: &Shared, real: Aggregate) {
    let effective = if real.status != "none" {
        real
    } else if shared.manual_show.load(Ordering::Relaxed) {
        Aggregate {
            status: "neutral".to_string(),
            session_label: String::new(),
        }
    } else {
        real
    };

    let _ = app.emit("state-changed", &effective);
    if let Some(win) = app.get_webview_window("light") {
        if effective.status == "none" {
            let _ = win.hide();
        } else {
            // 仅首次显示时自动定位到任务栏；之后保留用户拖动后的位置。
            #[cfg(windows)]
            if !shared.positioned.swap(true, Ordering::Relaxed) {
                crate::taskbar::position_over_taskbar(&win);
            }
            let _ = win.show();
        }
    }
}

/// 红灯：弹系统通知(含会话标识)。
fn notify_blocked(app: &AppHandle, label: &str) {
    let body = if label.is_empty() {
        "有 Claude 会话在等待你的确认".to_string()
    } else {
        format!("「{label}」在等待你的确认")
    };
    let _ = app
        .notification()
        .builder()
        .title("Claude 需要你")
        .body(body)
        .show();
}

/// 状态切换时播放系统提示音；红灯用更显眼的警告音，其它用提示音。
#[cfg(windows)]
fn play_sound(urgent: bool) {
    use windows::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONASTERISK, MB_ICONWARNING};
    unsafe {
        let _ = MessageBeep(if urgent { MB_ICONWARNING } else { MB_ICONASTERISK });
    }
}
#[cfg(not(windows))]
fn play_sound(_urgent: bool) {}

/// 从 hook JSON 负载里取 session_id 与 cwd。
fn parse_payload(body: &str) -> (String, Option<String>) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let session_id = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let cwd = v.get("cwd").and_then(|x| x.as_str()).map(|s| s.to_string());
    (session_id, cwd)
}
