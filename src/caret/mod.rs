//! 光标位置探测：四级策略，逐级降级，全部只读。
//!
//! 1. gti   —— GetGUIThreadInfo，原生控件最快；
//! 2. uia   —— UI Automation 文本模式，Chromium/Electron/WinUI 的主力；
//! 3. msaa  —— OBJID_CARET 辅助对象，部分浏览器/控件；
//! 4. anchor—— 拿不到真实光标时锚到正在输入的控件（只接受 Edit/文本元素）。
//!
//! 明确不用 AttachThreadInput + GetCaretPos：实测会干扰目标应用的输入法状态。

mod anchor;
mod gti;
mod msaa;
mod uia;

pub use uia::ensure_event_handler;

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowRect};

/// UIA/MSAA 是跨进程 COM 调用，每 20ms 一次太贵；缓存 80ms，
/// 期间用上次结果（位置补间本身会把移动做平滑，不会显得卡顿）。
const UIA_CACHE_MS: u128 = 80;

thread_local! {
    static HEAVY_CACHE: RefCell<Option<(Instant, Option<RECT>)>> = const { RefCell::new(None) };
}

/// 让下一拍强制重新查询（IME 组合期 UIA 可能返回停滞的矩形）
pub fn invalidate_cache() {
    HEAVY_CACHE.with(|slot| *slot.borrow_mut() = None);
}

fn heavy_cached() -> Option<RECT> {
    HEAVY_CACHE.with(|slot| {
        if let Some((at, value)) = slot.borrow().as_ref() {
            if at.elapsed().as_millis() < UIA_CACHE_MS {
                return *value;
            }
        }
        let fresh = uia::caret().or_else(msaa::caret);
        *slot.borrow_mut() = Some((Instant::now(), fresh));
        fresh
    })
}

/// 返回 caret 的屏幕坐标矩形
pub fn rect() -> Option<RECT> {
    if let Some((hwnd, caret)) = gti::caret() {
        let mut pt = POINT { x: caret.left, y: caret.top };
        unsafe {
            if !ClientToScreen(hwnd, &mut pt).as_bool() {
                return None;
            }
        }
        let height = (caret.bottom - caret.top).max(1);
        let width = (caret.right - caret.left).max(1);
        return Some(RECT {
            left: pt.x,
            top: pt.y,
            right: pt.x + width,
            bottom: pt.y + height,
        });
    }
    // 注入式读取（默认关闭）：Chromium/Electron/Qt 只有这条路能拿到真实光标
    if let Some(r) = crate::inject::caret() {
        return Some(r);
    }
    if let Some(r) = heavy_cached() {
        return Some(r);
    }
    anchor::rect()
}

/// caret 所属窗口（诊断用）
pub fn owner() -> Option<HWND> {
    gti::caret().map(|(hwnd, _)| hwnd)
}

fn fmt_rect(r: Option<RECT>) -> String {
    match r {
        Some(r) => format!("({},{}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top),
        None => "不可用".to_string(),
    }
}

/// 一行摘要，--probe 使用
pub fn probe() -> String {
    format!(
        "gti={} | uia={} | msaa={} | anchor={}",
        fmt_rect(gti::caret().map(|(_, c)| c)),
        fmt_rect(uia::caret()),
        fmt_rect(msaa::caret()),
        fmt_rect(anchor::rect()),
    )
}

/// UIA 细节，--probe 的补充行
pub fn uia_probe() -> String {
    uia::probe_text()
}

/// 全链路诊断，--diag 使用
pub fn diag() -> String {
    let mut out = String::new();
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return "前台窗口为空".into();
        }
        let mut class = [0u16; 256];
        let n = GetClassNameW(fg, &mut class);
        let cls = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        let mut title = [0u16; 256];
        let t = GetWindowTextW(fg, &mut title);
        let ttl = String::from_utf16_lossy(&title[..t.max(0) as usize]);
        out.push_str(&format!(
            "前台: {} class={cls} title={ttl:?}\n",
            crate::process::name(fg).unwrap_or_default()
        ));
        out.push_str(&format!("探测: {}\n", probe()));
        out.push_str(&format!("UIA: {}\n", uia::probe_text()));
        out.push_str(&format!("最终: {}\n", fmt_rect(rect())));

        let mut win = RECT::default();
        let _ = GetWindowRect(fg, &mut win);
        if let Some((t, r, capable)) = uia::focused_info() {
            let ww = (win.right - win.left).max(1) as i64;
            let wh = (win.bottom - win.top).max(1) as i64;
            let ew = (r.right - r.left).max(0) as i64;
            let eh = (r.bottom - r.top).max(0) as i64;
            out.push_str(&format!(
                "焦点元素: type={t} 文本能力={capable} 占窗口={}%\n",
                if ww * wh > 0 { ew * eh * 100 / (ww * wh) } else { 0 }
            ));
        }
    }
    out
}
