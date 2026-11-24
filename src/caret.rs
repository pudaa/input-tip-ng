//! 光标（caret）位置探测。
//!
//! 四级策略，逐级降级：
//! 1. GetGUIThreadInfo：大多数原生控件一次调用就能拿到 caret 矩形；
//! 2. UIA TextPattern：Chromium/Electron/WinUI 的网页编辑区只有这条能给出真实位置；
//! 3. MSAA OBJID_CARET：部分浏览器/控件的辅助对象能报光标位置；
//! 4. UIA 焦点元素包围盒：拿不到真实光标时，锚到「正在输入的那个控件」上（仅接受 Edit
//!    或带 TextPattern 的元素），比乱猜一个坐标安全。
//!
//! 明确不用 AttachThreadInput + GetCaretPos：实测它会让目标应用的输入法被锁成英文，
//! 而且 Chromium 返回的是陈旧坐标。

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo};

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

fn uia_caret() -> Option<RECT> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, SAFEARRAY};
    use windows::Win32::System::Ole::{
        SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetUBound, SafeArrayUnaccessData,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, IUIAutomationTextPattern2,
        IUIAutomationTextRange, UIA_TextPattern2Id, UIA_TextPatternId,
    };

    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let focused = automation.GetFocusedElement().ok()?;

        let range: IUIAutomationTextRange =
            if let Ok(p) = focused.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id)
            {
                let mut active = windows::core::BOOL(0);
                match p.GetCaretRange(&mut active) {
                    Ok(r) if active.as_bool() => r,
                    _ => p.GetSelection().ok()?.GetElement(0).ok()?,
                }
            } else if let Ok(p) =
                focused.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            {
                p.GetSelection().ok()?.GetElement(0).ok()?
            } else {
                return None;
            };

        let bounds: *mut SAFEARRAY = range.GetBoundingRectangles().ok()?;
        if bounds.is_null() {
            return None;
        }
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        if SafeArrayAccessData(bounds, &mut data).is_err() || data.is_null() {
            let _ = SafeArrayDestroy(bounds);
            return None;
        }
        let upper = SafeArrayGetUBound(bounds, 0).unwrap_or(-1);
        let mut out = None;
        if upper >= 3 {
            let arr = data as *const f64;
            let (x, y, w, h) = (*arr, *arr.add(1), *arr.add(2), *arr.add(3));
            if w.is_finite() && h.is_finite() && w >= 0.0 && h >= 0.0 {
                out = Some(RECT {
                    left: x as i32,
                    top: y as i32,
                    right: (x + w.max(2.0)) as i32,
                    bottom: (y + h.max(2.0)) as i32,
                });
            }
        }
        let _ = SafeArrayUnaccessData(bounds);
        let _ = SafeArrayDestroy(bounds);
        out
    }
}

fn msaa_caret() -> Option<RECT> {
    use windows::core::{GUID, Interface};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};

    const IID_IACCESSIBLE: GUID = GUID::from_u128(0x618736E0_3C3D_11CF_810C_00AA00389B71);
    const OBJID_CARET: u32 = 0xFFFF_FFF8;

    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
        AccessibleObjectFromWindow(fg, OBJID_CARET, &IID_IACCESSIBLE, &mut raw).ok()?;
        if raw.is_null() {
            return None;
        }
        let acc: IAccessible = IAccessible::from_raw(raw);
        let child = VARIANT::from(0i32); // CHILDID_SELF
        let (mut l, mut t, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
        acc.accLocation(&mut l, &mut t, &mut w, &mut h, &child).ok()?;
        if w <= 0 || h <= 0 || w > 64 || h > 200 {
            return None;
        }
        Some(RECT { left: l, top: t, right: l + w, bottom: t + h })
    }
}

fn uia_focused_element() -> Option<(i32, RECT)> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let focused = automation.GetFocusedElement().ok()?;
        let rect = focused.CurrentBoundingRectangle().ok()?;
        let control_type = focused.CurrentControlType().map(|v| v.0).unwrap_or(-1);
        Some((control_type, rect))
    }
}

fn uia_element_anchor() -> Option<RECT> {
    use windows::Win32::UI::Accessibility::{
        IUIAutomationTextPattern, IUIAutomationTextPattern2, UIA_EditControlTypeId,
        UIA_TextPattern2Id, UIA_TextPatternId,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let focused = automation.GetFocusedElement().ok()?;
        let rect = focused.CurrentBoundingRectangle().ok()?;
        let control_type = focused.CurrentControlType().map(|v| v.0).unwrap_or(-1);

        let is_edit = control_type == UIA_EditControlTypeId.0;
        let has_text_pattern = focused
            .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .is_ok()
            || focused
                .GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id)
                .is_ok();
        if !is_edit && !has_text_pattern {
            return None;
        }

        let mut win = RECT::default();
        let fg = GetForegroundWindow();
        if fg.0.is_null()
            || windows::Win32::UI::WindowsAndMessaging::GetWindowRect(fg, &mut win).is_err()
        {
            return None;
        }
        let ew = (rect.right - rect.left).max(0) as i64;
        let eh = (rect.bottom - rect.top).max(0) as i64;
        let ww = (win.right - win.left).max(1) as i64;
        let wh = (win.bottom - win.top).max(1) as i64;
        if ew <= 0 || eh <= 0 || (ew * eh) >= (ww * wh / 2) {
            return None; // 整窗大小的元素不是输入框
        }
        Some(RECT { left: rect.right, top: rect.bottom, right: rect.right + 2, bottom: rect.bottom + 2 })
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
    if let Some(rect) = uia_caret() {
        return Some(rect);
    }
    if let Some(rect) = msaa_caret() {
        return Some(rect);
    }
    uia_element_anchor()
}

/// 诊断：把每一级的原始返回值摊开
pub fn probe() -> String {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let gti_result = match GetGUIThreadInfo(0, &mut gti) {
            Ok(()) => format!(
                "gti(flags=0x{:X} caret_hwnd={} rc=({},{}))",
                gti.flags.0,
                gti.hwndCaret.0 as isize,
                gti.rcCaret.left,
                gti.rcCaret.top
            ),
            Err(e) => format!("gti 失败: {e}"),
        };
        let uia = match uia_caret() {
            Some(r) => format!("uia=({},{}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => "uia=不可用".to_string(),
        };
        let msaa = match msaa_caret() {
            Some(r) => format!("msaa=({},{}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => "msaa=不可用".to_string(),
        };
        let anchor = match uia_element_anchor() {
            Some(r) => format!("anchor=({},{})", r.left, r.top),
            None => "anchor=不可用".to_string(),
        };
        let focused = match uia_focused_element() {
            Some((t, r)) => format!("focusedType={t} rect=({},{}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => "focused=不可用".to_string(),
        };
        format!("{gti_result} | {uia} | {msaa} | {anchor} | {focused}")
    }
}

/// caret 所属窗口（诊断用）
pub fn owner() -> Option<HWND> {
    gti_caret().map(|(hwnd, _)| hwnd)
}

/// UIA 全链路诊断（--probe 的补充行）
pub fn uia_probe() -> String {
    format!("{}", if uia_caret().is_some() { "UIA 文本光标可用" } else { "UIA 文本光标不可用" })
}
