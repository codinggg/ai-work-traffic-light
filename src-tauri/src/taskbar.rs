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
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};

/// 灯相对任务栏左缘的水平偏移（像素）：越过最左侧的天气/widgets 按钮，避免重叠。
/// 想再左/右移就改这个值。
const LEFT_MARGIN_PX: i32 = 180;

/// 贴到任务栏左侧、垂直居中。
pub fn position_over_taskbar(window: &WebviewWindow) {
    let Some(rect) = taskbar_rect() else {
        return;
    };
    let size = window
        .outer_size()
        .unwrap_or(PhysicalSize::new(240, 44));

    let taskbar_h = rect.bottom - rect.top;
    let y = rect.top + ((taskbar_h - size.height as i32) / 2).max(0);
    let x = rect.left + LEFT_MARGIN_PX; // 越过最左侧的天气/widgets，避免重叠

    let _ = window.set_position(PhysicalPosition::new(x, y));
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
