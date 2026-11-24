//! IAccessible2 文本接口取光标——浏览器 / Electron / Qt 的真正主力。
//!
//! Chromium（Chrome、Edge、VS Code 等 Electron 应用）和 Qt 的光标是各自渲染出来的，
//! Win32 的 GetCaretPos 在它们的进程里根本读不到；UIA 的 TextPattern 在这些程序里
//! 常常「能拿到范围、拿不到包围盒」。它们都实现了 IAccessible2 的 IAccessibleText，
//! 读屏软件正是靠 caretOffset + characterExtents 拿光标屏幕坐标的。
//!
//! Windows SDK 不提供 IA2 头文件，这里按 IA2 规范（ia2_api_all.idl）的 vtable 顺序手写。
//! 只调用只读方法，不改目标进程状态。

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
};

/// MSAA：从窗口取可访问对象
const IID_IACCESSIBLE: GUID = GUID::from_u128(0x618736E0_3C3D_11CF_810C_00AA00389B71);
/// IA2 规范的 IAccessibleText（注意最后一段是 89C0FB4B，不是早期草案的 0231A2D3）
const IID_IACCESSIBLE_TEXT: GUID = GUID::from_u128(0x24FD2FFB_3AAD_4A08_8335_A3AD89C0FB4B);
const OBJID_CLIENT: u32 = 0xFFFF_FFFC;
/// IA2CoordinateType::IA2_COORDTYPE_SCREEN_RELATIVE
const COORD_SCREEN: i32 = 0;

type QiFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
type RefFn = unsafe extern "system" fn(*mut c_void) -> u32;
type CaretOffsetFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT;
type CharExtentsFn =
    unsafe extern "system" fn(*mut c_void, i32, i32, *mut i32, *mut i32, *mut i32, *mut i32) -> HRESULT;

/// IAccessibleText 的 vtable：IUnknown 三槽 + 规范里按声明顺序排的方法。
/// 只用到 caretOffset（槽 5）与 characterExtents（槽 6），中间的 addSelection /
/// attributes 只为对齐偏移，保留原始指针、永不调用。
#[repr(C)]
struct AccessibleTextVtbl {
    query_interface: QiFn,
    add_ref: RefFn,
    release: RefFn,
    _add_selection: *const c_void,
    _attributes: *const c_void,
    get_caret_offset: CaretOffsetFn,
    get_character_extents: CharExtentsFn,
}

/// 最小可用的 IAccessibleText 包装：生命周期由 IUnknown 的引用计数管理
#[repr(transparent)]
#[derive(Clone)]
struct AccessibleText(IUnknown);

unsafe impl Interface for AccessibleText {
    type Vtable = AccessibleTextVtbl;
    const IID: GUID = IID_IACCESSIBLE_TEXT;
}

impl AccessibleText {
    fn vtable(&self) -> &AccessibleTextVtbl {
        unsafe {
            let raw = self.0.as_raw();
            &**(raw as *mut *mut AccessibleTextVtbl)
        }
    }

    /// 光标在文本里的偏移；-1 / 失败表示光标不在这个对象上
    fn caret_offset(&self) -> Option<i32> {
        let (hr, offset) = self.raw_caret_offset();
        (hr.is_ok() && offset >= 0).then_some(offset)
    }

    /// 原始结果（HRESULT, offset），诊断用
    fn raw_caret_offset(&self) -> (HRESULT, i32) {
        unsafe {
            let mut offset = -1i32;
            let hr = (self.vtable().get_caret_offset)(self.0.as_raw(), &mut offset);
            (hr, offset)
        }
    }

    /// 第 offset 个字符在屏幕上的矩形（文末的虚拟字符也可以问）
    fn char_rect(&self, offset: i32) -> Option<RECT> {
        let (hr, x, y, w, h) = self.raw_extents(offset);
        if !hr.is_ok() || h <= 0 || w < 0 {
            return None;
        }
        Some(RECT { left: x, top: y, right: x + 2, bottom: y + h })
    }

    /// 原始结果（HRESULT 与四个分量），诊断用
    fn raw_extents(&self, offset: i32) -> (HRESULT, i32, i32, i32, i32) {
        unsafe {
            let (mut x, mut y, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
            let hr = (self.vtable().get_character_extents)(
                self.0.as_raw(),
                offset,
                COORD_SCREEN,
                &mut x,
                &mut y,
                &mut w,
                &mut h,
            );
            (hr, x, y, w, h)
        }
    }

    /// 光标矩形：取光标后第一个字符的左缘；文末取前一个字符的右缘
    fn caret_rect(&self) -> Option<RECT> {
        let offset = self.caret_offset()?;
        if let Some(r) = self.char_rect(offset) {
            return Some(r);
        }
        let prev = self.char_rect(offset - 1)?;
        Some(RECT { left: prev.right, top: prev.top, right: prev.right + 2, bottom: prev.bottom })
    }
}

fn accessible_of(hwnd: HWND) -> Option<IAccessible> {
    unsafe {
        if hwnd.0.is_null() {
            return None;
        }
        let mut raw: *mut c_void = std::ptr::null_mut();
        if AccessibleObjectFromWindow(hwnd, OBJID_CLIENT, &IID_IACCESSIBLE, &mut raw).is_err() {
            return None;
        }
        if raw.is_null() {
            return None;
        }
        Some(IAccessible::from_raw(raw))
    }
}

/// 从 accFocus 的 VARIANT 里取焦点对象（VT_DISPATCH / VT_UNKNOWN 两种常见形态）
fn focus_object(v: &VARIANT) -> Option<IAccessible> {
    unsafe {
        match v.Anonymous.Anonymous.vt.0 {
            9 => {
                // VT_DISPATCH
                let disp: &Option<windows::Win32::System::Com::IDispatch> =
                    &v.Anonymous.Anonymous.Anonymous.pdispVal;
                disp.as_ref()?.cast::<IAccessible>().ok()
            }
            13 => {
                // VT_UNKNOWN
                let unk: &Option<IUnknown> = &v.Anonymous.Anonymous.Anonymous.punkVal;
                unk.as_ref()?.cast::<IAccessible>().ok()
            }
            _ => None,
        }
    }
}

/// 在一个可访问对象上找可用的文本接口：先问对象本身，再问它的焦点元素
fn text_with_caret(acc: &IAccessible) -> Option<AccessibleText> {
    if let Ok(text) = acc.cast::<AccessibleText>() {
        if text.caret_offset().is_some() {
            return Some(text);
        }
    }
    unsafe {
        let v = acc.accFocus().ok()?;
        let focus = focus_object(&v)?;
        let text = focus.cast::<AccessibleText>().ok()?;
        text.caret_offset().is_some().then_some(text)
    }
}

const CHILD_MAX: usize = 16;
static mut CHILDREN: [isize; CHILD_MAX] = [0; CHILD_MAX];
static mut CHILD_COUNT: usize = 0;

unsafe extern "system" fn collect(hwnd: HWND, _: LPARAM) -> BOOL {
    unsafe {
        if CHILD_COUNT < CHILD_MAX {
            CHILDREN[CHILD_COUNT] = hwnd.0 as isize;
            CHILD_COUNT += 1;
        }
    }
    BOOL(1)
}

fn class_of(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// 候选窗口：前台线程的焦点窗口 → 前台窗口 → Chromium/Electron 的渲染子窗口
fn candidates(fg: HWND) -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    let mut push = |h: HWND, out: &mut Vec<HWND>| {
        if !h.0.is_null() && !out.iter().any(|x| x.0 == h.0) {
            out.push(h);
        }
    };
    if let Some((focus, _)) = super::gti::caret() {
        push(focus, &mut out);
    }
    push(super::gti::focus_window(), &mut out);
    push(fg, &mut out);

    unsafe {
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        CHILD_COUNT = 0;
        let _ = EnumChildWindows(Some(fg), Some(collect), LPARAM(0));
        for i in 0..CHILD_COUNT {
            let child = HWND(CHILDREN[i] as *mut c_void);
            let mut pid = 0u32;
            GetWindowThreadProcessId(child, Some(&mut pid));
            if pid != fg_pid {
                continue;
            }
            let cls = class_of(child);
            // 可访问对象通常挂在渲染子窗口上；Qt 也会创建中间窗口
            if cls.contains("Chrome_RenderWidgetHost") || cls.starts_with("Qt") {
                push(child, &mut out);
            }
        }
    }
    out
}

struct Cached {
    fg: isize,
    at: Instant,
    text: Option<AccessibleText>,
}

thread_local! {
    static CACHE: RefCell<Option<Cached>> = const { RefCell::new(None) };
}

const CACHE_TTL: Duration = Duration::from_millis(400);

fn resolve(fg: HWND) -> Option<AccessibleText> {
    for hwnd in candidates(fg) {
        if let Some(acc) = accessible_of(hwnd) {
            if let Some(text) = text_with_caret(&acc) {
                return Some(text);
            }
        }
    }
    None
}

/// 取光标屏幕坐标。失败返回 None，由上层继续降级。
pub fn caret() -> Option<RECT> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return None;
    }
    let key = fg.0 as isize;

    // 1. 先用缓存的对象（焦点在同一个窗口内移动时，对象依然有效）
    let cached = CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|c| c.fg == key && c.at.elapsed() < CACHE_TTL)
            .and_then(|c| c.text.clone())
    });
    if let Some(text) = &cached {
        if let Some(rect) = text.caret_rect() {
            return Some(rect);
        }
    }

    // 2. 缓存失效（换窗口 / 换了焦点元素）：重新解析一次
    let resolved = resolve(fg);
    let rect = resolved.as_ref().and_then(|t| t.caret_rect());
    CACHE.with(|slot| {
        *slot.borrow_mut() = Some(Cached { fg: key, at: Instant::now(), text: resolved });
    });
    rect
}

/// 诊断：说明解析到了哪个窗口 / 对象、光标偏移与矩形
pub fn probe() -> String {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return "前台窗口为空".into();
    }
    probe_at(fg)
}

/// 对指定顶层窗口做同一套诊断（不要求它在最前，--ia2-test 用）
pub fn probe_at(fg: HWND) -> String {
    let c = candidates(fg);
    let mut out = String::new();
    for hwnd in c {
        let cls = class_of(hwnd);
        let acc = accessible_of(hwnd);
        match acc {
            Some(acc) => {
                let direct = acc.cast::<AccessibleText>();
                let direct_raw = direct.as_ref().map(|t| t.raw_caret_offset());
                let direct_ext = direct.as_ref().map(|t| t.raw_extents(0));
                let focus_vt = unsafe {
                    acc.accFocus().ok().map(|v| unsafe { v.Anonymous.Anonymous.vt.0 })
                };
                let focus = unsafe { acc.accFocus().ok().and_then(|v| focus_object(&v)) };
                let focus_cast = focus.as_ref().map(|f| f.cast::<AccessibleText>().is_ok());
                let focus_text = focus.as_ref().and_then(|f| f.cast::<AccessibleText>().ok());
                let focus_raw = focus_text.as_ref().map(|t| t.raw_caret_offset());
                let rect = focus_text.as_ref().or(direct.as_ref().ok()).and_then(|t| t.caret_rect());
                out.push_str(&format!(
                    "hwnd={} class={cls} 对象=可访问 直QI={:?} 直接={direct_raw:?} ext0={direct_ext:?} 焦点vt={focus_vt:?} 焦点QI={focus_cast:?} 焦点={focus_raw:?} rect={rect:?}; ",
                    hwnd.0 as isize,
                    direct.as_ref().map(|_| ()),
                ));
            }
            None => {
                out.push_str(&format!("hwnd={} class={cls} 对象=无; ", hwnd.0 as isize));
            }
        }
    }
    out
}
