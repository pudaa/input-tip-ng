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
    CUIAutomation8, IUIAutomation, IUIAutomationElement, IUIAutomationFocusChangedEventHandler,
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
            // 必须用 CUIAutomation8（UIA3）：Chromium 的 UIA 提供者只对 UIA3 客户端
            // 暴露完整的 TextPattern；UIA1 只能看到 MSAA 桥，包围盒全空。
            let created = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok() };
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

/// 从一个文本区间得到光标位置：先按字符展开，失败再按行展开取行右端。
fn rect_from_range(range: &IUIAutomationTextRange) -> Option<RECT> {
    unsafe {
        let _ = range.ExpandToEnclosingUnit(TextUnit_Character);
        if let Some((x, y, _w, h)) = range_rect(range) {
            if h > 0.0 {
                return Some(RECT {
                    left: x as i32,
                    top: y as i32,
                    right: x as i32 + 2,
                    bottom: (y + h) as i32,
                });
            }
        }
        let _ = range.ExpandToEnclosingUnit(TextUnit_Line);
        if let Some((x, y, w, h)) = range_rect(range) {
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

/// 拿到光标矩形。
///
/// 顺序很重要：Chromium 的 caret range 经常能拿到但包围盒为空，而选区 range 有包围盒。
/// 所以「caret range 失败/无矩形」都要继续试选区，最后才放弃。
pub fn caret() -> Option<RECT> {
    wake_throttled();
    let auto = automation()?;

    if let Some(tp2) = cached_pattern::<IUIAutomationTextPattern2>(&auto, UIA_TextPattern2Id) {
        unsafe {
            let mut active = windows::core::BOOL(0);
            if let Ok(range) = tp2.GetCaretRange(&mut active) {
                if let Some(rect) = rect_from_range(&range) {
                    return Some(rect);
                }
            }
        }
    }

    if let Some(tp) = cached_pattern::<IUIAutomationTextPattern>(&auto, UIA_TextPatternId) {
        unsafe {
            if let Ok(ranges) = tp.GetSelection() {
                if let Ok(len) = ranges.Length() {
                    for i in (0..len).rev() {
                        if let Ok(r) = ranges.GetElement(i) {
                            // 先按原样取（Chromium 折叠后反而没有包围盒），不行再折叠
                            if let Some(rect) = rect_from_range(&r) {
                                return Some(rect);
                            }
                            let _ = r.MoveEndpointByRange(
                                TextPatternRangeEndpoint_Start,
                                &r,
                                TextPatternRangeEndpoint_End,
                            );
                            if let Some(rect) = rect_from_range(&r) {
                                return Some(rect);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// 唤醒要限流：每次唤醒都要枚举子窗口并做 MSAA 调用，80ms 一次太浪费
fn wake_throttled() {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    thread_local! {
        static LAST: RefCell<Option<Instant>> = const { RefCell::new(None) };
    }
    LAST.with(|slot| {
        if slot.borrow().map(|t| t.elapsed() < Duration::from_millis(2000)).unwrap_or(false) {
            return;
        }
        *slot.borrow_mut() = Some(Instant::now());
        wake_accessible();
    });
}

/// 唤醒 Chromium 的无障碍树。
///
/// Chromium 只有收到辅助技术的请求才会打开无障碍树；否则 UIA 只能看到「Document」外壳，
/// TextPattern 永远不可用（Chrome/Typora 拿不到光标就是这个原因）。对前台窗口的每个
/// 子窗口问一次 OBJID_CLIENT 就能触发，成本极低，且不需要注入。
pub fn wake_accessible() {
    use windows::core::{BOOL, GUID, Interface};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW, GetForegroundWindow};

    const IID_IACCESSIBLE: GUID = GUID::from_u128(0x618736E0_3C3D_11CF_810C_00AA00389B71);
    const OBJID_CLIENT: u32 = 0xFFFF_FFFC;
    const MAX: usize = 16;
    static mut CH: [isize; MAX] = [0; MAX];
    static mut N: usize = 0;

    unsafe extern "system" fn collect(hwnd: HWND, _: windows::Win32::Foundation::LPARAM) -> BOOL {
        unsafe {
            if N < MAX {
                CH[N] = hwnd.0 as isize;
                N += 1;
            }
        }
        BOOL(1)
    }

    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return;
        }
        let mut class = [0u16; 128];
        let len = GetClassNameW(fg, &mut class);
        let cls = String::from_utf16_lossy(&class[..len.max(0) as usize]);
        if !cls.contains("Chrome_WidgetWin") && !cls.contains("Qt") {
            return;
        }
        N = 0;
        let _ = EnumChildWindows(Some(fg), Some(collect), windows::Win32::Foundation::LPARAM(0));
        for i in 0..N {
            let child = HWND(CH[i] as *mut core::ffi::c_void);
            let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
            if AccessibleObjectFromWindow(child, OBJID_CLIENT, &IID_IACCESSIBLE, &mut raw).is_ok()
                && !raw.is_null()
            {
                let _ = IAccessible::from_raw(raw);
            }
        }
    }
}

/// 分步追踪 UIA 取光标的过程（--diag 用，定位卡在哪一步）
pub fn caret_trace() -> String {
    let mut log = String::new();
    let Some(auto) = automation() else {
        return "UIA 实例创建失败".into();
    };
    let Some(tp2) = cached_pattern::<IUIAutomationTextPattern2>(&auto, UIA_TextPattern2Id) else {
        log.push_str("TextPattern2 缓存模式不可用; ");
        return log;
    };
    log.push_str("TextPattern2 可用; ");
    unsafe {
        let mut active = windows::core::BOOL(0);
        match tp2.GetCaretRange(&mut active) {
            Ok(range) => {
                log.push_str(&format!("GetCaretRange ok active={}; ", active.as_bool()));
                let _ = range.ExpandToEnclosingUnit(TextUnit_Character);
                match range.GetBoundingRectangles() {
                    Ok(sa) if !sa.is_null() => {
                        let r = first_rect(sa);
                        log.push_str(&format!("char 展开 rect={:?}; ", r));
                    }
                    _ => log.push_str("char 展开无包围盒; "),
                }
            }
            Err(e) => log.push_str(&format!("GetCaretRange 失败({e}); ")),
        }
        if let Some(tp) = cached_pattern::<IUIAutomationTextPattern>(&auto, UIA_TextPatternId) {
            match tp.GetSelection() {
                Ok(ranges) => match ranges.Length() {
                    Ok(len) => {
                        log.push_str(&format!("TextPattern 选区数={len}; "));
                        if len > 0 {
                            if let Ok(r) = ranges.GetElement(len - 1) {
                                let _ = r.ExpandToEnclosingUnit(TextUnit_Character);
                                log.push_str(&format!("选区 char rect={:?}; ", r.GetBoundingRectangles().ok().map(|sa| !sa.is_null())));
                            }
                        }
                    }
                    Err(e) => log.push_str(&format!("Length 失败({e}); ")),
                },
                Err(e) => log.push_str(&format!("GetSelection 失败({e}); ")),
            }
        } else {
            log.push_str("TextPattern 缓存模式不可用; ");
        }
    }
    log
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
