//! 光标旁文字牌（Direct2D + DirectWrite）。
//!
//! 扁平化：纯色圆角块 + 文字，无阴影、无描边。
//! 四个关键细节：
//! 1. 位图尺寸按设备像素取整，绘制用的 DIP 尺寸由像素反推 —— 直边正好落在整像素上，
//!    不会出现半透明"玻璃边框"（之前的做法在四周留 1px 透明边，白底上会看出一圈浅边）；
//! 2. 关掉 DWM 给顶层窗口加的默认描边与系统圆角；
//! 3. 字号按「磅」解释（与旧版 AHK 的 s11 一致），再按显示器 DPI 换成设备像素排版；
//! 4. 显示/隐藏走 120ms 淡入淡出：缓存最后一帧的预乘 alpha 像素，逐帧整体缩放 alpha。

use std::cell::RefCell;
use std::time::Instant;

use windows::core::{HSTRING, Interface, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, RegisterClassW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetWindowPos, ULW_ALPHA, UpdateLayeredWindow,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WNDCLASSW, WINDOW_EX_STYLE, WM_CLOSE,
};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

const CLASS_NAME: &str = "InputTipNgSymbolWindow";

/// 文字两侧/上下的内边距（DIP）
const PAD_X_DIP: f32 = 5.0;
const PAD_Y_DIP: f32 = 2.0;
/// 淡入淡出时长（毫秒）
const FADE_MS: u128 = 120;

struct Ctx {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    format: Option<TextFormatCache>,
}

struct TextFormatCache {
    face: String,
    size: f32,
    weight: i32,
    format: IDWriteTextFormat,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = RefCell::new(None);
    /// 最近一帧的预乘 alpha 像素与窗口位置（淡入淡出时重复使用，不重新排版）
    static FRAME: RefCell<Option<Frame>> = RefCell::new(None);
    static ANIM: RefCell<Option<Anim>> = RefCell::new(None);
}

struct Frame {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    opacity: u8,
}

struct Anim {
    start: Instant,
    /// true = 淡入，false = 淡出
    fading_in: bool,
}

fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: alpha,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn text16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn with_ctx<T>(f: impl FnOnce(&mut Ctx) -> T) -> Option<T> {
    CTX.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            unsafe {
                let d2d = match D2D1CreateFactory::<ID2D1Factory>(
                    D2D1_FACTORY_TYPE_SINGLE_THREADED,
                    None,
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        crate::log::error("symbol", &format!("创建 Direct2D 工厂失败: {e}"));
                        return None;
                    }
                };
                let dwrite = match DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)
                {
                    Ok(v) => v,
                    Err(e) => {
                        crate::log::error("symbol", &format!("创建 DirectWrite 工厂失败: {e}"));
                        return None;
                    }
                };
                *slot = Some(Ctx { d2d, dwrite, format: None });
            }
        }
        slot.as_mut().map(f)
    })
}

fn text_format(ctx: &mut Ctx, face: &str, size: f32, weight: i32) -> Option<IDWriteTextFormat> {
    let need_new = match &ctx.format {
        Some(c) => c.face != face || (c.size - size).abs() > 0.01 || c.weight != weight,
        None => true,
    };
    if need_new {
        unsafe {
            let family = HSTRING::from(face);
            let locale = HSTRING::from("zh-CN");
            let format = ctx
                .dwrite
                .CreateTextFormat(
                    &family,
                    None,
                    DWRITE_FONT_WEIGHT(weight),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &locale,
                )
                .ok()?;
            ctx.format = Some(TextFormatCache {
                face: face.to_string(),
                size,
                weight,
                format: format.clone(),
            });
            return Some(format);
        }
    }
    ctx.format.as_ref().map(|c| c.format.clone())
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    match msg {
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn strip_system_decoration(hwnd: HWND) {
    unsafe {
        let none: u32 = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const u32 as *const core::ffi::c_void,
            4,
        );
        let pref = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            4,
        );
    }
}

pub fn create() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: PCWSTR::from_raw(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return HWND::default();
        }
        let title = wide("input-tip-ng symbol");
        let ex = WINDOW_EX_STYLE(
            WS_EX_LAYERED.0
                | WS_EX_TRANSPARENT.0
                | WS_EX_NOACTIVATE.0
                | WS_EX_TOOLWINDOW.0
                | WS_EX_TOPMOST.0,
        );
        let hwnd = CreateWindowExW(
            ex,
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
        )
        .unwrap_or_default();
        if !hwnd.0.is_null() {
            strip_system_decoration(hwnd);
        }
        hwnd
    }
}

/// 把预乘 alpha 的像素按 factor 缩放后贴到分层窗口
fn blit(hwnd: HWND, frame: &Frame, factor: f32) -> bool {
    let factor = factor.clamp(0.0, 1.0);
    let alpha = (frame.opacity as f32 / 255.0) * factor;
    unsafe {
        let mem_dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: frame.width,
                biHeight: -frame.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(v) => v,
            Err(_) => {
                let _ = DeleteDC(mem_dc);
                return false;
            }
        };
        let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
        if !bits.is_null() {
            let dst = std::slice::from_raw_parts_mut(bits as *mut u8, frame.pixels.len());
            if factor >= 0.999 {
                dst.copy_from_slice(&frame.pixels);
            } else {
                for (d, s) in dst.chunks_exact_mut(4).zip(frame.pixels.chunks_exact(4)) {
                    d[0] = (s[0] as f32 * factor) as u8;
                    d[1] = (s[1] as f32 * factor) as u8;
                    d[2] = (s[2] as f32 * factor) as u8;
                    d[3] = (s[3] as f32 * factor) as u8;
                }
            }
        }
        let dst_pt = POINT { x: frame.x, y: frame.y };
        let size = SIZE { cx: frame.width, cy: frame.height };
        let src_pt = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (alpha * 255.0) as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let ok = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&dst_pt),
            Some(&size),
            Some(mem_dc),
            Some(&src_pt),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
        .is_ok();
        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        ok
    }
}

/// 显示文字牌。size 单位是磅（与旧版 AHK 的 s11 一致），dpi 用前台窗口的真实 DPI。
#[allow(clippy::too_many_arguments)]
pub fn show(
    hwnd: HWND,
    text: &str,
    bg: u32,
    fg: u32,
    font_name: &str,
    size: f32,
    weight: i32,
    opacity: u8,
    offset: (i32, i32),
    corner: f32,
    caret: &RECT,
    dpi: u32,
) -> Option<(i32, i32, i32, i32)> {
    if hwnd.0.is_null() || text.is_empty() {
        return None;
    }
    let scale = dpi as f32 / 96.0;
    // 配置里的字号按「磅」理解：1 磅 = 4/3 像素（96 DPI），与 AHK 的 SetFont s11 对齐
    let size_dip = (size * 4.0 / 3.0).clamp(6.0, 200.0);

    let rendered = with_ctx(|ctx| -> Option<Frame> {
        let format = text_format(ctx, font_name, size_dip, weight)?;
        unsafe {
            let body = text16(text);
            let layout = ctx.dwrite.CreateTextLayout(&body, &format, 4096.0, 4096.0).ok()?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).ok()?;

            let ink_left = metrics.left;
            let ink_top = metrics.top;
            let ink_w = metrics.width.max(1.0);
            let ink_h = metrics.height.max(1.0);

            // 尺寸先按 DIP 估，再取整到设备像素，最后用像素反推回 DIP —— 保证直边落在整像素
            let w_dip_est = ink_w + PAD_X_DIP * 2.0;
            let h_dip_est = (ink_h + PAD_Y_DIP * 2.0).max(size_dip * 1.2);
            // 统一成正方形：边长取宽高较大者，文字再双向居中（之前是宽>高的扁矩形）
            let side_px = ((w_dip_est.max(h_dip_est)) * scale).round().max(10.0) as i32;
            let w_px = side_px;
            let h_px = side_px;
            let w_dip = w_px as f32 / scale;
            let h_dip = h_px as f32 / scale;

            // 圆角：不超过高度的 1/4，避免小尺寸下变成胶囊/椭圆
            let radius_dip = corner.clamp(0.0, h_dip * 0.25).max(1.0);

            let mem_dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w_px,
                    biHeight: -h_px,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0 as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap: HBITMAP =
                match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = DeleteDC(mem_dc);
                        return None;
                    }
                };
            let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target: ID2D1DCRenderTarget = match ctx.d2d.CreateDCRenderTarget(&props) {
                Ok(t) => t,
                Err(_) => {
                    SelectObject(mem_dc, old);
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                    let _ = DeleteDC(mem_dc);
                    return None;
                }
            };
            let bounds = RECT { left: 0, top: 0, right: w_px, bottom: h_px };
            if target.BindDC(mem_dc, &bounds).is_err() {
                SelectObject(mem_dc, old);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(mem_dc);
                return None;
            }
            let rt: ID2D1RenderTarget = match target.cast() {
                Ok(v) => v,
                Err(_) => {
                    SelectObject(mem_dc, old);
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                    let _ = DeleteDC(mem_dc);
                    return None;
                }
            };

            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.BeginDraw();
            rt.Clear(Some(&color(0, 0.0)));

            if let Ok(brush) = rt.CreateSolidColorBrush(&color(bg, 1.0), None) {
                let rounded = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F { left: 0.0, top: 0.0, right: w_dip, bottom: h_dip },
                    radiusX: radius_dip,
                    radiusY: radius_dip,
                };
                rt.FillRoundedRectangle(&rounded, &brush);
            }

            if let Ok(brush) = rt.CreateSolidColorBrush(&color(fg, 1.0), None) {
                // 墨迹盒在正方形里双向居中
                let text_left = (w_dip - ink_w) / 2.0 - ink_left;
                let text_top = (h_dip - ink_h) / 2.0 - ink_top;
                let text_rect = D2D_RECT_F {
                    left: text_left,
                    top: text_top,
                    right: text_left + ink_w + 8.0,
                    bottom: text_top + ink_h + 8.0,
                };
                rt.DrawText(
                    &body,
                    &format,
                    &text_rect,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            if let Err(e) = rt.EndDraw(None, None) {
                crate::log::error("symbol", &format!("EndDraw 失败: {e}"));
            }

            let x = caret.left + (offset.0 as f32 * scale).round() as i32;
            let y = caret.top + (offset.1 as f32 * scale).round() as i32 - h_px;

            let mut pixels = vec![0u8; (w_px * h_px * 4) as usize];
            if !bits.is_null() {
                std::ptr::copy_nonoverlapping(
                    bits as *const u8,
                    pixels.as_mut_ptr(),
                    pixels.len(),
                );
            }
            if let Ok(path) = std::env::var("ITNG_PREVIEW_DUMP") {
                dump_bmp(std::path::Path::new(&path), &pixels, w_px, h_px);
            }

            SelectObject(mem_dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem_dc);

            // 配置里的 opacity 是 0-100 的百分比，这里先换成 0-255 的绝对 alpha，
            // 淡入淡出时再乘动画系数（之前直接按百分比当 alpha 用，导致整块半透明、像蒙了层玻璃）
            let opacity_255 = if opacity >= 100 { 255 } else { (opacity as u32 * 255 / 100) as u8 };
            Some(Frame { pixels, width: w_px, height: h_px, x, y, opacity: opacity_255 })
        }
    });

    let frame = rendered.flatten()?;
    let x = frame.x;
    let y = frame.y;
    let w = frame.width;
    let h = frame.height;
    FRAME.with(|f| *f.borrow_mut() = Some(frame));

    // 先以 alpha=0 贴上去再淡入，避免第一帧闪一下
    FRAME.with(|f| {
        if let Some(frame) = f.borrow().as_ref() {
            let _ = blit(hwnd, frame, 0.0);
        }
    });
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE);
    }
    ANIM.with(|a| {
        *a.borrow_mut() = Some(Anim { start: Instant::now(), fading_in: true });
    });
    Some((x, y, w, h))
}

/// 每帧推进动画：返回是否需要继续调用
pub fn tick_animation(hwnd: HWND) -> bool {
    let anim = ANIM.with(|a| {
        let slot = a.borrow();
        slot.as_ref().map(|anim| (anim.start, anim.fading_in))
    });
    let Some((start, fading_in)) = anim else { return false };

    let elapsed = start.elapsed().as_millis();
    let t = (elapsed as f32 / FADE_MS as f32).clamp(0.0, 1.0);
    let factor = if fading_in { t } else { 1.0 - t };
    FRAME.with(|f| {
        if let Some(frame) = f.borrow().as_ref() {
            let _ = blit(hwnd, frame, factor);
        }
    });

    if t >= 1.0 {
        ANIM.with(|a| *a.borrow_mut() = None);
        if !fading_in {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_HIDE);
            }
            FRAME.with(|f| *f.borrow_mut() = None);
        }
        return false;
    }
    true
}

/// 开始淡出（真正隐藏由 tick_animation 在动画结束时执行）
pub fn begin_hide(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let has_frame = FRAME.with(|f| f.borrow().is_some());
    if !has_frame {
        return false;
    }
    ANIM.with(|a| *a.borrow_mut() = Some(Anim { start: Instant::now(), fading_in: false }));
    true
}

#[allow(dead_code)]
pub fn hide(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_HIDE);
    }
}

/// 把 32 位 BGRA 像素写成 32 位 BMP（top-down），供预览取图核对渲染质量
fn dump_bmp(path: &std::path::Path, pixels: &[u8], w: i32, h: i32) {
    use std::io::Write;
    if w <= 0 || h <= 0 {
        return;
    }
    let image_size = pixels.len();
    let mut out = Vec::with_capacity(54 + image_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + image_size) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&(-h).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    // 可选：合成到指定底色，模拟桌面
    if let Ok(bghex) = std::env::var("ITNG_PREVIEW_BG") {
        if let Ok(v) = u32::from_str_radix(bghex.trim_start_matches("0x"), 16) {
            let (br, bgg, bb) = (
                ((v >> 16) & 0xFF) as u32,
                ((v >> 8) & 0xFF) as u32,
                (v & 0xFF) as u32,
            );
            for px in pixels.chunks_exact(4) {
                let (b, g, r, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
                let inv = 255 - a;
                out.push((b + bb * inv / 255).min(255) as u8);
                out.push((g + bgg * inv / 255).min(255) as u8);
                out.push((r + br * inv / 255).min(255) as u8);
                out.push(255);
            }
            if let Ok(mut f) = std::fs::File::create(path) {
                let _ = f.write_all(&out);
            }
            return;
        }
    }
    out.extend_from_slice(pixels);
    if let Ok(mut f) = std::fs::File::create(path) {
        let _ = f.write_all(&out);
    }
}
