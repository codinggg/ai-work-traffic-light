// U6: 把悬浮灯定位到 Windows 任务栏左下（天气/widgets 附近）。
//
// Windows 11 砍了 deskband，没法真正嵌入任务栏；这里用 TrafficMonitor 同款思路：
// 找到任务栏窗口(Shell_TrayWnd)拿到它的屏幕矩形，把我们这个置顶悬浮窗摆到
// 它的左侧、垂直居中。失败(找不到任务栏/取不到矩形)就保持原位，不致命。
//
// 已知 v1 局限：仅处理主任务栏；任务栏自动隐藏、非底部停靠、跨多显示器的
// 精细处理留待后续。

use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
};

/// 灯相对任务栏左缘的水平偏移（像素）：越过最左侧的天气/widgets 按钮，避免重叠。
/// 想再左/右移就改这个值。
const LEFT_MARGIN_PX: i32 = 180;
/// 灯底部与任务栏顶边的间隙（像素）。
const GAP_ABOVE_PX: i32 = 4;

/// 放在任务栏正上方、靠左（不放进任务栏矩形内，否则会被任务栏盖住看不到）。
pub fn position_over_taskbar(window: &WebviewWindow) {
    let Some(rect) = taskbar_rect() else {
        return;
    };
    let size = window
        .outer_size()
        .unwrap_or(PhysicalSize::new(240, 44));

    let x = rect.left + LEFT_MARGIN_PX; // 越过最左侧的天气/widgets
    let y = (rect.top - size.height as i32 - GAP_ABOVE_PX).max(0); // 任务栏上方

    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// 把灯重新顶到所有置顶窗口的最前面。
///
/// 为什么需要：任务栏(Shell_TrayWnd)自己也是 topmost；用户点任务栏时，shell 会把
/// 任务栏提到 topmost band 的最前，于是和灯重叠的地方就被任务栏盖住了。重新插一次
/// HWND_TOPMOST 就能把灯夺回最前。NOMOVE|NOSIZE 保持原位原大小，NOACTIVATE 不抢焦点
/// （不会打断用户正在操作的窗口）。
///
/// HWND 跨 crate 版本桥接：Tauri 自带 windows 0.61，本 crate 用 0.58，两版 HWND 都是
/// `*mut c_void` 的透明包装，取原始指针重建本版 HWND 即可。
pub fn reassert_topmost(window: &WebviewWindow) {
    let Ok(raw) = window.hwnd() else {
        return;
    };
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

/// 取任务栏窗口的屏幕矩形（物理像素）。
fn taskbar_rect() -> Option<RECT> {
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

/// Claude 的「工作窗口」进程名（小写）：编辑器/终端。切到这些里说明你在看 Claude。
const EDITOR_PROCESSES: &[&str] = &[
    "code.exe",            // VS Code
    "code - insiders.exe", // VS Code Insiders
    "cursor.exe",          // Cursor
    "windsurf.exe",        // Windsurf
    "claude.exe",          // Claude 桌面端
    "windowsterminal.exe", // Windows Terminal
    "wt.exe",
    "powershell.exe",
    "pwsh.exe",
    "cmd.exe",
];

/// Codex 的「工作窗口」进程名（小写）：Codex 独立窗口。
/// 若你的 Codex 跑在 VS Code 扩展里(窗口是 code.exe)，把 "code.exe" 也加进来即可。
const CODEX_PROCESSES: &[&str] = &["codex.exe"];

/// 前台窗口是否是给定来源("claude"/"codex")对应的工作窗口。
/// 用于"精确到窗口"的停闪：哪个来源在催你，就得切到它的窗口才算已查看(常亮)。
pub fn foreground_matches_source(source: &str) -> bool {
    let Some(name) = foreground_process_name() else {
        return false;
    };
    let name = name.to_lowercase();
    let set: &[&str] = match source {
        "codex" => CODEX_PROCESSES,
        "claude" => EDITOR_PROCESSES,
        _ => return false,
    };
    set.contains(&name.as_str())
}

/// 取当前前台窗口的进程名（只含文件名，如 "Code.exe"）。失败返回 None。
/// 既用于判定工作窗口，也用于调试输出（看清到底把哪个窗口认成了什么）。
pub fn foreground_process_name() -> Option<String> {
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
        process_name(pid)
    }
}

/// 用进程快照按 pid 取可执行文件名（只含文件名）。
/// 为何不用 OpenProcess+QueryFullProcessImageNameW：普通权限进程打不开**提权进程**
/// （如以管理员运行的 VS Code/Codex），OpenProcess 会失败而误判成"非工作窗口"。
/// Toolhelp 快照能列出所有进程的名字（任务管理器同理），不受提权影响。
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
