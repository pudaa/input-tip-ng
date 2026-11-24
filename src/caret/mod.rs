//! 光标位置探测：五级策略，逐级降级，全部只读。
//!
//! 1. gti   —— GetGUIThreadInfo，原生控件最快；
//! 2. ia2   —— IAccessible2 文本接口，Chromium/Electron/Qt 的主力（光标由各自绘制，
//!              Win32 读不到，UIA 常常只给范围不给包围盒）；
//! 3. uia   —— UI Automation 文本模式，WinUI3 等程序；
//! 4. msaa  —— OBJID_CARET 辅助对象，少数老控件；
//! 5. anchor—— 拿不到真实光标时锚到正在输入的控件（只接受 Edit/文本元素）。
//!
//! 明确不用 AttachThreadInput + GetCaretPos：实测会干扰目标应用的输入法状态。

mod anchor;
mod gti;
mod ia2;
mod msaa;
mod uia;

pub use uia::ensure_event_handler;

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowRect};

/// 跨进程查询（IA2/UIA/MSAA）每 20ms 一次太贵；缓存 80ms，
/// 期间用上次结果（位置补间本身会把移动做平滑，不会显得卡顿）。
const HEAVY_CACHE_MS: u128 = 80;

thread_local! {
    static HEAVY_CACHE: RefCell<Option<(Instant, Option<(RECT, &'static str)>)>> = const { RefCell::new(None) };
    /// 注入查询要往目标 UI 线程发同步消息，太频繁会拖慢目标应用的输入
    static INJECT_CACHE: RefCell<Option<(Instant, Option<RECT>)>> = const { RefCell::new(None) };
    /// 本次 rect() 的结果来自哪一级（日志与 --diag 用）
    static SOURCE: RefCell<&'static str> = const { RefCell::new("none") };
}

/// 本次 rect() 采用的光标来源
pub fn source() -> &'static str {
    SOURCE.with(|s| *s.borrow())
}

fn record(src: &'static str) {
    SOURCE.with(|s| *s.borrow_mut() = src);
}

/// 注入查询至少间隔 120ms：SendMessageTimeout 是同步的，每 20ms 发一次会给前台应用添延迟
fn inject_cached() -> Option<RECT> {
    INJECT_CACHE.with(|slot| {
        if let Some((at, value)) = slot.borrow().as_ref() {
            if at.elapsed().as_millis() < 120 {
                return *value;
            }
        }
        let fresh = crate::inject::caret();
        *slot.borrow_mut() = Some((Instant::now(), fresh));
        fresh
    })
}

/// 让下一拍强制重新查询（IME 组合期各接口可能返回停滞的矩形）
pub fn invalidate_cache() {
    HEAVY_CACHE.with(|slot| *slot.borrow_mut() = None);
    INJECT_CACHE.with(|slot| *slot.borrow_mut() = None);
}

fn heavy_cached() -> Option<(RECT, &'static str)> {
    HEAVY_CACHE.with(|slot| {
        if let Some((at, value)) = slot.borrow().as_ref() {
            if at.elapsed().as_millis() < HEAVY_CACHE_MS {
                return *value;
            }
        }
        // 顺序有讲究：IA2 是浏览器/Electron/Qt 唯一可靠的矩形来源，先问它
        let fresh = ia2::caret()
            .map(|r| (r, "ia2"))
            .or_else(|| uia::caret().map(|r| (r, "uia")))
            .or_else(|| msaa::caret().map(|r| (r, "msaa")));
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
        record("gti");
        return Some(RECT {
            left: pt.x,
            top: pt.y,
            right: pt.x + width,
            bottom: pt.y + height,
        });
    }
    // 注入式读取（默认关闭）：只对真正的 Win32 编辑控件有效
    if let Some(r) = inject_cached() {
        record("inject");
        return Some(r);
    }
    if let Some((r, src)) = heavy_cached() {
        record(src);
        return Some(r);
    }
    if let Some(r) = anchor::rect() {
        record("anchor");
        return Some(r);
    }
    record("none");
    None
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
        "gti={} | ia2={} | uia={} | msaa={} | anchor={} | 最终来源={}",
        fmt_rect(gti::caret().map(|(_, c)| c)),
        fmt_rect(ia2::caret()),
        fmt_rect(uia::caret()),
        fmt_rect(msaa::caret()),
        fmt_rect(anchor::rect()),
        rect().map(|_| source()).unwrap_or("不可用"),
    )
}

/// UIA 细节，--probe 的补充行
pub fn uia_probe() -> String {
    uia::probe_text()
}

/// IA2 细节，--diag 使用
pub fn ia2_probe() -> String {
    ia2::probe()
}

/// IA2 细节（指定窗口），--ia2-test 使用
pub fn ia2_probe_at(hwnd: HWND) -> String {
    ia2::probe_at(hwnd)
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
        uia::wake_accessible();
        out.push_str(&format!("探测: {}\n", probe()));
        out.push_str(&format!("IA2: {}\n", ia2::probe()));
        out.push_str(&format!("UIA: {}\n", uia::probe_text()));
        out.push_str(&format!("UIA 追踪: {}\n", uia::caret_trace()));
        let final_rect = rect();
        out.push_str(&format!("最终: {} 来源={}\n", fmt_rect(final_rect), source()));

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
