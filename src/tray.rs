//! 托盘图标与右键菜单。
//!
//! 用 Shell_NotifyIcon 而不是自绘窗口：任务栏溢出区、DPI 缩放、深浅色主题都由系统处理。
//! 左键/右键都弹菜单（旧版左键直接切暂停，容易误触，这里统一成菜单）。

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_GUID, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW, MF_SEPARATOR,
    MF_STRING, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
};

/// 托盘回调消息号
pub const WM_TRAY: u32 = WM_APP + 1;

const TRAY_ID: u32 = 1;

/// 固定 GUID：程序重启（甚至异常退出留下"幽灵图标"）时，Shell 会用同 GUID 的新图标替换旧图标，
/// 不会在托盘里堆积一排点了没反应的死图标。
const TRAY_GUID: GUID = GUID::from_u128(0x9F1C2D3A_5B4E_4C7D_8A90_1B2C3D4E5F60);

pub enum MenuAction {
    TogglePause,
    OpenSettings,
    OpenConfig,
    OpenLogs,
    Exit,
    None,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    // 优先用内嵌的应用图标（32px，任务栏通知区常见尺寸）；失败再退到系统默认图标
    if let Some(icon) = crate::icon::load(32) {
        return icon;
    }
    unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() }
}

fn make_data(hwnd: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_GUID,
        uCallbackMessage: WM_TRAY,
        hIcon: icon(),
        guidItem: TRAY_GUID,
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
        let settings = wide("设置…");
        let open_cfg = wide("打开配置文件");
        let open_log = wide("打开日志目录");
        let quit = wide("退出");

        let _ = AppendMenuW(menu, MF_STRING, 1, PCWSTR::from_raw(pause.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, 5, PCWSTR::from_raw(settings.as_ptr()));
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
        // 标准做法：菜单关闭后补一条 WM_NULL，避免下次点托盘时前台激活状态不对
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(hwnd),
            windows::Win32::UI::WindowsAndMessaging::WM_NULL,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(0),
        );

        match cmd.0 {
            1 => MenuAction::TogglePause,
            2 => MenuAction::OpenConfig,
            3 => MenuAction::OpenLogs,
            4 => MenuAction::Exit,
            5 => MenuAction::OpenSettings,
            _ => MenuAction::None,
        }
    }
}
