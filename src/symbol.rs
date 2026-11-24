//! 光标旁文字牌（Direct2D + DirectWrite）。
//!
//! 扁平化：纯色圆角块 + 文字，无阴影、无描边。
//! 三个关键细节：
//! 1. 离屏 DIB 四周各留 1 像素透明边，圆角抗锯齿不会顶到窗口边界（否则会被裁出硬边）；
//! 2. 关掉 DWM 给顶层窗口加的默认描边与系统圆角（DWMWA_BORDER_COLOR=none / DONOTROUND）；
//! 3. 字号按「磅」解释（与旧版 AHK 的 s11 一致），再按显示器 DPI 换成设备像素排版。

use std::cell::RefCell;

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
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, RegisterClassW, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, ULW_ALPHA,
    UpdateLayeredWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, WNDCLASSW, WINDOW_EX_STYLE, WM_CLOSE,
};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

const CLASS_NAME: &str = "InputTipNgSymbolWindow";

/// 圆角块四周的透明边（物理像素）：给抗锯齿留余量，避免裁切产生硬边
const EDGE_MARGIN_PX: i32 = 1;

/// 文字两侧/上下的内边距（DIP）
const PAD_X_DIP: f32 = 6.0;
const PAD_Y_DIP: f32 = 3.0;

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

/// 去掉系统给顶层窗口加的描边与系统圆角（Win11）
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
        // 必须置顶：否则在前台窗口最大化/全屏时会被压在下面（Win32 的 IsWindowVisible 仍是 true，
        // 但屏幕上看不到，非常难查）
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

    let rendered = with_ctx(|ctx| -> Option<(i32, i32, i32, i32)> {
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

            // 圆角块尺寸（DIP）：文字墨迹 + 内边距，保证最小高度不至于扁
            let w_dip = ink_w + PAD_X_DIP * 2.0;
            let h_dip = (ink_h + PAD_Y_DIP * 2.0).max(size_dip * 1.25);
            let w_px = (w_dip * scale).round().max(8.0) as i32;
            let h_px = (h_dip * scale).round().max(8.0) as i32;

            // 圆角：不超过高度的 1/4，避免小尺寸下变成胶囊/椭圆
            let radius_dip = corner.clamp(0.0, h_dip * 0.22).max(1.0);
            // 1 像素透明边折算成 DIP，绘制整体内缩
            let inset_dip = EDGE_MARGIN_PX as f32 / scale;

            let dib_w = w_px + EDGE_MARGIN_PX * 2;
            let dib_h = h_px + EDGE_MARGIN_PX * 2;

            let mem_dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: dib_w,
                    biHeight: -dib_h,
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
            let bounds = RECT { left: 0, top: 0, right: dib_w, bottom: dib_h };
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

            // 透明底必须用灰度抗锯齿，否则半透明边缘会出现彩色描边
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.BeginDraw();
            rt.Clear(Some(&color(0, 0.0)));

            if let Ok(brush) = rt.CreateSolidColorBrush(&color(bg, 1.0), None) {
                let rounded = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: inset_dip,
                        top: inset_dip,
                        right: inset_dip + w_dip,
                        bottom: inset_dip + h_dip,
                    },
                    radiusX: radius_dip,
                    radiusY: radius_dip,
                };
                rt.FillRoundedRectangle(&rounded, &brush);
            }

            if let Ok(brush) = rt.CreateSolidColorBrush(&color(fg, 1.0), None) {
                // 把排版框左移/上移墨迹偏移，让真实墨迹落在内边距中心
                let text_rect = D2D_RECT_F {
                    left: inset_dip + PAD_X_DIP - ink_left,
                    top: inset_dip + PAD_Y_DIP - ink_top,
                    right: inset_dip + PAD_X_DIP - ink_left + ink_w + 8.0,
                    bottom: inset_dip + PAD_Y_DIP - ink_top + ink_h + 8.0,
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

            if let Ok(path) = std::env::var("ITNG_PREVIEW_DUMP") {
                dump_bmp(std::path::Path::new(&path), bits, dib_w, dib_h);
            }

            let x = caret.left + (offset.0 as f32 * scale).round() as i32 - EDGE_MARGIN_PX;
            let y = caret.top + (offset.1 as f32 * scale).round() as i32 - h_px - EDGE_MARGIN_PX;
            let dst = POINT { x, y };
            let size_px = SIZE { cx: dib_w, cy: dib_h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: if opacity >= 100 {
                    255
                } else {
                    (opacity as u32 * 255 / 100) as u8
                },
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            if let Err(e) = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&dst),
                Some(&size_px),
                Some(mem_dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            ) {
                crate::log::error("symbol", &format!("UpdateLayeredWindow 失败: {e}"));
            }

            // 每次显示都重新提到最前，避免被后来的置顶窗口盖住
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );

            SelectObject(mem_dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem_dc);

            Some((x, y, dib_w, dib_h))
        }
    });

    let out = rendered.flatten();
    if out.is_some() {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    out
}

/// 把 32 位 BGRA 像素写成 32 位 BMP（top-down），供预览取图核对渲染质量
fn dump_bmp(path: &std::path::Path, bits: *const core::ffi::c_void, w: i32, h: i32) {
    use std::io::Write;
    if bits.is_null() || w <= 0 || h <= 0 {
        return;
    }
    let stride = (w * 4) as usize;
    let image_size = stride * h as usize;
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
    let data = unsafe { std::slice::from_raw_parts(bits as *const u8, image_size) };
    // 可选：把预乘 alpha 合成到指定底色（模拟桌面），便于肉眼检查边缘有没有暗边
    if let Ok(bghex) = std::env::var("ITNG_PREVIEW_BG") {
        if let Ok(v) = u32::from_str_radix(bghex.trim_start_matches("0x"), 16) {
            let (br, bgg, bb) = (((v >> 16) & 0xFF) as u32, ((v >> 8) & 0xFF) as u32, (v & 0xFF) as u32);
            let mut composited = Vec::with_capacity(image_size);
            for px in data.chunks_exact(4) {
                let (b, g, r, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
                let inv = 255 - a;
                composited.push(((b + bb * inv / 255).min(255)) as u8);
                composited.push(((g + bgg * inv / 255).min(255)) as u8);
                composited.push(((r + br * inv / 255).min(255)) as u8);
                composited.push(255);
            }
            out.extend_from_slice(&composited);
            if let Ok(mut f) = std::fs::File::create(path) {
                let _ = f.write_all(&out);
            }
            return;
        }
    }
    out.extend_from_slice(data);
    if let Ok(mut f) = std::fs::File::create(path) {
        let _ = f.write_all(&out);
    }
}

pub fn hide(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
        );
    }
}
