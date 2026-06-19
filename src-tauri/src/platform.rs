// 平台相关：悬浮灯定位、置顶、前台窗口检测（“聚焦常亮”用）。
//
// Windows 用 Win32（任务栏上方定位、SetWindowPos 置顶、Toolhelp 取前台进程名）。
// macOS/Linux 用 active-win-pos-rs 取前台窗口、用 Tauri 主屏信息做角落定位。
// Wayland 取不到前台窗口 → foreground_token() 返回 None，聚焦常亮降级为“持续闪到状态改变”。

use tauri::WebviewWindow;

/// Claude 的「工作窗口」归一化标识（小写、无 .exe）：编辑器 + 各平台常见终端。
/// 切到这些窗口 = 你在看 Claude。多余项无害（各平台取超集）。
const EDITOR_TOKENS: &[&str] = &[
    "code",
    "code - insiders",
    "cursor",
    "windsurf",
    "claude",
    // 终端（Windows / macOS / Linux）
    "windowsterminal",
    "wt",
    "powershell",
    "pwsh",
    "cmd",
    "terminal",
    "iterm2",
    "wezterm",
    "alacritty",
    "kitty",
    "konsole",
    "gnome-terminal",
    "gnome-terminal-server",
    "tilix",
    "xterm",
];

/// Codex 的「工作窗口」标识：Codex 独立窗口。
/// 若你的 Codex 跑在 VS Code 扩展里（窗口名是 code），把 "code" 也加进来即可。
const CODEX_TOKENS: &[&str] = &["codex"];

/// 前台窗口是否是给定来源（"claude"/"codex"）对应的工作窗口。平台无关。
/// 用于“精确到窗口”的停闪：哪个来源在催你，就得切到它的窗口才算已查看（常亮）。
pub fn foreground_matches_source(source: &str) -> bool {
    let Some(tok) = foreground_token() else {
        return false;
    };
    let set: &[&str] = match source {
        "codex" => CODEX_TOKENS,
        "claude" => EDITOR_TOKENS,
        _ => return false,
    };
    set.contains(&tok.as_str())
}

/// 归一化进程/应用名：小写 + 去掉结尾 ".exe"。
fn normalize(name: &str) -> String {
    let n = name.trim().to_lowercase();
    n.strip_suffix(".exe").unwrap_or(&n).to_string()
}

// ===== 前台窗口标识：唯一按 OS 分支的部分 =====

/// 取当前前台窗口的归一化进程/应用名。失败（或 Wayland 取不到）返回 None。
/// 也用于开发模式调试打印（核对到底把哪个窗口认成了什么）。
#[cfg(windows)]
pub fn foreground_token() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        process_name(pid).map(|n| normalize(&n))
    }
}

#[cfg(not(windows))]
pub fn foreground_token() -> Option<String> {
    let win = active_win_pos_rs::get_active_window().ok()?;
    // macOS：app_name 最准（"Code"/"Codex"）。
    // Linux：可执行文件名（"code"/"codex"）更准，app_name 可能是 WM_CLASS。
    #[cfg(target_os = "macos")]
    let raw = win.app_name;
    #[cfg(not(target_os = "macos"))]
    let raw = win
        .process_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(win.app_name);
    let tok = normalize(&raw);
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

// ===== 首次显示时的定位 =====

#[cfg(windows)]
pub fn place_window(window: &WebviewWindow) {
    position_over_taskbar(window);
}

/// macOS/Linux：没有“任务栏”概念，摆到主屏左下角留边距。之后用户可拖动，位置会被持久化。
#[cfg(not(windows))]
pub fn place_window(window: &WebviewWindow) {
    use tauri::{PhysicalPosition, PhysicalSize};
    const MARGIN: i32 = 24;
    const BOTTOM_GAP: i32 = 56; // 躲开底部 dock/panel 的大致高度
    if let Ok(Some(mon)) = window.primary_monitor() {
        let msize = mon.size();
        let mpos = mon.position();
        let wsize = window.outer_size().unwrap_or(PhysicalSize::new(120, 50));
        let x = mpos.x + MARGIN;
        let y = (mpos.y + msize.height as i32 - wsize.height as i32 - BOTTOM_GAP).max(mpos.y);
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

// ===== 置顶 =====

/// 把灯重新顶到最前。仅 Windows 需要：点任务栏会把任务栏抢到 topmost band 最前盖住灯。
#[cfg(windows)]
pub fn reassert_topmost(window: &WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let Ok(raw) = window.hwnd() else {
        return;
    };
    // HWND 跨 windows-crate 版本桥接（Tauri 0.61 / 本 crate 0.58，皆为 *mut c_void 包装）。
    let hwnd = HWND(raw.0);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// macOS/Linux：alwaysOnTop 已足够，无需周期性重置。
#[cfg(not(windows))]
pub fn reassert_topmost(_window: &WebviewWindow) {}

// ===== Windows 专用实现 =====

/// 灯相对任务栏左缘的水平偏移：越过最左侧的天气/widgets 按钮。
#[cfg(windows)]
const LEFT_MARGIN_PX: i32 = 180;
/// 灯底部与任务栏顶边的间隙。
#[cfg(windows)]
const GAP_ABOVE_PX: i32 = 4;

/// 放在任务栏正上方、靠左（不放进任务栏矩形内，否则会被任务栏盖住看不到）。
#[cfg(windows)]
fn position_over_taskbar(window: &WebviewWindow) {
    use tauri::{PhysicalPosition, PhysicalSize};
    let Some(rect) = taskbar_rect() else {
        return;
    };
    let size = window.outer_size().unwrap_or(PhysicalSize::new(240, 44));
    let x = rect.left + LEFT_MARGIN_PX;
    let y = (rect.top - size.height as i32 - GAP_ABOVE_PX).max(0);
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// 取任务栏窗口的屏幕矩形（物理像素）。
#[cfg(windows)]
fn taskbar_rect() -> Option<windows::Win32::Foundation::RECT> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};
    unsafe {
        let hwnd = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        if hwnd.0.is_null() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some(rect)
    }
}

/// 用进程快照按 pid 取可执行文件名（只含文件名）。
/// 为何不用 OpenProcess+QueryFullProcessImageNameW：普通权限进程打不开**提权进程**
/// （如以管理员运行的 VS Code/Codex），OpenProcess 会失败而误判成“非工作窗口”。
/// Toolhelp 快照能列出所有进程的名字（任务管理器同理），不受提权影响。
#[cfg(windows)]
fn process_name(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    let end = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    found = Some(String::from_utf16_lossy(&entry.szExeFile[..end]));
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}
