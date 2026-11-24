//! 应用图标：用 Direct2D 画一个多尺寸 ICO，并从内嵌 ICO 取 HICON 给托盘用。
//!
//! 设计：圆角正方形（圆角 = 边长 22%），上半 CN 棕 #A4622D、下半 EN 蓝 #87CEFA，
//! 中间一条白色竖向光标条 —— 纯色扁平、无渐变无描边，颜色与提示牌同源。
//! 多尺寸（16~256）分别渲染，避免系统缩放导致小尺寸发糊。

use std::path::Path;

use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1CreateFactory, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, ID2D1DCRenderTarget, ID2D1Factory,
    ID2D1RenderTarget,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP, HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR,
};

const CN_BROWN: u32 = 0xA4622D;
const EN_BLUE: u32 = 0x87CEFA;

fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: alpha,
    }
}

fn render(size: i32) -> Option<Vec<u8>> {
    unsafe {
        let factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let mem_dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap: HBITMAP =
            CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target: ID2D1DCRenderTarget = factory.CreateDCRenderTarget(&props).ok()?;
        let bounds = windows::Win32::Foundation::RECT { left: 0, top: 0, right: size, bottom: size };
        if target.BindDC(mem_dc, &bounds).is_err() {
            SelectObject(mem_dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem_dc);
            return None;
        }
        let rt: ID2D1RenderTarget = target.cast().ok()?;

        let s = size as f32;
        let radius = s * 0.22;
        let shape = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: 0.0, top: 0.0, right: s, bottom: s },
            radiusX: radius,
            radiusY: radius,
        };

        rt.BeginDraw();
        rt.Clear(Some(&color(0, 0.0)));

        // 上半：中文棕
        if let Ok(brush) = rt.CreateSolidColorBrush(&color(CN_BROWN, 1.0), None) {
            rt.FillRoundedRectangle(&shape, &brush);
        }
        // 下半：英文蓝（裁到下半区，保留底部圆角）
        if let Ok(brush) = rt.CreateSolidColorBrush(&color(EN_BLUE, 1.0), None) {
            rt.PushAxisAlignedClip(
                &D2D_RECT_F { left: 0.0, top: s / 2.0, right: s, bottom: s },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            rt.FillRoundedRectangle(&shape, &brush);
            rt.PopAxisAlignedClip();
        }
        // 白色光标条
        if let Ok(brush) = rt.CreateSolidColorBrush(&color(0xFFFFFF, 1.0), None) {
            let bar_w = (s * 0.13).max(1.5);
            let bar_h = s * 0.42;
            let bar = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: (s - bar_w) / 2.0,
                    top: (s - bar_h) / 2.0,
                    right: (s + bar_w) / 2.0,
                    bottom: (s + bar_h) / 2.0,
                },
                radiusX: bar_w / 2.0,
                radiusY: bar_w / 2.0,
            };
            rt.FillRoundedRectangle(&bar, &brush);
        }
        let _ = rt.EndDraw(None, None);

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits as *const u8, pixels.as_mut_ptr(), pixels.len());
        }
        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        Some(pixels)
    }
}

// 256 那一档单独要 262KB，等打包 exe 图标时再生成；托盘/窗口只需到 64
const SIZES: [i32; 7] = [16, 20, 24, 32, 40, 48, 64];

/// 生成多尺寸 ICO 文件
pub fn generate_ico(path: &Path) -> Result<(), String> {
    let mut images: Vec<(i32, Vec<u8>)> = Vec::new();
    for size in SIZES {
        let px = render(size).ok_or_else(|| format!("渲染 {size}px 图标失败"))?;
        images.push((size, px));
    }

    let count = images.len() as u16;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for (size, px) in &images {
        let s = *size;
        let row = s * 4;
        let mask_row = ((s + 31) / 32 * 4) as usize;
        let xor_size = (row * s) as usize;
        let and_size = mask_row * s as usize;
        let mut blob: Vec<u8> = Vec::with_capacity(40 + xor_size + and_size);
        blob.extend_from_slice(&40u32.to_le_bytes());
        blob.extend_from_slice(&s.to_le_bytes());
        blob.extend_from_slice(&(s * 2).to_le_bytes());
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&32u16.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&((xor_size + and_size) as u32).to_le_bytes());
        for _ in 0..4 {
            blob.extend_from_slice(&0u32.to_le_bytes());
        }
        for y in (0..s).rev() {
            let start = (y * row) as usize;
            blob.extend_from_slice(&px[start..start + row as usize]);
        }
        blob.extend(std::iter::repeat(0u8).take(and_size));

        let dim = if s >= 256 { 0u8 } else { s as u8 };
        out.extend_from_slice(&[dim, dim, 0u8, 0u8]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += blob.len() as u32;
        blobs.push(blob);
    }
    for blob in blobs {
        out.extend_from_slice(&blob);
    }
    std::fs::write(path, &out).map_err(|e| format!("写入 ICO 失败: {e}"))?;
    Ok(())
}

const EMBEDDED_ICO: &[u8] = include_bytes!("../assets/app.ico");

/// 从内嵌 ICO 里取最接近指定尺寸的 HICON
pub fn load(size: i32) -> Option<HICON> {
    let bytes = EMBEDDED_ICO;
    if bytes.len() < 6 {
        return None;
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut best: Option<(i32, u32, u32)> = None;
    for i in 0..count {
        let base = 6 + i * 16;
        if base + 16 > bytes.len() {
            break;
        }
        let w = if bytes[base] == 0 { 256 } else { bytes[base] as i32 };
        let len = u32::from_le_bytes([bytes[base + 8], bytes[base + 9], bytes[base + 10], bytes[base + 11]]);
        let off = u32::from_le_bytes([bytes[base + 12], bytes[base + 13], bytes[base + 14], bytes[base + 15]]);
        if best.map(|(bw, _, _)| (w - size).abs() < (bw - size).abs()).unwrap_or(true) {
            best = Some((w, len, off));
        }
    }
    let (w, len, off) = best?;
    let end = (off + len) as usize;
    if end > bytes.len() {
        return None;
    }
    let data = &bytes[off as usize..end];
    unsafe {
        CreateIconFromResourceEx(data, true, 0x0003_0000, w, w, LR_DEFAULTCOLOR).ok()
    }
}

#[allow(dead_code)]
pub fn destroy(icon: HICON) {
    unsafe {
        let _ = DestroyIcon(icon);
    }
}
