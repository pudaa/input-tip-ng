//! 设置窗口：单窗口 + 左侧导航，全部 Direct2D 自绘。
//!
//! 为什么自绘而不是原生控件：原生控件的 focus 虚线框、黑色边框、默认间距很难压住，
//! 而且高 DPI 下控件尺寸不跟手。自绘只依赖 Direct2D / DirectWrite，与提示牌同一套渲染路径。

mod view;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect,
    GetMessageW, GetWindowLongPtrW, MSG, PostQuitMessage, RegisterClassW, SetWindowLongPtrW,
    ShowWindow, TranslateMessage, WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_PAINT, WM_SIZE, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

pub const CLASS_NAME: &str = "InputTipNgSettings";

pub(crate) struct State {
    pub(crate) page: usize,
    pub(crate) cfg: crate::config::AppConfig,
    pub(crate) dragging: Option<usize>,
    pub(crate) dpi: u32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        match msg {
            WM_PAINT => {
                if !state_ptr.is_null() {
                    view::paint(hwnd, &mut *state_ptr);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
                if !state_ptr.is_null() {
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let pressed = msg == WM_LBUTTONDOWN;
                    let released = msg == WM_LBUTTONUP;
                    if view::on_mouse(hwnd, &mut *state_ptr, x, y, msg, pressed, released) {
                        return LRESULT(0);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_SIZE => {
                if !state_ptr.is_null() {
                    let _ = view::invalidate(hwnd, &*state_ptr);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// 打开设置窗口（阻塞到窗口关闭）
pub fn run() -> i32 {
    unsafe {
        let instance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => return 1,
        };
        let class = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: PCWSTR::from_raw(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let title = wide("input-tip-ng 设置");
        let hwnd = match CreateWindowExW(
            WS_EX_APPWINDOW,
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            120,
            760,
            560,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return 1,
        };

        // Win11：圆角 + 去掉系统描边，和提示牌保持一致的扁平观感
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            4,
        );

        let state = Box::new(State {
            page: 0,
            cfg: crate::config::AppConfig::load(),
            dragging: None,
            dpi: 96,
        });
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);

        let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    0
}

/// 给 view 用：窗口客户区尺寸
pub(crate) fn client_size(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        (rc.right - rc.left, rc.bottom - rc.top)
    }
}

pub(crate) fn dpi_of(hwnd: HWND) -> u32 {
    unsafe {
        let d = GetDpiForWindow(hwnd);
        if d == 0 { 96 } else { d }
    }
}
