//! 常驻进程：单实例 + 隐藏窗口 + 轮询循环 + 光标旁文字牌。
//!
//! 每一拍做四件事：取前台窗口 -> 白名单判断 -> 探测状态与 caret -> 决定显示/隐藏。
//! 全部在同一线程完成（一个 Mutex 保护运行时状态），不跨线程共享窗口句柄。

use std::sync::Mutex;
use std::time::Instant;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT,
    WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
    GetForegroundWindow, GetMessageW, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer,
    TranslateMessage, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_TIMER, WNDCLASSW, WS_POPUP,
};

use crate::config::{AppConfig, SymbolTheme};
use crate::state::InputState;
use crate::{caret, ime, log, process, symbol, tray};

/// TaskbarCreated：资源管理器重启后要重新把图标加回去
static mut TASKBAR_CREATED: u32 = 0;

const WINDOW_CLASS: &str = "InputTipNgResidentWindow";

struct Runtime {
    window: isize,
    symbol: isize,
    theme: SymbolTheme,
    /// 每状态一份：(状态, 文字, 底色)
    styles: Vec<(InputState, String, u32)>,
    rules: Vec<String>,
    paused: bool,
    /// 上一次的前台窗口与进程名缓存：进程名只在窗口切换时查一次（每拍都 OpenProcess 太浪费）
    last_fg: isize,
    last_proc: String,
    /// 托盘提示里显示的状态名，避免每拍都 Shell_NotifyIcon
    last_tip_state: &'static str,
    last_state: Option<InputState>,
    last_caret: Option<RECT>,
    last_show: Option<Instant>,
}

static RT: Mutex<Option<Runtime>> = Mutex::new(None);

const ALL_STATES: [InputState; 6] = [
    InputState::Cn,
    InputState::En,
    InputState::Caps,
    InputState::Us,
    InputState::Jp,
    InputState::Kr,
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hwnd_of(rt: &Runtime) -> HWND {
    HWND(rt.symbol as *mut core::ffi::c_void)
}

fn same_rect(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

/// 请求淡出：动画结束后由 symbol 内部真正隐藏
fn hide(rt: &mut Runtime) {
    if rt.last_show.is_some() {
        let _ = symbol::begin_hide(hwnd_of(rt));
    }
    rt.last_show = None;
}

/// 一拍：判断该不该显示、显示什么
fn tick() {
    let mut guard = match RT.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(rt) = guard.as_mut() else { return };

    // 先推进淡入淡出；没有动画时几乎零开销
    let _ = symbol::tick_animation(hwnd_of(rt));

    if !rt.theme.enabled || rt.paused {
        hide(rt);
        return;
    }

    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        hide(rt);
        return;
    }

    if rt.last_fg != fg.0 as isize {
        rt.last_fg = fg.0 as isize;
        rt.last_proc = process::name(fg).unwrap_or_default();
    }
    let proc_name = rt.last_proc.clone();
    let allowed =
        rt.rules.is_empty() || rt.rules.iter().any(|r| r.eq_ignore_ascii_case(&proc_name));
    if !allowed {
        hide(rt);
        rt.last_state = None;
        rt.last_caret = None;
        return;
    }

    let Some(caret_rect) = caret::rect() else {
        hide(rt);
        rt.last_state = None;
        rt.last_caret = None;
        return;
    };

    let state = ime::detect(fg);
    let changed = rt.last_state != Some(state)
        || match &rt.last_caret {
            Some(prev) => !same_rect(prev, &caret_rect),
            None => true,
        };

    let now = Instant::now();
    if changed {
        let (text, bg) = rt
            .styles
            .iter()
            .find(|(s, _, _)| *s == state)
            .map(|(_, t, b)| (t.clone(), *b))
            .unwrap_or_else(|| (state.key().to_string(), 0x808080));

        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(fg) };
        let shown = symbol::show(
            hwnd_of(rt),
            &text,
            bg,
            rt.theme.fg,
            &rt.theme.font,
            rt.theme.size,
            rt.theme.weight,
            rt.theme.opacity,
            (rt.theme.offset_x, rt.theme.offset_y),
            rt.theme.corner,
            &caret_rect,
            if dpi == 0 { 96 } else { dpi },
        );
        if let Some((x, y, w, h)) = shown {
            log::write(
                "app",
                "debug",
                &format!("文字牌显示于 ({x},{y}) {w}x{h}，caret=({},{})", caret_rect.left, caret_rect.top),
            );
        }
        rt.last_state = Some(state);
        rt.last_caret = Some(caret_rect);
        rt.last_show = Some(now);

        // 托盘提示跟着状态走，只有状态变化时才调用 Shell_NotifyIcon
        if rt.last_tip_state != state.key() {
            rt.last_tip_state = state.key();
            let tip = format!("input-tip-ng · {}", state.key());
            let main = HWND(rt.window as *mut core::ffi::c_void);
            let _ = tray::update_tip(main, &tip);
        }
    } else if let Some(shown_at) = rt.last_show {
        if now.duration_since(shown_at).as_millis() as u64 >= rt.theme.hide_delay_ms {
            let _ = symbol::begin_hide(hwnd_of(rt));
            rt.last_show = None;
        }
    }
}

/// 处理托盘菜单
fn handle_menu(hwnd: HWND) {
    let paused = {
        let guard = match RT.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.as_ref().map(|rt| rt.paused).unwrap_or(false)
    };

    log::write("tray", "debug", "打开托盘菜单");
    let action = tray::popup_menu(hwnd, paused);
    log::write("tray", "debug", "托盘菜单已关闭");
    match action {
        tray::MenuAction::TogglePause => {
            let now_paused = {
                let mut guard = match RT.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                match guard.as_mut() {
                    Some(rt) => {
                        rt.paused = !rt.paused;
                        rt.last_state = None;
                        rt.last_caret = None;
                        rt.last_show = None;
                        rt.paused
                    }
                    None => false,
                }
            };
            let tip = if now_paused { "input-tip-ng（已暂停）" } else { "input-tip-ng" };
            let _ = tray::update_tip(hwnd, tip);
            log::info("tray", &format!("提示已{}", if now_paused { "暂停" } else { "恢复" }));
        }
        tray::MenuAction::OpenConfig => {
            let path = crate::log::root_dir().join("config.ini");
            let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
        }
        tray::MenuAction::OpenLogs => {
            let dir = crate::log::log_dir();
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
        tray::MenuAction::Exit => {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        tray::MenuAction::None => {}
    }
}

fn request_exit_impl() -> bool {
    let class = wide(WINDOW_CLASS);
    unsafe {
        match FindWindowW(
            windows::core::PCWSTR::from_raw(class.as_ptr()),
            windows::core::PCWSTR::null(),
        ) {
            Ok(hwnd) if !hwnd.0.is_null() => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                true
            }
            _ => false,
        }
    }
}

/// 请求已运行实例退出；返回是否找到实例
pub fn request_exit() -> bool {
    request_exit_impl()
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            tick();
            LRESULT(0)
        }
        tray::WM_TRAY => {
            // 经典回调格式：lParam 低位是鼠标消息
            let mouse = (lparam.0 & 0xFFFF) as u32;
            log::write("tray", "debug", &format!("收到托盘回调 mouse=0x{mouse:04X}"));
            if mouse == 0x0205 || mouse == 0x0202 || mouse == 0x0203 || mouse == 0x0204 {
                // WM_RBUTTONUP / WM_LBUTTONUP
                handle_menu(hwnd);
            }
            LRESULT(0)
        }
        m if m != 0 && m == unsafe { TASKBAR_CREATED } => {
            let ok = tray::add(hwnd, "input-tip-ng");
            log::write("tray", if ok { "info" } else { "warn" }, "资源管理器重启，重新添加托盘图标");
            LRESULT(0)
        }
        WM_CLOSE => {
            log::info("app", "收到退出请求");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
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
    let cfg = AppConfig::load();
    let theme = cfg.symbol_theme();
    let styles: Vec<(InputState, String, u32)> = ALL_STATES
        .iter()
        .map(|s| (*s, cfg.symbol_text(*s), cfg.symbol_bg(*s)))
        .collect();
    let rules = cfg.rules_show();
    let poll_ms = cfg.poll_interval_ms();

    log::info(
        "app",
        &format!(
            "启动 input-tip-ng v{}（{}），白名单 {} 个进程，轮询 {}ms",
            env!("CARGO_PKG_VERSION"),
            if autostarted { "自启" } else { "手工" },
            rules.len(),
            poll_ms
        ),
    );

    let mutex_name = wide("Local\\input-tip-ng-singleton");

    unsafe {
        let mutex = match CreateMutexW(None, false, windows::core::PCWSTR::from_raw(mutex_name.as_ptr())) {
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
            lpszClassName: windows::core::PCWSTR::from_raw(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            log::error("app", "注册窗口类失败");
            return 1;
        }

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::PCWSTR::from_raw(class.as_ptr()),
            windows::core::PCWSTR::from_raw(title.as_ptr()),
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

        TASKBAR_CREATED = windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(
            windows::core::w!("TaskbarCreated"),
        );

        let symbol_hwnd = symbol::create();
        if symbol_hwnd.0.is_null() {
            log::error("app", "创建文字牌窗口失败，状态提示将不可用");
        }

        {
            let mut guard = match RT.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *guard = Some(Runtime {
                window: hwnd.0 as isize,
                symbol: symbol_hwnd.0 as isize,
                theme,
                styles,
                rules,
                paused: false,
                last_fg: 0,
                last_proc: String::new(),
                last_tip_state: "",
                last_state: None,
                last_caret: None,
                last_show: None,
            });
        }

        let tray_ok = tray::add(hwnd, "input-tip-ng");
        log::write(
            "tray",
            if tray_ok { "info" } else { "warn" },
            if tray_ok { "托盘图标已添加" } else { "托盘图标添加失败" },
        );

        let _ = SetTimer(Some(hwnd), 1, poll_ms, None);
        log::info("app", "常驻进程已就绪");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        tray::remove(hwnd);
        log::info("app", "退出");
        let _ = CloseHandle(mutex);
    }
    0
}
