// 平台相关：悬浮灯定位、置顶、前台窗口检测（“聚焦常亮”用）。
//
// Windows 用 Win32（任务栏上方定位、SetWindowPos 置顶、Toolhelp 取前台进程名）。
// macOS/Linux 用 active-win-pos-rs 取前台窗口、用 Tauri 主屏信息做角落定位。
// Wayland 取不到前台窗口 → foreground_token() 返回 None，聚焦常亮降级为“持续闪到状态改变”。

use tauri::WebviewWindow;

/// 需【标题含项目名】才算切到那个具体窗口的工作窗口：GUI 编辑器 / IDE。
/// 它们标题里带工作区/项目名，且常多开窗口，得靠标题区分到底是哪个窗口。
const TITLE_MATCH_TOKENS: &[&str] = &[
    "code",
    "code - insiders",
    "cursor",
    "windsurf",
    "antigravity ide",
    "antigravity",
];

/// 只比【进程名】即算已查看的工作窗口：终端，以及独立的 Codex / Claude Code 桌面应用。
/// 这些标题不一定含项目名、通常就一个窗口，切到它就算你在看（多个同类窗口无法互相区分，可接受）。
const PROCESS_ONLY_TOKENS: &[&str] = &[
    // 独立桌面应用（codex.exe / claude.exe）
    "claude",
    "codex",
    // 终端（Windows / macOS / Linux）
    "windowsterminal",
    "wt",
    "powershell",
    "pwsh",
    "cmd",
    "conhost",
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


/// 前台窗口是否是「给定来源 + 给定项目(label)」对应的那个工作窗口。平台无关。
/// 用于"精确到窗口"的停闪。分两类处理：
/// 只比进程名类(PROCESS_ONLY_TOKENS：终端、独立 Codex/Claude 应用)：切到它就算已查看。
/// 标题匹配类(TITLE_MATCH_TOKENS：VS Code 等编辑器)：要标题含项目名才算切到那个具体窗口；
/// label 为空、或取不到标题(如 Wayland)时退化为只比进程名。
pub fn foreground_matches_window(source: &str, label: &str) -> bool {
    let Some(tok) = foreground_token() else {
        return false;
    };
    match source {
        "codex" | "claude" | "antigravity" => {}
        _ => return false,
    }
    let tok = tok.as_str();
    // 终端 / 独立应用：标题不可靠或就一个窗口 -> 只比进程名，切到它就算已查看。
    if PROCESS_ONLY_TOKENS.contains(&tok) {
        return true;
    }
    // 编辑器：要标题含项目名才算切到那个具体窗口。
    if !TITLE_MATCH_TOKENS.contains(&tok) {
        return false;
    }
    let label = label.trim();
    if label.is_empty() {
        return true; // 没项目名 -> 只比进程名
    }
    match foreground_title() {
        Some(title) => title.to_lowercase().contains(&label.to_lowercase()),
        None => true, // 取不到标题 -> 退化为进程名匹配
    }
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

/// 取当前前台窗口的标题文本。用于"精确到窗口"：标题里通常带项目/工作区名，
/// 据此区分同一个 app(如 VS Code)的多个窗口。取不到返回 None。
#[cfg(windows)]
pub fn foreground_title() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..n as usize]))
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

#[cfg(not(windows))]
pub fn foreground_title() -> Option<String> {
    let win = active_win_pos_rs::get_active_window().ok()?;
    let t = win.title.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
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
