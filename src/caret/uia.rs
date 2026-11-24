//! 第二级：UI Automation 文本模式。
//!
//! Chromium/Electron/WinUI 只有这条路能给出真实光标位置。算法对照旧版 InputTip
//! 的 getCaretPosFromUIA 移植：
//! 1. 用 CacheRequest 一次性把 TextPattern / TextPattern2 缓存到焦点元素；
//! 2. 先取 GetCaretRange；拿不到就取 Selection 的最后一个 range 并折叠成光标；
//! 3. 按「字符」展开后取包围盒；字符单位在文末会失败，再按「行」展开取行右端。
//!
//! 另外注册一个焦点变化事件处理器：Chromium 只有在检测到 UIA 客户端时才会打开
//! 无障碍树，否则 TextPattern 根本不可用。

use std::cell::RefCell;

use windows::core::{implement, Ref, Result as WinResult};
use windows::Win32::Foundation::RECT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationFocusChangedEventHandler,
    IUIAutomationFocusChangedEventHandler_Impl, IUIAutomationTextPattern, IUIAutomationTextPattern2,
    IUIAutomationTextRange, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    TextUnit_Character, TextUnit_Line, UIA_TextPattern2Id, UIA_TextPatternId,
};

// 每个 tick 都 CoCreateInstance 会不停创建 UIA 实例（线程数、私有内存一路涨），
// 这里只创建一次并复用。
thread_local! {
    static AUTO: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

fn automation() -> Option<IUIAutomation> {
    AUTO.with(|slot| {
        if slot.borrow().is_none() {
            let created = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok() };
            *slot.borrow_mut() = created;
        }
        slot.borrow().clone()
    })
}

/// 从 SAFEARRAY 里读第一组 (x, y, w, h)
fn first_rect(psa: *mut SAFEARRAY) -> Option<(f64, f64, f64, f64)> {
    if psa.is_null() {
        return None;
    }
    unsafe {
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        if SafeArrayAccessData(psa, &mut data).is_err() || data.is_null() {
            let _ = SafeArrayDestroy(psa);
            return None;
        }
        let mut out = None;
        if SafeArrayGetUBound(psa, 0).unwrap_or(-1) >= 3 {
            let arr = data as *const f64;
            let (x, y, w, h) = (*arr, *arr.add(1), *arr.add(2), *arr.add(3));
            if x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() {
                out = Some((x, y, w, h));
            }
        }
        let _ = SafeArrayUnaccessData(psa);
        let _ = SafeArrayDestroy(psa);
        out
    }
}

fn range_rect(range: &IUIAutomationTextRange) -> Option<(f64, f64, f64, f64)> {
    unsafe { first_rect(range.GetBoundingRectangles().ok()?) }
}

/// 取焦点元素上指定的文本模式（用缓存请求，旧版就是这么做的）
fn cached_pattern<T: windows::core::Interface>(auto: &IUIAutomation, id: windows::Win32::UI::Accessibility::UIA_PATTERN_ID) -> Option<T> {
    unsafe {
        let cache = auto.CreateCacheRequest().ok()?;
        cache.AddPattern(UIA_TextPattern2Id).ok()?;
        cache.AddPattern(UIA_TextPatternId).ok()?;
        let focused = auto.GetFocusedElementBuildCache(&cache).ok()?;
        focused.GetCachedPatternAs::<T>(id).ok()
    }
}

fn focused_element() -> Option<IUIAutomationElement> {
    unsafe { automation()?.GetFocusedElement().ok() }
}

/// 拿到光标矩形
pub fn caret() -> Option<RECT> {
    let auto = automation()?;

    // 1) TextPattern2 的 caret range
    let mut range: Option<IUIAutomationTextRange> = None;
    if let Some(tp2) = cached_pattern::<IUIAutomationTextPattern2>(&auto, UIA_TextPattern2Id) {
        unsafe {
            let mut active = windows::core::BOOL(0);
            match tp2.GetCaretRange(&mut active) {
                Ok(r) if active.as_bool() => range = Some(r),
                _ => {}
            }
        }
    }

    // 2) 退回 TextPattern 的最后一个选区，并折叠成光标
    if range.is_none() {
        if let Some(tp) = cached_pattern::<IUIAutomationTextPattern>(&auto, UIA_TextPatternId) {
            unsafe {
                if let Ok(ranges) = tp.GetSelection() {
                    if let Ok(len) = ranges.Length() {
                        if len > 0 {
                            if let Ok(r) = ranges.GetElement(len - 1) {
                                let _ = r.MoveEndpointByRange(
                                    TextPatternRangeEndpoint_Start,
                                    &r,
                                    TextPatternRangeEndpoint_End,
                                );
                                range = Some(r);
                            }
                        }
                    }
                }
            }
        }
    }
    let range = range?;

    unsafe {
        // 3) 按字符展开（光标是退化区间，必须展开才有包围盒）
        let _ = range.ExpandToEnclosingUnit(TextUnit_Character);
        if let Some((x, y, _w, h)) = range_rect(&range) {
            if h > 0.0 {
                return Some(RECT {
                    left: x as i32,
                    top: y as i32,
                    right: x as i32 + 2,
                    bottom: (y + h) as i32,
                });
            }
        }

        // 4) 字符单位在文末等内容上会返回空，改按行展开，取行的右端
        let _ = range.ExpandToEnclosingUnit(TextUnit_Line);
        if let Some((x, y, w, h)) = range_rect(&range) {
            if h > 0.0 {
                let left = (x + w) as i32;
                return Some(RECT {
                    left,
                    top: y as i32,
                    right: left + 2,
                    bottom: (y + h) as i32,
                });
            }
        }
    }
    None
}

/// 焦点元素的类型与包围盒（锚点与诊断用，保留给后续按窗口覆盖表）
#[allow(dead_code)]
pub fn focused_rect() -> Option<(i32, RECT)> {
    let el = focused_element()?;
    unsafe {
        let rect = el.CurrentBoundingRectangle().ok()?;
        let ctype = el.CurrentControlType().map(|v| v.0).unwrap_or(-1);
        Some((ctype, rect))
    }
}

/// 一次拿全：控件类型、包围盒、是否具备文本能力（锚点判定用）
pub fn focused_info() -> Option<(i32, RECT, bool)> {
    let el = focused_element()?;
    unsafe {
        let rect = el.CurrentBoundingRectangle().ok()?;
        let ctype = el.CurrentControlType().map(|v| v.0).unwrap_or(-1);
        let capable = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).is_ok()
            || el
                .GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id)
                .is_ok();
        Some((ctype, rect, capable))
    }
}

/// 诊断：焦点元素支持哪些文本模式
pub fn probe_text() -> String {
    let Some(el) = focused_element() else {
        return "焦点元素不可用".to_string();
    };
    unsafe {
        let tp2 = el.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id).is_ok();
        let tp1 = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).is_ok();
        let name = el.CurrentName().map(|b| b.to_string()).unwrap_or_default();
        let ctype = el.CurrentControlType().map(|v| v.0).unwrap_or(-1);
        format!("name={name:?} type={ctype} textPattern2={tp2} textPattern={tp1}")
    }
}

// ---- Chromium 无障碍树唤醒 ----

#[implement(IUIAutomationFocusChangedEventHandler)]
struct FocusHandler;

impl IUIAutomationFocusChangedEventHandler_Impl for FocusHandler_Impl {
    fn HandleFocusChangedEvent(&self, _sender: Ref<IUIAutomationElement>) -> WinResult<()> {
        Ok(())
    }
}

// COM 指针不是 Send/Sync，且只会在主线程用，放 thread_local 最省事
thread_local! {
    static EVENT_WIRING: RefCell<Option<(IUIAutomation, IUIAutomationFocusChangedEventHandler)>> =
        const { RefCell::new(None) };
}

/// 注册焦点事件处理器：Chromium 检测到 UIA 客户端后会打开无障碍树，
/// TextPattern 才可能可用（这是让 Chrome/Edge/Typora 出光标的关键一步）。
pub fn ensure_event_handler() -> bool {
    EVENT_WIRING.with(|slot| {
        if slot.borrow().is_some() {
            return true;
        }
        let Some(auto) = automation() else {
            return false;
        };
        let handler: IUIAutomationFocusChangedEventHandler = FocusHandler.into();
        let ok = unsafe { auto.AddFocusChangedEventHandler(None, &handler).is_ok() };
        if ok {
            *slot.borrow_mut() = Some((auto, handler));
        }
        ok
    })
}
