//! 第一级：GetGUIThreadInfo（原生控件最快的一条）

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetGUIThreadInfo};

/// 返回 (caret 所属窗口, 客户区坐标的 caret 矩形)
pub fn caret() -> Option<(HWND, RECT)> {
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

/// 前台线程的焦点窗口（MSAA 阶段要用）
pub fn focus_window() -> HWND {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(0, &mut gti).is_ok() {
            return gti.hwndFocus;
        }
        HWND::default()
    }
}
