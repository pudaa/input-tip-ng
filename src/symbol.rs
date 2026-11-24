//! 光标旁文字牌（Direct2D + DirectWrite）。
//!
//! 为什么不用 GDI 画在普通窗口上：
//! - 普通窗口靠窗口区域裁圆角，边缘是 1 位掩码，必然有锯齿；
//! - 整窗 alpha 会让文字与背景一起被系统拉伸，高 DPI 下糊。
//!
//! 这里的做法：离屏画到 32 位 DIB（预乘 alpha），DirectWrite 抗锯齿排版，
//! 再用 UpdateLayeredWindow 整块贴上去 —— 每像素 alpha，圆角平滑，文字按真实 DPI 排版。

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
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SW_SHOWNOACTIVATE, ULW_ALPHA,
    UpdateLayeredWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    WS_POPUP, WNDCLASSW, WINDOW_EX_STYLE, WM_CLOSE,
};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

const CLASS_NAME: &str = "InputTipNgSymbolWindow";

/// 每线程的 D2D/DWrite 对象（窗口与绘制都在主线程，故用 thread_local 而不是全局 Mutex）
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
            // 对齐交给调用方按「墨迹盒」精确定位：居中对齐会把字形的左右留白也算进去，
            // 短文本（En / 漢）看上去就会偏左偏下，所以这里保持默认的左/上对齐。
            let _ = format;
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
            WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0,
        );
        CreateWindowExW(
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
        .unwrap_or_default()
    }
}

/// 显示文字牌。dpi 用调用方给的前台窗口的真实 DPI。
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

    let rendered = with_ctx(|ctx| -> Option<(i32, i32, i32, i32)> {
        let format = text_format(ctx, font_name, size, weight)?;
        unsafe {
            // 1. 量文字（DIP）
            let body = text16(text);
            let layout = ctx.dwrite.CreateTextLayout(&body, &format, 4096.0, 4096.0).ok()?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).ok()?;
            // 墨迹盒：left/top 是字形相对排版框的偏移，width/height 是真实墨迹尺寸
            let ink_left = metrics.left;
            let ink_top = metrics.top;
            let ink_w = metrics.width.max(1.0);
            let ink_h = metrics.height.max(1.0);

            // 2. 圆角牌尺寸（DIP -> 物理像素）
            let pad_x_dip = 4.0;
            let pad_y_dip = 1.5;
            let w_dip = ink_w + pad_x_dip * 2.0;
            let h_dip = ink_h + pad_y_dip * 2.0;
            let w_px = (w_dip * scale).round().max(8.0) as i32;
            let h_px = (h_dip * scale).round().max(8.0) as i32;
            crate::log::write(
                "symbol",
                "debug",
                &format!(
                    "ink=({:.2},{:.2}) {:.2}x{:.2} chip={}x{} dpi={} scale={:.2}",
                    ink_left, ink_top, ink_w, ink_h, w_px, h_px, dpi, scale
                ),
            );

            // 3. 离屏 DIB
            let mem_dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w_px,
                    biHeight: -h_px, // 负值 = 自上而下
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

            // 4. D2D 画到这块 DIB
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
            // 透明底上必须用灰度抗锯齿：次像素（ClearType）会在半透明边缘留下彩色描边
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.BeginDraw();
            rt.Clear(Some(&color(0, 0.0)));
            let bg_brush = rt.CreateSolidColorBrush(&color(bg, 1.0), None).ok();
            if let Some(brush) = &bg_brush {
                let rounded = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: w_dip,
                        bottom: h_dip,
                    },
                    radiusX: corner,
                    radiusY: corner,
                };
                rt.FillRoundedRectangle(&rounded, brush);
            }
            let fg_brush = rt.CreateSolidColorBrush(&color(fg, 1.0), None).ok();
            if let Some(brush) = &fg_brush {
                // 把排版框整体左移/上移 ink_left/ink_top，让真实墨迹落在 (pad, pad)，
                // 这样短文本在任何字体下都视觉居中。
                let rect = D2D_RECT_F {
                    left: pad_x_dip - ink_left,
                    top: pad_y_dip - ink_top,
                    right: pad_x_dip - ink_left + ink_w + 8.0,
                    bottom: pad_y_dip - ink_top + ink_h + 8.0,
                };
                rt.DrawText(
                    &body,
                    &format,
                    &rect,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            if let Err(e) = rt.EndDraw(None, None) {
                crate::log::error("symbol", &format!("EndDraw 失败: {e}"));
            }
            if !bits.is_null() {
                let px = (bits as *const u8)
                    .add(((h_px / 2) * w_px + (w_px / 2)) as usize * 4);
                let b = *px;
                let g = *px.add(1);
                let r = *px.add(2);
                let a = *px.add(3);
                crate::log::write(
                    "symbol",
                    "debug",
                    &format!("DIB 中心像素 BGRA=({b},{g},{r},{a})"),
                );
            }

            // 5. 贴到分层窗口
            let x = caret.left + (offset.0 as f32 * scale).round() as i32;
            let y = caret.top + (offset.1 as f32 * scale).round() as i32 - h_px;
            let dst = POINT { x, y };
            let size_px = SIZE { cx: w_px, cy: h_px };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: if opacity >= 100 { 255 } else { (opacity as u32 * 255 / 100) as u8 },
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

            if let Ok(path) = std::env::var("ITNG_PREVIEW_DUMP") {
                dump_bmp(std::path::Path::new(&path), bits, w_px, h_px);
            }

            SelectObject(mem_dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem_dc);

            Some((x, y, w_px, h_px))
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
