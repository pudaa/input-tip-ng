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
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE,
};
use windows::Win32::UI::Input::Ime::{
    COMPOSITIONFORM, GCS_COMPSTR, ImmGetCompositionStringW, ImmGetCompositionWindow,
    ImmGetContext, ImmReleaseContext,
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

/// 中文组合输入期间，Chromium/Qt 的渲染光标没有 Win32 光标可读，
/// 但输入法上下文里的 COMPOSITIONFORM.ptCurrentPos 就是「正在组字的那个点」。
/// 这个接口是线程私有的，必须在本进程的 UI 线程里调——正是注入之后的位置。
unsafe fn ime_composition_caret() -> Option<(i32, i32, i32)> {
    unsafe {
        let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let tid = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, None);
        if tid != windows::Win32::System::Threading::GetCurrentThreadId() {
            return None;
        }
        let himc = ImmGetContext(fg);
        if himc.0.is_null() {
            return None;
        }
        // 只有正在组字（有临时组合串）才采信这个位置，否则它是上一次的陈旧值
        let len = ImmGetCompositionStringW(himc, GCS_COMPSTR, None, 0);
        let mut out = None;
        if len > 0 {
            let mut cf = COMPOSITIONFORM::default();
            if ImmGetCompositionWindow(himc, &mut cf).as_bool() {
                let mut pt = cf.ptCurrentPos;
                if ClientToScreen(fg, &mut pt).as_bool() {
                    out = Some((pt.x, pt.y, 20));
                }
            }
        }
        let _ = ImmReleaseContext(fg, himc);
        out
    }
}

/// 在当前线程里取光标屏幕坐标（本函数运行在目标 UI 线程，GetCaretPos 才有意义）。
///
/// 先问 GetGUIThreadInfo：它给 hwndCaret + 客户区矩形，不依赖 GetFocus
/// （Chromium/Electron 的焦点在渲染子窗口上，GetFocus 经常为空，之前就是被这条挡掉的）；
/// 再退回 GetCaretPos，最后用 GetFocus 换算。
unsafe fn caret_screen() -> Option<(i32, i32, i32)> {
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(
            windows::Win32::System::Threading::GetCurrentThreadId(),
            &mut gti,
        )
        .is_ok()
            && !gti.hwndCaret.0.is_null()
        {
            let mut pt = POINT { x: gti.rcCaret.left, y: gti.rcCaret.top };
            // (0,0) 且没有高度：多数是「有 caret 窗口但没有真光标」的脏值，
            // 直接当光标会把提示牌吸到窗口左上角
            let empty = gti.rcCaret.left == 0 && gti.rcCaret.top == 0 && gti.rcCaret.bottom == 0;
            if !empty && ClientToScreen(gti.hwndCaret, &mut pt).as_bool() {
                let height = (gti.rcCaret.bottom - gti.rcCaret.top).max(8);
                return Some((pt.x, pt.y, height));
            }
        }
        // Chromium 这类程序的光标可能挂在前台窗口所在线程（不一定等于当前线程）
        let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if !fg.0.is_null() {
            let fg_tid = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, None);
            if fg_tid != 0 && fg_tid != windows::Win32::System::Threading::GetCurrentThreadId() {
                let mut gti2 = GUITHREADINFO {
                    cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                    ..Default::default()
                };
                if GetGUIThreadInfo(fg_tid, &mut gti2).is_ok() && !gti2.hwndCaret.0.is_null() {
                    let mut pt = POINT { x: gti2.rcCaret.left, y: gti2.rcCaret.top };
                    if ClientToScreen(gti2.hwndCaret, &mut pt).as_bool() {
                        let height = (gti2.rcCaret.bottom - gti2.rcCaret.top).max(8);
                        return Some((pt.x, pt.y, height));
                    }
                }
            }
        }
        let mut pt = POINT::default();
        if GetCaretPos(&mut pt).is_ok() && (pt.x != 0 || pt.y != 0) {
            // Chromium 的 UI 线程上 GetCaretPos 会「成功」返回 (0,0)，那不是光标
            let focus = GetFocus();
            if !focus.0.is_null() {
                let mut screen = pt;
                if ClientToScreen(focus, &mut screen).as_bool() {
                    return Some((screen.x, screen.y, 8));
                }
            }
        }
        // 最后才用输入法组合窗口：它是「组字点」的近似，不是真光标
        ime_composition_caret()
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
            if let Some((x, y, height)) = caret_screen() {
                shared.left = x;
                shared.top = y;
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
