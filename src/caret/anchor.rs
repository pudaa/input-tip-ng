//! 第四级兜底：拿不到真实光标时，锚到「正在输入的那个控件」。
//!
//! 只接受 Edit 控件或带 TextPattern 的元素，并且必须比窗口小一半以上，
//! 否则会把整页文档当成输入框，指示牌就跑到窗口边上了。

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

pub fn rect() -> Option<RECT> {
    let (control_type, rect, capable) = super::uia::focused_info()?;
    use windows::Win32::UI::Accessibility::UIA_EditControlTypeId;
    if control_type != UIA_EditControlTypeId.0 && !capable {
        return None;
    }

    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let mut win = RECT::default();
        if GetWindowRect(fg, &mut win).is_err() {
            return None;
        }
        let ew = (rect.right - rect.left).max(0) as i64;
        let eh = (rect.bottom - rect.top).max(0) as i64;
        let ww = (win.right - win.left).max(1) as i64;
        let wh = (win.bottom - win.top).max(1) as i64;
        if ew <= 0 || eh <= 0 || ew * eh >= ww * wh / 2 {
            return None;
        }
    }

    // 锚在输入框右缘、垂直居中
    let mid_y = rect.top + (rect.bottom - rect.top) / 2;
    Some(RECT { left: rect.right, top: mid_y, right: rect.right + 2, bottom: mid_y + 2 })
}
