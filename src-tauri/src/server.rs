// U3: 本地状态接入端点。U7: 红灯进入时的系统通知 + 可选提示音。
//
// 在 127.0.0.1:<port> 起极简 HTTP 服务，hook 把事件 POST 到 /event/<EventName>，
// body 是 hook 原始 JSON 负载。收到后：更新状态机(U4) -> 算聚合 -> 检测是否
// "刚进入红灯" -> 通知/提示音(U7) -> 推 state-changed 给前端 -> 显隐窗口。
// 立即回 204，让 hook 端 fire-and-forget。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::Aggregate;
use crate::Shared;

/// Codex 会话(没有 SessionEnd 事件)的黄灯保活时长：超过这么久没有新一轮事件就自动消隐，
/// 避免「该你了」黄灯永久卡住。想调就改这里。
const CODEX_IDLE_TIMEOUT_SECS: u64 = 600;

/// Antigravity 会话闲置超时（10 分钟）清理
const ANTIGRAVITY_IDLE_TIMEOUT_SECS: u64 = 600;

pub fn start(app: AppHandle, shared: Arc<Shared>, port: u16) {
    // transcript 轮询线程：hooks 不会在 API 报错(如 429)时触发，但错误会写进会话
    // transcript。每 ~1.5s 读各活跃会话 transcript 的新增内容，发现 API 错误就把该
    // 会话标记为 error(黄灯)；同时监视 Codex 会话日志(working/idle)，并清理过期的
    // Codex 会话(自动消隐)。任一有变化就刷新显示。
    {
        let app = app.clone();
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut codex = crate::codex::CodexWatcher::new();
            let mut antigravity = crate::antigravity::AntigravityWatcher::new();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let (changed, activity) = {
                    let mut store = shared.store.lock().unwrap();
                    let err = store.scan_api_errors();
                    let cdx = codex.poll(&mut store);
                    let anti = antigravity.poll(&mut store);
                    let expired = store.expire(
                        "codex:",
                        std::time::Duration::from_secs(CODEX_IDLE_TIMEOUT_SECS),
                    ) || store.expire(
                        "antigravity:",
                        std::time::Duration::from_secs(ANTIGRAVITY_IDLE_TIMEOUT_SECS),
                    );
                    (err || cdx || anti || expired, err || cdx || anti)
                };
                // 新活动(Codex/Antigravity 新一轮、或 API 错误) -> 取消"已看过"的中性态(auto_off)，
                // 让这条新状态(如 Codex 一轮结束的红灯)显示出来。expire(仅自动消隐)不算新活动。
                if activity {
                    shared.auto_off.store(false, Ordering::Relaxed);
                }
                if changed {
                    let agg = shared.store.lock().unwrap().aggregate();
                    apply_effective(&app, &shared, agg);
                }
            }
        });
    }

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
            let (session_id, cwd, transcript) = parse_payload(&body);
            // 新的 hook 事件 = 新活动：清掉"红灯已自动灭"标记，让灯重新显示。
            shared.auto_off.store(false, Ordering::Relaxed);

            let agg = {
                let mut store = shared.store.lock().unwrap();
                store.apply(&event, &session_id, cwd.as_deref());
                if let Some(t) = &transcript {
                    store.set_transcript(&session_id, t);
                }
                store.aggregate()
            };

            // 诊断：把收到的事件与结果状态追加到 events.log（排查灯色问题；问题定位后可移除）。
            log_event(&event, &session_id, &agg.status);

            // 检测状态变化：任意状态切换(若开启)播放一次提示音。红灯(一轮结束)用"红灯提示音"
            // (更醒目)，其它用普通音；都不弹系统通知，只靠灯闪烁+声音提醒。
            let changed = {
                let mut last = shared.last_status.lock().unwrap();
                let c = *last != agg.status;
                *last = agg.status.clone();
                c
            };
            if changed && shared.sound_enabled.load(Ordering::Relaxed) {
                let is_red = agg.status == "blocked";
                let custom = if is_red {
                    shared.sound_urgent_file.lock().unwrap().clone()
                } else {
                    shared.sound_file.lock().unwrap().clone()
                };
                play_sound(is_red, custom.as_deref());
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
    let effective = if shared.auto_off.load(Ordering::Relaxed) {
        // 红灯已被你切到窗口看过并自动灭：只显示外壳、三个灯都不亮(neutral)，且不闪；
        // 一直保持到下一个 hook 事件清零 auto_off。这样切走也不会再闪(只闪到你看过为止)。
        Aggregate {
            status: "neutral".to_string(),
            session_label: String::new(),
            source: String::new(),
        }
    } else if real.status != "none" {
        real
    } else if shared.manual_show.load(Ordering::Relaxed) {
        Aggregate {
            status: "neutral".to_string(),
            session_label: String::new(),
            source: String::new(),
        }
    } else {
        real
    };

    let _ = app.emit("state-changed", &effective);
    // 同步推一次"是否已查看(常亮)"：只有切到当前催你来源对应的窗口才算已查看。
    // 这里立刻算一次消除状态变化时的闪烁延迟；窗口切换则由 main.rs 定时器负责。
    let ack = crate::platform::foreground_matches_window(&effective.source, &effective.session_label);
    let _ = app.emit("focus-changed", ack);
    if let Some(win) = app.get_webview_window("light") {
        if effective.status == "none" {
            let _ = win.hide();
        } else {
            let vertical = shared
                .vertical_layout
                .load(std::sync::atomic::Ordering::Relaxed);
            let _ = crate::resize_light_window(&win, shared, vertical);
            // 仅首次显示时自动定位；之后保留用户拖动后的位置。
            if !shared.positioned.swap(true, Ordering::Relaxed) {
                crate::platform::place_window(&win);
            }
            let _ = win.show();
        }
    }
}

/// 状态切换时播放提示音。
/// 优先放自定义音：把 config 值(audio/ 下文件名或绝对路径)解析成真实文件再放；
/// 否则回落到系统提示音：红灯用更显眼的警告音，其它用普通提示音。
#[cfg(windows)]
fn play_sound(urgent: bool, custom: Option<&str>) {
    if let Some(value) = custom {
        if let Some(path) = crate::config::resolve_sound(value) {
            use windows::core::HSTRING;
            use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
            let wide = HSTRING::from(path.to_string_lossy().as_ref());
            unsafe {
                let _ = PlaySoundW(&wide, None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
            }
            return;
        }
    }
    use windows::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONASTERISK, MB_ICONWARNING};
    unsafe {
        let _ = MessageBeep(if urgent { MB_ICONWARNING } else { MB_ICONASTERISK });
    }
}

/// macOS：自定义音用 afplay 放文件；默认放系统音（红灯更显眼）。
#[cfg(target_os = "macos")]
fn play_sound(urgent: bool, custom: Option<&str>) {
    use std::process::Command;
    if let Some(value) = custom {
        if let Some(path) = crate::config::resolve_sound(value) {
            let _ = Command::new("afplay").arg(path).spawn();
            return;
        }
    }
    let sys = if urgent {
        "/System/Library/Sounds/Sosumi.aiff"
    } else {
        "/System/Library/Sounds/Funk.aiff"
    };
    let _ = Command::new("afplay").arg(sys).spawn();
}

/// Linux：自定义 .wav 依次试常见播放器；默认放 freedesktop 主题音，兜底响终端铃。
#[cfg(target_os = "linux")]
fn play_sound(urgent: bool, custom: Option<&str>) {
    use std::process::Command;
    if let Some(value) = custom {
        if let Some(path) = crate::config::resolve_sound(value) {
            let p = path.to_string_lossy().to_string();
            for (cmd, args) in [
                ("paplay", vec![p.as_str()]),
                ("aplay", vec![p.as_str()]),
                ("ffplay", vec!["-nodisp", "-autoexit", "-loglevel", "quiet", p.as_str()]),
            ] {
                if Command::new(cmd).args(&args).spawn().is_ok() {
                    return;
                }
            }
            return;
        }
    }
    let event = if urgent { "dialog-warning" } else { "message" };
    if Command::new("canberra-gtk-play")
        .args(["-i", event])
        .spawn()
        .is_ok()
    {
        return;
    }
    eprint!("\x07"); // BEL 兜底
}

/// 其它非 Windows 平台：暂不发声。
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn play_sound(_urgent: bool, _custom: Option<&str>) {}

/// 试听一个自定义提示音(托盘里选好后放一次让用户确认)；传 config 值(文件名或路径)。
pub fn preview_sound(value: &str) {
    play_sound(false, Some(value));
}

/// 诊断：每个收到的 hook 事件都打到 stderr(dev 控制台可见) + 追加到 exe 同目录 events.log。
/// 用于排查"某操作灯色不对"——看清到底触发了哪些 hook、顺序与间隔（例如权限弹窗出现时
/// 有没有 PermissionRequest 事件）。超过 256KB 自动清空，避免无限增长。临时诊断，定位后可移除。
fn log_event(event: &str, session_id: &str, status: &str) {
    // 实时打到 stderr —— 跑 `pnpm tauri dev` 时控制台直接能看到每个 hook 触发。
    eprintln!("[traffic-light][hook] {event} session={session_id} -> {status}");
    use std::io::Write;
    let Some(path) = crate::config::debug_log_path() else {
        return;
    };
    if std::fs::metadata(&path)
        .map(|m| m.len() > 256 * 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&path);
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{ms} {event} session={session_id} -> {status}");
    }
}

/// 从 hook JSON 负载里取 session_id、cwd 与 transcript_path。
fn parse_payload(body: &str) -> (String, Option<String>, Option<String>) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let session_id = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let cwd = v.get("cwd").and_then(|x| x.as_str()).map(|s| s.to_string());
    let transcript = v
        .get("transcript_path")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    (session_id, cwd, transcript)
}
