//! 输入法状态探测。
//!
//! Windows 没有一条能覆盖所有程序的公开接口，故这里按可靠性从高到低多路尝试：
//! 1. ImmGetContext + ImmGetConversionStatus：跨进程可用，能同时拿到「转换模式」与「打开状态」；
//! 2. 给前台线程的默认 IME 窗口发 WM_IME_CONTROL(IMC_GETOPENSTATUS)：部分第三方输入法只认这条；
//! 3. 都失败时回退到「该进程最后一次成功探测到的状态」，而不是猜一个默认值
//!    （旧版靠 GeneralStrategy 猜 opened/convMode 哪个可信，切换窗口时容易闪错）。
//!
//! 仍然读不到的：未开启 IMM32 兼容层的 TSF-only 程序（部分 WinUI3/UWP）与提权窗口（UIPI 阻断）。
//! 这是系统限制，新版不承诺 100%，但保证不自作聪明地乱猜。

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::Ime::{
    IME_CONVERSION_MODE, IME_SENTENCE_MODE, ImmGetContext, ImmGetConversionStatus,
    ImmGetDefaultIMEWnd, ImmReleaseContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, VK_CAPITAL};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
};

use crate::process;
use crate::state::InputState;

/// 首次运行或探测全失败时的默认状态
const DEFAULT_STATE: InputState = InputState::En;

static CACHE: Mutex<Option<HashMap<String, InputState>>> = Mutex::new(None);

fn cache_with<T>(f: impl FnOnce(&mut HashMap<String, InputState>) -> T) -> T {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    f(guard.as_mut().unwrap())
}

fn remember(proc_name: &str, state: InputState) {
    if proc_name.is_empty() {
        return;
    }
    let key = proc_name.to_ascii_lowercase();
    cache_with(|m| {
        m.insert(key, state);
    });
}

fn recall(proc_name: &str) -> Option<InputState> {
    if proc_name.is_empty() {
        return None;
    }
    let key = proc_name.to_ascii_lowercase();
    cache_with(|m| m.get(&key).copied())
}

fn caps_lock_on() -> bool {
    unsafe { GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 }
}

fn lang_id(hwnd: HWND) -> u16 {
    unsafe {
        let tid = GetWindowThreadProcessId(hwnd, None);
        let hkl = GetKeyboardLayout(tid);
        // HKL 的低字是键盘布局 id，低字节为主语言 id
        (hkl.0 as usize & 0xFFFF) as u16
    }
}

/// 转换模式：Bit0 = IME_CMODE_NATIVE（中/日/韩文输入态）
fn conversion_mode(hwnd: HWND) -> Option<u32> {
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.0.is_null() {
            return None;
        }
        let mut conv = IME_CONVERSION_MODE(0);
        let mut sent = IME_SENTENCE_MODE(0);
        let ok = ImmGetConversionStatus(himc, Some(&mut conv), Some(&mut sent)).as_bool();
        let _ = ImmReleaseContext(hwnd, himc);
        if ok { Some(conv.0) } else { None }
    }
}

/// 打开状态：非 0 表示处于中文输入态。走默认 IME 窗口，兼容只认消息的输入法。
fn open_status(hwnd: HWND) -> Option<u32> {
    unsafe {
        let ime_wnd = ImmGetDefaultIMEWnd(hwnd);
        if ime_wnd.0.is_null() {
            return None;
        }
        // WM_IME_CONTROL = 0x0283，IMC_GETOPENSTATUS = 0x0006
        let mut result: usize = 0;
        let ok = SendMessageTimeoutW(
            ime_wnd,
            0x0283,
            WPARAM(0x0006),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            100,
            Some(&mut result),
        );
        if ok.0 != 0 { Some(result as u32) } else { None }
    }
}

/// 主入口：给前台窗口，返回当前应显示的状态
pub fn detect(fg: HWND) -> InputState {
    if fg.0.is_null() {
        return DEFAULT_STATE;
    }
    if caps_lock_on() {
        return InputState::Caps;
    }

    let lang = lang_id(fg);
    let core = (lang & 0xFF) as u8;

    // 非中文布局：直接按语言给结论（与旧版一致，避免在日/韩布局上误判中英）
    match core {
        0x11 => return InputState::Jp,
        0x12 => return InputState::Kr,
        0x09 => return InputState::Us,
        0x04 => {}
        _ => {}
    }

    let proc_name = process::name(fg).unwrap_or_default();

    if let Some(conv) = conversion_mode(fg) {
        if conv != 0 {
            return if conv & 1 != 0 { InputState::Cn } else { InputState::En };
        }
    }
    if let Some(opened) = open_status(fg) {
        let state = if opened != 0 { InputState::Cn } else { InputState::En };
        remember(&proc_name, state);
        return state;
    }

    if let Some(cached) = recall(&proc_name) {
        return cached;
    }
    DEFAULT_STATE
}

/// 诊断信息，--probe 使用：把每一路的原始返回值都摊开，便于判断该信谁
pub fn probe(fg: HWND) -> String {
    use windows::Win32::UI::Input::Ime::ImmGetContext;

    unsafe {
        let lang = lang_id(fg);
        let tid = GetWindowThreadProcessId(fg, None);
        let himc = ImmGetContext(fg);
        let conv = conversion_mode(fg);
        let opened = open_status(fg);
        let _ = GetCurrentThreadId;
        let _ = ImmGetDefaultIMEWnd;
        format!(
            "lang=0x{:04X} tid={} himc={} conversion={:?} opened={:?} caps={}",
            lang,
            tid,
            !himc.0.is_null(),
            conv,
            opened,
            caps_lock_on()
        )
    }
}
