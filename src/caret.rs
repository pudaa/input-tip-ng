//! 光标（caret）位置探测。
//!
//! 走 GetGUIThreadInfo：跨进程可用，拿到的是前台线程的真实 caret 矩形（客户区坐标），
//! 再 ClientToScreen 转屏幕坐标。拿不到时不猜位置——宁可不显示，也不显示在错误的地方。

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetGUIThreadInfo};

/// 返回 caret 的屏幕坐标矩形（至少 1x1）
pub fn rect() -> Option<RECT> {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(0, &mut gti).ok()?;
        if gti.hwndCaret.0.is_null() {
            return None;
        }
        let caret = gti.rcCaret;
        let mut pt = POINT { x: caret.left, y: caret.top };
        if !ClientToScreen(gti.hwndCaret, &mut pt).as_bool() {
            return None;
        }

        let height = (caret.bottom - caret.top).max(1);
        let width = (caret.right - caret.left).max(1);
        Some(RECT {
            left: pt.x,
            top: pt.y,
            right: pt.x + width,
            bottom: pt.y + height,
        })
    }
}

/// 方便日志：给一个窗口句柄取 caret 所属窗口（诊断用）
pub fn owner() -> Option<HWND> {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(0, &mut gti).ok()?;
        if gti.hwndCaret.0.is_null() { None } else { Some(gti.hwndCaret) }
    }
}
