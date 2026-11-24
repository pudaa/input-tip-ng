//! 注入到目标进程的小钩子。
//!
//! 宿主用 CreateRemoteThread 让目标进程 LoadLibrary 这个 DLL，再调用导出函数
//! ITNGStart(hwnd)。我们直接把目标窗口过程换成自己的（SetWindowLongPtr 子类化）：
//! 宿主往该窗口发私有消息时，回调在**目标 UI 线程**里执行 GetFocus + GetCaretPos，
//! 把光标矩形写进共享内存，宿主读走即可。
//!
//! 只读光标位置，不改变目标进程的任何业务状态。

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GetCaretPos, GetGUIThreadInfo, GUITHREADINFO, GWLP_WNDPROC, SetWindowLongPtrW,
};

/// 宿主的查询消息号
pub const HOOK_QUERY_MSG: u32 = 0x8000 + 0x51;

/// 共享内存布局：序号 + 左/上/宽/高
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CaretShared {
    pub seq: u32,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

static mut VIEW: *mut CaretShared = std::ptr::null_mut();
static mut OLD_PROC: isize = 0;
static mut TARGET: isize = 0;
static READY: AtomicIsize = AtomicIsize::new(0);
static CALLS: AtomicIsize = AtomicIsize::new(0);

fn shared() -> Option<&'static mut CaretShared> {
    unsafe {
        if !VIEW.is_null() {
            return Some(&mut *VIEW);
        }
        let name = windows::core::HSTRING::from("Local\\input-tip-ng-caret");
        let mut view: *mut CaretShared = std::ptr::null_mut();
        if let Ok(handle) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &name) {
            let p = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if !p.Value.is_null() {
                view = p.Value as *mut CaretShared;
            }
        }
        if view.is_null() {
            if let Ok(handle) = CreateFileMappingW(
                HANDLE::default(),
                None,
                PAGE_READWRITE,
                0,
                std::mem::size_of::<CaretShared>() as u32,
                &name,
            ) {
                let p = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
                if !p.Value.is_null() {
                    view = p.Value as *mut CaretShared;
                }
            }
        }
        if view.is_null() {
            return None;
        }
        VIEW = view;
        Some(&mut *view)
    }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == HOOK_QUERY_MSG {
        CALLS.fetch_add(1, Ordering::SeqCst);
        let seq = wparam.0 as u32;
        if let Some(shared) = shared() {
            shared.seq = seq;
            shared.width = -1; // 先标记「回调跑到了」
            shared.left = 0;
            shared.top = 0;
            shared.height = 0;
            let mut pt = POINT::default();
            let focus = unsafe { GetFocus() };
            if unsafe { GetCaretPos(&mut pt) }.is_ok() && !focus.0.is_null() {
                let mut screen = pt;
                let _ = unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(focus, &mut screen) };
                let mut gti = GUITHREADINFO {
                    cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                    ..Default::default()
                };
                let _ = unsafe {
                    GetGUIThreadInfo(
                        windows::Win32::System::Threading::GetCurrentThreadId(),
                        &mut gti,
                    )
                };
                let height = (gti.rcCaret.bottom - gti.rcCaret.top).max(8);
                shared.left = screen.x;
                shared.top = screen.y;
                shared.width = 2;
                shared.height = height;
            }
        }
        return LRESULT(1);
    }
    unsafe {
        let old = std::mem::transmute::<isize, windows::Win32::UI::WindowsAndMessaging::WNDPROC>(OLD_PROC);
        CallWindowProcW(old, hwnd, msg, wparam, lparam)
    }
}

/// 宿主注入后调用：子类化目标窗口
#[unsafe(no_mangle)]
pub extern "system" fn ITNGStart(hwnd_value: usize) -> i32 {
    unsafe {
        if let Some(shared) = shared() {
            shared.seq = 0xFFFF_FFFF; // 标记：DLL 已执行到这里、共享内存可用
            shared.width = 0;
        }
        let hwnd = HWND(hwnd_value as *mut c_void);
        if hwnd.0.is_null() {
            return -1;
        }
        // 已经子类化过就别再来一次（否则 OLD_PROC 指向自己，普通消息会无限递归）
        if OLD_PROC != 0 && TARGET == hwnd_value as isize {
            return 0;
        }
        TARGET = hwnd_value as isize;
        let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass_proc as usize as isize);
        if old == 0 {
            return -2;
        }
        OLD_PROC = old;
        READY.store(1, Ordering::SeqCst);
        0
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn ITNGStop() -> i32 {
    unsafe {
        if TARGET != 0 && OLD_PROC != 0 {
            let hwnd = HWND(TARGET as *mut c_void);
            let _ = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, OLD_PROC);
            OLD_PROC = 0;
            TARGET = 0;
        }
        0
    }
}

#[allow(non_camel_case_types)]
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_hinst: windows::Win32::Foundation::HINSTANCE, _reason: u32, _reserved: usize) -> i32 {
    let _ = READY.load(Ordering::SeqCst);
    1
}
