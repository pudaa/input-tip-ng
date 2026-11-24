//! 光标（caret）位置探测。
//!
//! 两级策略：
//! 1. GetGUIThreadInfo(0)：大多数原生控件可用，一次调用拿到前台线程的 caret 矩形；
//! 2. 失败时 AttachThreadInput + GetCaretPos：Win11 记事本（RichEditD2DPT）等控件
//!    GUITHREADINFO 里 flags=0、不给 rcCaret，但共享消息队列后 GetCaretPos 仍能取到。
//!
//! 都拿不到就不猜位置——宁可不显示，也不显示在错误的地方。

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetCaretPos, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
};

fn gti_caret() -> Option<(HWND, RECT)> {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(0, &mut gti).ok()?;
        if gti.hwndCaret.0.is_null() {
            return None;
        }
        Some((gti.hwndCaret, gti.rcCaret))
    }
}

/// 共享消息队列后读 caret：兼容不填 GUITHREADINFO 的控件
fn attached_caret() -> Option<(HWND, POINT)> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let target_tid = GetWindowThreadProcessId(fg, None);
        if target_tid == 0 || target_tid == GetCurrentThreadId() {
            return None;
        }
        if !AttachThreadInput(GetCurrentThreadId(), target_tid, true).as_bool() {
            return None;
        }
        let mut pt = POINT::default();
        let ok = GetCaretPos(&mut pt).is_ok();
        let focus = GetFocus();
        let _ = AttachThreadInput(GetCurrentThreadId(), target_tid, false);
        if !ok || focus.0.is_null() {
            let _ = focus;
            return None;
        }
        if !ClientToScreen(focus, &mut pt).as_bool() {
            return None;
        }
        Some((focus, pt))
    }
}

/// 返回 caret 的屏幕坐标矩形（至少 1x1）
pub fn rect() -> Option<RECT> {
    if let Some((hwnd, caret)) = gti_caret() {
        let mut pt = POINT { x: caret.left, y: caret.top };
        unsafe {
            if !ClientToScreen(hwnd, &mut pt).as_bool() {
                return None;
            }
        }
        let height = (caret.bottom - caret.top).max(1);
        let width = (caret.right - caret.left).max(1);
        return Some(RECT {
            left: pt.x,
            top: pt.y,
            right: pt.x + width,
            bottom: pt.y + height,
        });
    }

    // 兜底：拿不到矩形时用 GetCaretPos 的点位，按字体高度估一个 2px 宽的竖线
    let (_, pt) = attached_caret()?;
    Some(RECT {
        left: pt.x,
        top: pt.y,
        right: pt.x + 2,
        bottom: pt.y + 16,
    })
}

/// 诊断：把两级的原始返回值都摊开，用来判断该修哪一级
pub fn probe() -> String {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let gti_result = match GetGUIThreadInfo(0, &mut gti) {
            Ok(()) => format!(
                "flags=0x{:X} focus={} caret_hwnd={} rc=({}, {}) {}x{}",
                gti.flags.0,
                gti.hwndFocus.0 as isize,
                gti.hwndCaret.0 as isize,
                gti.rcCaret.left,
                gti.rcCaret.top,
                gti.rcCaret.right - gti.rcCaret.left,
                gti.rcCaret.bottom - gti.rcCaret.top
            ),
            Err(e) => format!("GetGUIThreadInfo 失败: {e}"),
        };
        let attached = match attached_caret() {
            Some((hwnd, pt)) => format!("attach_focus={} caret=({}, {})", hwnd.0 as isize, pt.x, pt.y),
            None => "attach=不可用".to_string(),
        };
        format!("{gti_result} | {attached}")
    }
}

/// caret 所属窗口（诊断用）
pub fn owner() -> Option<HWND> {
    gti_caret().map(|(hwnd, _)| hwnd)
}
