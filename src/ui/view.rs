//! 设置窗口的布局、绘制与命中测试。
//!
//! 坐标统一用 DIP（Direct2D 渲染目标按窗口 DPI 映射到像素），鼠标事件先除以缩放比换算成 DIP。
//! 只有开关和滑块两类控件，不需要文本输入，避免在自绘窗口里处理 IME。

use std::cell::RefCell;

use windows::core::Interface;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1Factory, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};

use crate::config::AppConfig;

const NAV_W: f32 = 176.0;
const PAD: f32 = 28.0;
const ROW_H: f32 = 54.0;

const BG: u32 = 0xF6F7F9;
const CARD: u32 = 0xFFFFFF;
const TEXT: u32 = 0x1F2328;
const MUTED: u32 = 0x6B7280;
const ACCENT: u32 = 0x3B82F6;
const TRACK_COLOR: u32 = 0xE3E6EA;
const SELECTED: u32 = 0xEAF1FE;
const PREVIEW_BG: u32 = 0x87CEFA;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Nav(usize),
    Toggle,
    Slider { min: f32, max: f32 },
}

#[derive(Clone)]
struct Item {
    kind: Kind,
    rect: RECT,
    section: &'static str,
    key: &'static str,
    label: &'static str,
}

struct View {
    items: Vec<Item>,
    drag: Option<usize>,
}

thread_local! {
    static VIEW: RefCell<View> = RefCell::new(View { items: Vec::new(), drag: None });
    static FACTORY: RefCell<Option<(ID2D1Factory, IDWriteFactory)>> = const { RefCell::new(None) };
}

fn color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

fn factories() -> Option<(ID2D1Factory, IDWriteFactory)> {
    FACTORY.with(|f| {
        if f.borrow().is_none() {
            unsafe {
                let d2d =
                    D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                        .ok()?;
                let dw = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED).ok()?;
                *f.borrow_mut() = Some((d2d, dw));
            }
        }
        f.borrow().clone()
    })
}

fn num(cfg: &AppConfig, section: &str, key: &str, def: f32) -> f32 {
    cfg.ini
        .get(section, key)
        .and_then(|v| v.trim().parse::<f32>().ok())
        .unwrap_or(def)
}

fn flag(cfg: &AppConfig, section: &str, key: &str) -> bool {
    cfg.ini
        .get(section, key)
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

fn set_num(cfg: &mut AppConfig, section: &str, key: &str, value: f32) {
    cfg.ini.set(section, key, &format!("{:.0}", value));
    let _ = cfg.save();
}

fn set_flag(cfg: &mut AppConfig, section: &str, key: &str, value: bool) {
    cfg.ini.set(section, key, if value { "true" } else { "false" });
    let _ = cfg.save();
}

type Row = (&'static str, &'static str, &'static str, Kind);

fn page_items(page: usize) -> Vec<Row> {
    match page {
        0 => vec![
            ("autostart", "enabled", "开机自启", Kind::Toggle),
            ("app", "idle_limit_ms", "静止多久后收起", Kind::Slider { min: 300.0, max: 4000.0 }),
            ("app", "poll_interval_ms", "轮询间隔", Kind::Slider { min: 10.0, max: 100.0 }),
        ],
        1 => vec![
            ("symbol", "size", "字号（磅）", Kind::Slider { min: 8.0, max: 24.0 }),
            ("symbol", "corner", "圆角", Kind::Slider { min: 0.0, max: 12.0 }),
            ("symbol", "opacity", "不透明度", Kind::Slider { min: 30.0, max: 100.0 }),
            ("symbol", "offset_x", "距光标右缘", Kind::Slider { min: -20.0, max: 40.0 }),
            ("symbol", "hide_delay_ms", "停留时间", Kind::Slider { min: 500.0, max: 6000.0 }),
        ],
        _ => vec![
            ("inject", "enabled", "注入式读取光标", Kind::Toggle),
        ],
    }
}

fn layout(page: usize, w_dip: f32) -> Vec<Item> {
    let mut items = Vec::new();
    for i in 0..3 {
        items.push(Item {
            kind: Kind::Nav(i),
            rect: RECT {
                left: 12,
                top: (64.0 + i as f32 * 44.0) as i32,
                right: (NAV_W - 12.0) as i32,
                bottom: (64.0 + i as f32 * 44.0 + 34.0) as i32,
            },
            section: "",
            key: "",
            label: "",
        });
    }
    let left = NAV_W + PAD;
    let row_w = w_dip - left - PAD;
    for (i, (section, key, label, kind)) in page_items(page).into_iter().enumerate() {
        let top = PAD + 36.0 + i as f32 * ROW_H;
        items.push(Item {
            kind,
            rect: RECT {
                left: left as i32,
                top: top as i32,
                right: (left + row_w) as i32,
                bottom: (top + ROW_H - 14.0) as i32,
            },
            section,
            key,
            label,
        });
    }
    items
}

pub fn invalidate(hwnd: HWND, state: &crate::ui::State) -> Result<(), ()> {
    let (w, h) = crate::ui::client_size(hwnd);
    let scale = crate::ui::dpi_of(hwnd) as f32 / 96.0;
    let items = layout(state.page, w as f32 / scale);
    let _ = h;
    VIEW.with(|v| v.borrow_mut().items = items);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    Ok(())
}

pub fn paint(hwnd: HWND, state: &mut crate::ui::State) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let (w_px, h_px) = crate::ui::client_size(hwnd);
        let dpi = crate::ui::dpi_of(hwnd);
        let scale = dpi as f32 / 96.0;
        let w = w_px as f32 / scale;
        let h = h_px as f32 / scale;

        let Some((d2d, dw)) = factories() else {
            let _ = EndPaint(hwnd, &ps);
            return;
        };
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let Ok(target) = d2d.CreateDCRenderTarget(&props) else {
            let _ = EndPaint(hwnd, &ps);
            return;
        };
        let bounds = RECT { left: 0, top: 0, right: w_px, bottom: h_px };
        if target.BindDC(hdc, &bounds).is_err() {
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let Ok(rt) = target.cast::<ID2D1RenderTarget>() else {
            let _ = EndPaint(hwnd, &ps);
            return;
        };
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        rt.BeginDraw();
        rt.Clear(Some(&color(BG)));

        fill_rect(&rt, 0.0, 0.0, NAV_W, h, CARD);
        draw_text(&dw, &rt, "input-tip-ng", PAD - 4.0, 24.0, 15.0, TEXT);

        let items = layout(state.page, w);
        let nav_names = ["常规", "提示", "规则"];
        for item in &items {
            let r = rect_f(&item.rect);
            match item.kind {
                Kind::Nav(idx) => {
                    let selected = idx == state.page;
                    if selected {
                        fill_round(&rt, r, 8.0, SELECTED);
                    }
                    draw_text(
                        &dw,
                        &rt,
                        nav_names[idx],
                        r.left + 16.0,
                        r.top + 8.0,
                        14.0,
                        if selected { ACCENT } else { MUTED },
                    );
                }
                Kind::Toggle => {
                    let value = if item.section == "autostart" {
                        state.cfg.autostart_enabled()
                    } else {
                        flag(&state.cfg, item.section, item.key)
                    };
                    draw_text(&dw, &rt, item.label, r.left, r.top + 6.0, 14.0, TEXT);
                    let tw = 42.0;
                    let th = 23.0;
                    let x = r.right - tw;
                    let y = r.top + 2.0;
                    fill_round(
                        &rt,
                        D2D_RECT_F { left: x, top: y, right: x + tw, bottom: y + th },
                        th / 2.0,
                        if value { ACCENT } else { TRACK_COLOR },
                    );
                    let knob = th - 6.0;
                    let kx = if value { x + tw - knob - 3.0 } else { x + 3.0 };
                    fill_circle(&rt, kx + knob / 2.0, y + th / 2.0, knob / 2.0, CARD);
                }
                Kind::Slider { min, max } => {
                    let value = num(&state.cfg, item.section, item.key, min).clamp(min, max);
                    draw_text(&dw, &rt, item.label, r.left, r.top, 14.0, TEXT);
                    draw_text(&dw, &rt, &format!("{:.0}", value), r.right - 48.0, r.top, 14.0, MUTED);
                    let track_y = r.top + 28.0;
                    let track_w = r.right - r.left - 8.0;
                    fill_round(
                        &rt,
                        D2D_RECT_F { left: r.left, top: track_y, right: r.left + track_w, bottom: track_y + 4.0 },
                        2.0,
                        TRACK_COLOR,
                    );
                    let ratio = ((value - min) / (max - min)).clamp(0.0, 1.0);
                    fill_round(
                        &rt,
                        D2D_RECT_F { left: r.left, top: track_y, right: r.left + track_w * ratio, bottom: track_y + 4.0 },
                        2.0,
                        ACCENT,
                    );
                    fill_circle(&rt, r.left + track_w * ratio, track_y + 2.0, 7.0, ACCENT);
                }
            }
        }

        // 提示页：右上角实时预览
        if state.page == 1 {
            let size = num(&state.cfg, "symbol", "size", 11.0) * 4.0 / 3.0;
            let side = (size * 2.2).max(24.0);
            let x = w - PAD - side;
            let y = PAD + 36.0;
            let corner = num(&state.cfg, "symbol", "corner", 7.0);
            fill_round(
                &rt,
                D2D_RECT_F { left: x, top: y, right: x + side, bottom: y + side },
                corner,
                PREVIEW_BG,
            );
            let text_size = size * 0.8;
            draw_text(&dw, &rt, "En", x + side * 0.24, y + side * 0.26, text_size, 0xFFFFFF);
        }

        let _ = rt.EndDraw(None, None);
        let _ = EndPaint(hwnd, &ps);
        VIEW.with(|v| v.borrow_mut().items = items);
    }
}

pub fn on_mouse(
    hwnd: HWND,
    state: &mut crate::ui::State,
    x: i32,
    y: i32,
    msg: u32,
    pressed: bool,
    released: bool,
) -> bool {
    let scale = crate::ui::dpi_of(hwnd) as f32 / 96.0;
    let x = (x as f32 / scale) as i32;
    let y = (y as f32 / scale) as i32;

    let dragging = VIEW.with(|v| v.borrow().drag);
    if let Some(idx) = dragging {
        if released {
            VIEW.with(|v| v.borrow_mut().drag = None);
            return true;
        }
        if msg == 0x0200 || pressed {
            let items = VIEW.with(|v| v.borrow().items.clone());
            if let Some(item) = items.get(idx) {
                if let Kind::Slider { min, max } = item.kind {
                    let r = rect_f(&item.rect);
                    let ratio = ((x as f32 - r.left) / (r.right - r.left)).clamp(0.0, 1.0);
                    let value = (min + (max - min) * ratio).round();
                    set_num(&mut state.cfg, item.section, item.key, value);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            }
            return true;
        }
    }

    if pressed {
        let items = VIEW.with(|v| v.borrow().items.clone());
        for (idx, item) in items.iter().enumerate() {
            if !(x >= item.rect.left && x < item.rect.right && y >= item.rect.top && y < item.rect.bottom) {
                continue;
            }
            match item.kind {
                Kind::Nav(page) => {
                    state.page = page;
                    let _ = invalidate(hwnd, state);
                    return true;
                }
                Kind::Toggle => {
                    let current = if item.section == "autostart" {
                        state.cfg.autostart_enabled()
                    } else {
                        flag(&state.cfg, item.section, item.key)
                    };
                    set_flag(&mut state.cfg, item.section, item.key, !current);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    return true;
                }
                Kind::Slider { .. } => {
                    VIEW.with(|v| v.borrow_mut().drag = Some(idx));
                    return true;
                }
            }
        }
    }
    false
}

fn rect_f(r: &RECT) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}

fn fill_rect(rt: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, rgb: u32) {
    unsafe {
        if let Ok(brush) = rt.CreateSolidColorBrush(&color(rgb), None) {
            rt.FillRectangle(&D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, &brush);
        }
    }
}

fn fill_round(rt: &ID2D1RenderTarget, r: D2D_RECT_F, radius: f32, rgb: u32) {
    unsafe {
        if let Ok(brush) = rt.CreateSolidColorBrush(&color(rgb), None) {
            rt.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius },
                &brush,
            );
        }
    }
}

/// 圆点 = 正方形 + 半径 = 半边长，避免依赖椭圆几何类型
fn fill_circle(rt: &ID2D1RenderTarget, cx: f32, cy: f32, radius: f32, rgb: u32) {
    fill_round(
        rt,
        D2D_RECT_F {
            left: cx - radius,
            top: cy - radius,
            right: cx + radius,
            bottom: cy + radius,
        },
        radius,
        rgb,
    );
}

fn draw_text(
    dw: &IDWriteFactory,
    rt: &ID2D1RenderTarget,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    rgb: u32,
) {
    unsafe {
        let family = windows::core::HSTRING::from("Microsoft YaHei UI");
        let locale = windows::core::HSTRING::from("zh-CN");
        let Ok(format) = dw.CreateTextFormat(
            &family,
            None,
            DWRITE_FONT_WEIGHT(400),
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &locale,
        ) else {
            return;
        };
        let Ok(brush) = rt.CreateSolidColorBrush(&color(rgb), None) else { return };
        let body: Vec<u16> = text.encode_utf16().collect();
        rt.DrawText(
            &body,
            &format,
            &D2D_RECT_F { left: x, top: y, right: x + 520.0, bottom: y + size * 2.2 },
            &brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

#[allow(dead_code)]
fn _unused(f: IDWriteTextFormat) {
    let _ = f;
}
