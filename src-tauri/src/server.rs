// U3: 本地状态接入端点。
//
// 在 127.0.0.1:<port> 起一个极简 HTTP 服务，Claude Code 的 hook 把事件
// POST 到 /event/<EventName>，body 是 hook 的原始 JSON 负载。事件名放在
// URL 路径里(我们在 U5 安装器里控制)，比依赖负载里的 hook_event_name 更稳。
//
// 收到后：更新状态机(U4) -> 算聚合 -> 推 `state-changed` 给前端 -> 显隐窗口。
// 立即回 204，让 hook 端 fire-and-forget、不拖慢 Claude。

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager};

use crate::state::Store;

pub fn start(app: AppHandle, store: Arc<Mutex<Store>>, port: u16) {
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[traffic-light] 监听 127.0.0.1:{port} 失败: {e}");
                return;
            }
        };

        for mut req in server.incoming_requests() {
            let event = req
                .url()
                .strip_prefix("/event/")
                .map(|s| s.to_string());

            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            // 先快速回应，hook 端不必等待。
            let _ = req.respond(tiny_http::Response::empty(204));

            let Some(event) = event else { continue };
            let (session_id, cwd) = parse_payload(&body);

            let agg = {
                let mut store = store.lock().unwrap();
                store.apply(&event, &session_id, cwd.as_deref());
                store.aggregate()
            };

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
