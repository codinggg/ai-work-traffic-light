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

            // 只在"刚进入红灯"的瞬间提醒，避免每个红事件都弹。
            let entered_blocked = {
                let mut last = shared.last_status.lock().unwrap();
                let entered = agg.status == "blocked" && *last != "blocked";
                *last = agg.status.clone();
                entered
            };
            if entered_blocked {
                notify_blocked(
                    &app,
                    &agg.session_label,
                    shared.sound_enabled.load(Ordering::Relaxed),
                );
            }

            let _ = app.emit("state-changed", &agg);
            if let Some(win) = app.get_webview_window("light") {
                let _ = if agg.status == "none" {
                    win.hide()
                } else {
                    win.show()
                };
            }
        }
    });
}

/// 红灯：弹系统通知(含会话标识) + 可选提示音。
fn notify_blocked(app: &AppHandle, label: &str, sound: bool) {
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

    if sound {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Diagnostics::Debug::MessageBeep;
            use windows::Win32::UI::WindowsAndMessaging::MB_ICONWARNING;
            let _ = MessageBeep(MB_ICONWARNING);
        }
    }
}

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
