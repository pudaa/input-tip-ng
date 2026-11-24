//! 常驻进程外壳：单实例 + 隐藏窗口 + 消息循环。
//!
//! 当前 P0 阶段只提供「活着」与「可被 --exit 关闭」的最小骨架；
//! 托盘、状态提示、规则引擎在后续阶段接入同一个窗口与消息循环。

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, WINDOW_EX_STYLE,
    WM_CLOSE, WM_DESTROY, WM_TIMER, WNDCLASSW, WS_POPUP,
};

use crate::log;

const WINDOW_CLASS: &str = "InputTipNgResidentWindow";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 请求已运行实例退出；返回是否找到实例
pub fn request_exit() -> bool {
    let class = wide(WINDOW_CLASS);
    unsafe {
        match FindWindowW(PCWSTR::from_raw(class.as_ptr()), PCWSTR::null()) {
            Ok(hwnd) if !hwnd.0.is_null() => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                true
            }
            _ => false,
        }
    }
}

use windows::core::PCWSTR;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            log::write("app", "debug", "心跳");
            LRESULT(0)
        }
        WM_CLOSE => {
            log::info("app", "收到退出请求");
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 启动常驻进程；已有实例时返回 0（自启与手工启动并存不该报错）
pub fn run(autostarted: bool) -> i32 {
    let _ = log::ensure_dirs();
    log::info(
        "app",
        &format!(
            "启动 input-tip-ng v{}（{}）",
            env!("CARGO_PKG_VERSION"),
            if autostarted { "自启" } else { "手工" }
        ),
    );

    let mutex_name = wide("Local\\input-tip-ng-singleton");

    unsafe {
        let mutex = match CreateMutexW(None, false, PCWSTR::from_raw(mutex_name.as_ptr())) {
            Ok(h) => h,
            Err(e) => {
                log::error("app", &format!("创建单实例互斥体失败: {e}"));
                return 1;
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log::info("app", "已有实例在运行，本次启动退出");
            let _ = CloseHandle(mutex);
            return 0;
        }

        let instance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                log::error("app", &format!("获取模块句柄失败: {e}"));
                return 1;
            }
        };

        let class = wide(WINDOW_CLASS);
        let title = wide("input-tip-ng");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: PCWSTR::from_raw(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            log::error("app", "注册窗口类失败");
            return 1;
        }

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::error("app", &format!("创建窗口失败: {e}"));
                return 1;
            }
        };

        let _ = SetTimer(Some(hwnd), 1, 5 * 60 * 1000, None);
        log::info("app", "常驻进程已就绪");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        log::info("app", "退出");
        let _ = CloseHandle(mutex);
    }
    0
}
