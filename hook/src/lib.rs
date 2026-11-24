//! 注入到目标进程的小钩子。
//!
//! 做法：宿主用 CreateRemoteThread 让目标进程 LoadLibrary 这个 DLL，再调用导出函数
//! ITNGStart(hwnd)；它在目标进程里装一个 WH_CALLWNDPROC 钩子。宿主往 hwnd 发一条
//! 私有消息（wParam = 序号），钩子在**目标 UI 线程**里执行 GetFocus + GetCaretPos，
//! 把光标矩形写进共享内存；宿主读共享内存即可。
//!
//! 只读光标位置，不改变目标进程任何状态。

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleExW;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCaretPos, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    SetWindowsHookExW, WH_CALLWNDPROC,
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

static SHARED_NAME: windows::core::HSTRING = windows::core::HSTRING::new();
static mut VIEW: *mut CaretShared = std::ptr::null_mut();
static HINSTANCE_SELF: AtomicIsize = AtomicIsize::new(0);
static ATTACHED: AtomicUsize = AtomicUsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);

fn shared() -> Option<&'static mut CaretShared> {
    unsafe {
        if !VIEW.is_null() {
            return Some(&mut *VIEW);
        }
        let name = windows::core::HSTRING::from("Local\\input-tip-ng-caret");
        let mut view: *mut CaretShared = std::ptr::null_mut();
        // 先尝试打开宿主创建的映射，没有就自己建一个
        if let Ok(handle) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &name) {
            let p = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if !p.Value.is_null() {
                view = p.Value as *mut CaretShared;
            }
        }
        if view.is_null() {
            if let Ok(handle) = CreateFileMappingW(
                windows::Win32::Foundation::HANDLE::default(),
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
        let _ = &SHARED_NAME;
        Some(&mut *view)
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        unsafe {
            let msg = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CWPSTRUCT);
            if msg.message == HOOK_QUERY_MSG {
                let seq = wparam.0 as u32;
                if let Some(shared) = shared() {
                    let mut pt = POINT::default();
                    let focus = GetFocus();
                    if GetCaretPos(&mut pt).is_ok() && !focus.0.is_null() {
                        let mut screen = pt;
                        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(focus, &mut screen);
                        let mut gti = GUITHREADINFO {
                            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                            ..Default::default()
                        };
                        let _ = GetGUIThreadInfo(GetCurrentThreadId(), &mut gti);
                        let height = (gti.rcCaret.bottom - gti.rcCaret.top).max(8);
                        shared.seq = seq;
                        shared.left = screen.x;
                        shared.top = screen.y;
                        shared.width = 2;
                        shared.height = height;
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 宿主注入后调用：在目标进程里装钩子
#[unsafe(no_mangle)]
pub extern "system" fn ITNGStart(hwnd_value: usize) -> i32 {
    unsafe {
        // 拿到本 DLL 的模块句柄（装钩子需要）
        let self_addr = ITNGStart as usize as *const u16;
        let mut hmod = windows::Win32::Foundation::HMODULE::default();
        let _ = GetModuleHandleExW(0x4 /* FROM_ADDRESS */, windows::core::PCWSTR(self_addr), &mut hmod);
        let hinst = HINSTANCE(hmod.0);
        HINSTANCE_SELF.store(hinst.0 as isize, Ordering::SeqCst);

        let hwnd = HWND(hwnd_value as *mut c_void);
        let tid = GetWindowThreadProcessId(hwnd, None);
        if tid == 0 {
            return -1;
        }
        let hook = SetWindowsHookExW(WH_CALLWNDPROC, Some(hook_proc), Some(hinst), tid);
        match hook {
            Ok(h) => {
                HOOK.store(h.0 as isize, Ordering::SeqCst);
                ATTACHED.store(1, Ordering::SeqCst);
                let _ = shared();
                0
            }
            Err(_) => -2,
        }
    }
}

/// 让线程在 DLL 卸载前保持存在（钩子回调由系统在目标线程里调用，不需要额外线程）
#[unsafe(no_mangle)]
pub extern "system" fn ITNGStop() -> i32 {
    unsafe {
        let h = HOOK.load(Ordering::SeqCst);
        if h != 0 {
            let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(
                windows::Win32::UI::WindowsAndMessaging::HHOOK(h as *mut c_void),
            );
            HOOK.store(0, Ordering::SeqCst);
        }
        0
    }
}

#[allow(non_camel_case_types)]
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_hinst: HINSTANCE, reason: u32, _reserved: usize) -> i32 {
    if reason == 1 {
        // DLL_PROCESS_ATTACH：不做重活
    }
    1
}

// 引用一些常量，避免未使用警告（w! 宏在部分特性组合下需要）
#[allow(dead_code)]
fn _keep() {
    let _ = w!("input-tip-ng");
}
