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


/// 第三级：UI Automation 文本模式。Chromium/Electron/WinUI 这类程序
/// GUITHREADINFO 没有 caret，GetCaretPos 又会返回线程里某个陈旧位置，
/// 只有 UIA 的 TextPattern 能给出真正的文本光标矩形。
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

/// 诊断：把 UIA 每一步的结果摊开（Chromium 上 uia=不可用 时要靠它定位是哪一步失败）
pub fn uia_probe() -> String {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, IUIAutomationTextPattern2,
        UIA_TextPattern2Id, UIA_TextPatternId,
    };

    unsafe {
        let automation: IUIAutomation =
            match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                Ok(a) => a,
                Err(e) => return format!("CoCreateInstance 失败: {e}"),
            };
        let focused = match automation.GetFocusedElement() {
            Ok(f) => f,
            Err(e) => return format!("GetFocusedElement 失败: {e}"),
        };
        let name = focused.CurrentName().map(|b| b.to_string()).unwrap_or_default();
        let ctype = focused.CurrentControlType().map(|v| v.0).unwrap_or(-1);
        let tp2 = focused.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id);
        let tp1 = focused.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId);
        let mut s = format!(
            "focused={name:?} controlType={ctype} textPattern2={} textPattern={}",
            tp2.is_ok(),
            tp1.is_ok()
        );
        if let Ok(p) = tp2 {
            let mut active = windows::core::BOOL(0);
            match p.GetCaretRange(&mut active) {
                Ok(_) => s.push_str(&format!(" caretRange=ok active={}", active.as_bool())),
                Err(e) => s.push_str(&format!(" GetCaretRange 失败: {e}")),
            }
        }
        if let Some(r) = uia_caret() {
            s.push_str(&format!(" rect=({}, {}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top));
        } else {
            s.push_str(" rect=未取到");
        }
        s
    }
}


/// 第四级：MSAA 的 caret 对象（AccessibleObjectFromWindow + OBJID_CARET）。
/// Chromium 的地址栏/网页编辑框、部分 Win32 控件用这条路能拿到真实光标位置。
fn msaa_caret() -> Option<RECT> {
    use windows::core::{GUID, Interface};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};

    // {618736E0-3C3D-11CF-810C-00AA00389B71}
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
        // 光标应该是一条细线；明显不是的情形（整窗大小）直接丢弃
        if w <= 0 || h <= 0 || w > 64 || h > 200 {
            return None;
        }
        Some(RECT { left: l, top: t, right: l + w, bottom: t + h })
    }
}


/// 取 UIA 焦点元素的包围盒（用来校验 GetCaretPos 给出的点是否落在输入框里）
fn uia_focused_rect() -> Option<RECT> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let focused = automation.GetFocusedElement().ok()?;
        focused.CurrentBoundingRectangle().ok()
    }
}

/// 第五级兜底：没有真实光标时，用 UIA 焦点元素的包围盒右下角作锚点。
/// 只接受「看得像输入框」的元素（Edit 控件，或面积明显小于窗口的矩形），
/// 避免在 Chromium 页面主体上把提示画到窗口右下角。
fn uia_element_anchor() -> Option<RECT> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, UIA_EditControlTypeId};

    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let focused = automation.GetFocusedElement().ok()?;
        let rect = focused.CurrentBoundingRectangle().ok()?;
        let control_type = focused.CurrentControlType().map(|v| v.0).unwrap_or(-1);

        let fg = GetForegroundWindow();
        let mut win = RECT::default();
        if fg.0.is_null() || windows::Win32::UI::WindowsAndMessaging::GetWindowRect(fg, &mut win).is_err() {
            return None;
        }
        let ew = (rect.right - rect.left).max(0) as i64;
        let eh = (rect.bottom - rect.top).max(0) as i64;
        let ww = (win.right - win.left).max(1) as i64;
        let wh = (win.bottom - win.top).max(1) as i64;
        if ew <= 0 || eh <= 0 {
            return None;
        }
        let is_edit = control_type == UIA_EditControlTypeId.0;
        let small_enough = ew * eh < ww * wh / 2;
        if !is_edit && !small_enough {
            return None;
        }
        // 锚点放在元素右下角，配合「上方 -offset」的默认摆放，视觉上贴在输入框右端
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

    // 二级：UIA 文本模式（Chromium/Electron/WinUI 只有这条能给出真实位置）
    if let Some(rect) = uia_caret() {
        return Some(rect);
    }

    // 三级：MSAA caret 对象（Chromium 地址栏、部分 Win32 控件）
    if let Some(rect) = msaa_caret() {
        return Some(rect);
    }

    // 四级：共享消息队列后读 GetCaretPos（部分原生控件可用；Chromium 会给出 (0,0) 之类的陈旧位置）
    if let Some((_, pt)) = attached_caret() {
        if pt.x > 1 || pt.y > 1 {
            let mut plausible = true;
            unsafe {
                let fg = GetForegroundWindow();
                if !fg.0.is_null() {
                    let mut rc = RECT::default();
                    if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(fg, &mut rc).is_ok() {
                        let margin = 80;
                        plausible = pt.x >= rc.left - margin
                            && pt.x <= rc.right + margin
                            && pt.y >= rc.top - margin
                            && pt.y <= rc.bottom + margin;
                    }
                }
            }
            // 还要落在 UIA 焦点元素里：Chromium 的 GetCaretPos 会给出窗口里某个陈旧位置，
            // 只按窗口范围校验挡不住（提示会跑到窗口顶部）
            if plausible {
                if let Some(focus_rect) = uia_focused_rect() {
                    let margin = 16;
                    plausible = pt.x >= focus_rect.left - margin
                        && pt.x <= focus_rect.right + margin
                        && pt.y >= focus_rect.top - margin
                        && pt.y <= focus_rect.bottom + margin;
                }
            }
            if plausible {
                return Some(RECT {
                    left: pt.x,
                    top: pt.y,
                    right: pt.x + 2,
                    bottom: pt.y + 16,
                });
            }
        }
    }

    // 五级兜底：UIA 焦点元素包围盒右下角
    uia_element_anchor()
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
        let uia = match uia_caret() {
            Some(r) => format!("uia=({}, {}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => "uia=不可用".to_string(),
        };
        let msaa = match msaa_caret() {
            Some(r) => format!("msaa=({}, {}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => "msaa=不可用".to_string(),
        };
        let attached = match attached_caret() {
            Some((hwnd, pt)) => format!("attach_focus={} caret=({}, {})", hwnd.0 as isize, pt.x, pt.y),
            None => "attach=不可用".to_string(),
        };
        format!("{gti_result} | {uia} | {msaa} | {attached}")
    }
}

/// caret 所属窗口（诊断用）
pub fn owner() -> Option<HWND> {
    gti_caret().map(|(hwnd, _)| hwnd)
}
