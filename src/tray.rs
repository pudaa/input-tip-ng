//! 托盘图标与右键菜单。
//!
//! 用 Shell_NotifyIcon 而不是自绘窗口：任务栏溢出区、DPI 缩放、深浅色主题都由系统处理。
//! 左键/右键都弹菜单（旧版左键直接切暂停，容易误触，这里统一成菜单）。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW, MF_SEPARATOR,
    MF_STRING, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
};

/// 托盘回调消息号
pub const WM_TRAY: u32 = WM_APP + 1;

const TRAY_ID: u32 = 1;

pub enum MenuAction {
    TogglePause,
    OpenConfig,
    OpenLogs,
    Exit,
    None,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() }
}

fn make_data(hwnd: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon(),
        ..Default::default()
    };
    let tip16 = wide(tip);
    let n = tip16.len().min(data.szTip.len());
    data.szTip[..n].copy_from_slice(&tip16[..n]);
    data
}

pub fn add(hwnd: HWND, tip: &str) -> bool {
    let data = make_data(hwnd, tip);
    unsafe { Shell_NotifyIconW(NIM_ADD, &data).as_bool() }
}

pub fn update_tip(hwnd: HWND, tip: &str) -> bool {
    let data = make_data(hwnd, tip);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() }
}

pub fn remove(hwnd: HWND) {
    let data = make_data(hwnd, "");
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// 在光标处弹出菜单并返回用户选择
pub fn popup_menu(hwnd: HWND, paused: bool) -> MenuAction {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return MenuAction::None;
        };
        let pause_text = if paused { "恢复提示" } else { "暂停提示" };
        let pause = wide(pause_text);
        let open_cfg = wide("打开配置");
        let open_log = wide("打开日志目录");
        let quit = wide("退出");

        let _ = AppendMenuW(menu, MF_STRING, 1, PCWSTR::from_raw(pause.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 2, PCWSTR::from_raw(open_cfg.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, 3, PCWSTR::from_raw(open_log.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 4, PCWSTR::from_raw(quit.as_ptr()));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // 必须先置前台，否则菜单点外面不消失（Windows 的老毛病）
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 {
            1 => MenuAction::TogglePause,
            2 => MenuAction::OpenConfig,
            3 => MenuAction::OpenLogs,
            4 => MenuAction::Exit,
            _ => MenuAction::None,
        }
    }
}
