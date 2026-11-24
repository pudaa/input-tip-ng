//! 第三级：MSAA 的 caret 对象（AccessibleObjectFromWindow + OBJID_CARET）。
//!
//! Chromium/Electron 的 caret 对象挂在子窗口（Chrome_RenderWidgetHostHWND）上，
//! 所以按「焦点窗口 → 顶层窗口 → 子窗口」的顺序都问一遍。

use windows::core::{GUID, Interface};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetForegroundWindow, GetWindowThreadProcessId,
};

const IID_IACCESSIBLE: GUID = GUID::from_u128(0x618736E0_3C3D_11CF_810C_00AA00389B71);
const OBJID_CARET: u32 = 0xFFFF_FFF8;

fn caret_of(hwnd: HWND) -> Option<RECT> {
    unsafe {
        if hwnd.0.is_null() {
            return None;
        }
        let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
        AccessibleObjectFromWindow(hwnd, OBJID_CARET, &IID_IACCESSIBLE, &mut raw).ok()?;
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

const CHILD_MAX: usize = 24;
static mut CHILDREN: [isize; CHILD_MAX] = [0; CHILD_MAX];
static mut CHILD_COUNT: usize = 0;

unsafe extern "system" fn collect(hwnd: HWND, _: windows::Win32::Foundation::LPARAM) -> windows::core::BOOL {
    unsafe {
        if CHILD_COUNT < CHILD_MAX {
            CHILDREN[CHILD_COUNT] = hwnd.0 as isize;
            CHILD_COUNT += 1;
        }
    }
    windows::core::BOOL(1)
}

pub fn caret() -> Option<RECT> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        if let Some(r) = caret_of(super::gti::focus_window()) {
            return Some(r);
        }
        if let Some(r) = caret_of(fg) {
            return Some(r);
        }

        CHILD_COUNT = 0;
        let _ = EnumChildWindows(Some(fg), Some(collect), windows::Win32::Foundation::LPARAM(0));
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        for i in 0..CHILD_COUNT {
            let h = HWND(CHILDREN[i] as *mut core::ffi::c_void);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid != fg_pid {
                continue;
            }
            if let Some(r) = caret_of(h) {
                return Some(r);
            }
        }
    }
    None
}
