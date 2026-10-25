//! Colors, geometry and pixel drawing for the flyout. Shapes are anti-aliased with a small
//! signed-distance rasteriser, text uses ClearType. Pure drawing: no window or audio logic.
use crate::app::Row;
use std::{collections::HashMap, ffi::c_void, mem::size_of, ptr::null_mut};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::Registry::*,
        UI::{
            Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
            WindowsAndMessaging::*,
        },
    },
};

pub const W: f32 = 380.0;
pub const PAD: f32 = 16.0;
pub const ROW_H: f32 = 60.0;
pub const GAP: f32 = 12.0;
pub const EMPTY_H: f32 = 44.0;
pub const EDGE: f32 = 12.0;
const CHIP_W: f32 = 150.0;

#[derive(Clone, Copy)]
struct Theme { bg: u32, border: u32, text: u32, dim: u32, track: u32, card: u32, ring: u32, accent: u32 }

/// Memory bitmap, fonts and icon cache. Freed whenever the flyout closes.
#[derive(Default)]
pub struct Gdi {
    mem: isize,
    bmp: isize,
    bits: usize,
    size: (i32, i32),
    fonts: [isize; 4], // text, title (semibold), glyph, small glyph
    icons: HashMap<String, isize>,
}

fn reg_dword(sub: PCWSTR, name: PCWSTR) -> Option<u32> {
    let (mut v, mut n) = (0u32, 4u32);
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, sub, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut c_void), Some(&mut n)) };
    (r == ERROR_SUCCESS).then_some(v)
}

pub fn light_theme(system: bool) -> bool {
    let name = if system { w!("SystemUsesLightTheme") } else { w!("AppsUseLightTheme") };
    reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), name).unwrap_or(0) == 1
}

fn theme() -> Theme {
    let a = reg_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor")).unwrap_or(0xFFD47800);
    let accent = ((a & 0xff) << 16) | (a & 0xff00) | ((a >> 16) & 0xff);
    if light_theme(false) {
        Theme { bg: 0xF3F3F3, border: 0xD6D6D6, text: 0x1B1B1B, dim: 0x5F5F5F, track: 0x9A9A9A, card: 0xFFFFFF, ring: 0xB0B0B0, accent }
    } else {
        Theme { bg: 0x242424, border: 0x3C3C3C, text: 0xFFFFFF, dim: 0xA6A6A6, track: 0x666666, card: 0x454545, ring: 0x5A5A5A, accent }
    }
}

fn bgr(c: u32) -> COLORREF {
    COLORREF(((c & 0xff) << 16) | (c & 0xff00) | (c >> 16))
}

// ---- geometry -------------------------------------------------------------

pub fn row_top(s: f32, i: usize) -> f32 {
    (PAD + i as f32 * ROW_H + if i >= 1 { GAP } else { 0.0 }) * s
}

pub fn total_h(s: f32, nrows: usize) -> i32 {
    let empty = if nrows <= 1 { EMPTY_H } else { 0.0 };
    (row_top(s, nrows) + (empty + 4.0) * s).ceil() as i32
}

pub fn slider_x(s: f32) -> (f32, f32) {
    (PAD * s + 40.0 * s, W * s - PAD * s - 44.0 * s)
}

pub fn center_y(s: f32, i: usize) -> f32 {
    row_top(s, i) + 40.0 * s
}

/// Horizontal extent of the device button on an app's title line.
pub fn chip_x(s: f32) -> (f32, f32) {
    let right = (W - PAD - 22.0) * s;
    (right - CHIP_W * s, right)
}

#[derive(PartialEq)]
pub enum Part { Mute, Slider, Device, Body }

pub fn hit(s: f32, rows: &[Row], x: f32, y: f32) -> Option<(usize, Part)> {
    let i = (0..rows.len()).find(|&i| y >= row_top(s, i) && y < row_top(s, i) + ROW_H * s)?;
    let (x0, x1) = slider_x(s);
    let cy = center_y(s, i);
    let (cl, cr) = chip_x(s);
    let part = if rows[i].device.is_some() && y < row_top(s, i) + 26.0 * s && x >= cl && x <= cr {
        Part::Device
    } else if (y - cy).abs() > 17.0 * s {
        Part::Body
    } else if x >= PAD * s && x <= PAD * s + 34.0 * s {
        Part::Mute
    } else if x >= x0 - 10.0 * s && x <= x1 + 10.0 * s {
        Part::Slider
    } else {
        Part::Body
    };
    Some((i, part))
}

// ---- drawing --------------------------------------------------------------

/// Anti-aliased rounded rectangle blended into a 0x00RRGGBB buffer.
fn rrect(buf: &mut [u32], bw: i32, bh: i32, r: (f32, f32, f32, f32), rad: f32, color: u32) {
    let (x0, y0, x1, y1) = r;
    let (cx, cy, hx, hy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let rad = rad.min(hx).min(hy);
    for py in (y0.floor() as i32 - 1).max(0)..(y1.ceil() as i32 + 1).min(bh) {
        for px in (x0.floor() as i32 - 1).max(0)..(x1.ceil() as i32 + 1).min(bw) {
            let dx = (px as f32 + 0.5 - cx).abs() - (hx - rad);
            let dy = (py as f32 + 0.5 - cy).abs() - (hy - rad);
            let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - rad;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov <= 0.0 { continue; }
            let p = &mut buf[(py * bw + px) as usize];
            let mix = |sh: u32| {
                let (a, b) = ((*p >> sh & 0xff) as f32, (color >> sh & 0xff) as f32);
                ((a + (b - a) * cov).round() as u32) << sh
            };
            *p = mix(16) | mix(8) | mix(0);
        }
    }
}

fn circle(buf: &mut [u32], bw: i32, bh: i32, c: (f32, f32), r: f32, color: u32) {
    rrect(buf, bw, bh, (c.0 - r, c.1 - r, c.0 + r, c.1 + r), r, color);
}

/// Linear peak (0..1) to a 0..1 bar length on a 60 dB scale, so quiet sound still shows.
pub fn level(peak: f32) -> f32 {
    if peak <= 0.0 { return 0.0; }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn glyph_for(vol: f32, mute: bool) -> u16 {
    if mute { 0xE74F } else if vol <= 0.0 { 0xE992 } else if vol < 0.34 { 0xE993 } else if vol < 0.67 { 0xE994 } else { 0xE995 }
}

fn font(height: f32, weight: i32, face: PCWSTR) -> isize {
    unsafe {
        CreateFontW(-(height.round() as i32), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, face).0 as isize
    }
}

fn draw_text(dc: HDC, f: isize, color: u32, text: &str, mut r: RECT, flags: DRAW_TEXT_FORMAT) {
    let mut w: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        SelectObject(dc, HGDIOBJ(f as _));
        SetTextColor(dc, bgr(color));
        DrawTextW(dc, &mut w, &mut r, flags | DT_SINGLELINE | DT_NOPREFIX);
    }
}

fn rc(l: f32, t: f32, r: f32, b: f32) -> RECT {
    RECT { left: l.round() as i32, top: t.round() as i32, right: r.round() as i32, bottom: b.round() as i32 }
}

fn icon_for(g: &mut Gdi, path: &str) -> Option<HICON> {
    if let Some(h) = g.icons.get(path) { return Some(HICON(*h as _)); }
    let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut sfi = SHFILEINFOW::default();
    let r = unsafe { SHGetFileInfoW(PCWSTR(w.as_ptr()), Default::default(), Some(&mut sfi), size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON) };
    if r == 0 || sfi.hIcon.is_invalid() { return None; }
    g.icons.insert(path.to_string(), sfi.hIcon.0 as isize);
    Some(sfi.hIcon)
}

fn ensure_gdi(g: &mut Gdi, dpi: u32, hdc: HDC, w: i32, h: i32) {
    unsafe {
        if g.fonts[0] == 0 {
            let s = dpi as f32 / 96.0;
            g.fonts = [
                font(14.0 * s, 400, w!("Segoe UI Variable Text")),
                font(14.0 * s, 600, w!("Segoe UI Variable Text")),
                font(18.0 * s, 400, w!("Segoe Fluent Icons")),
                font(12.0 * s, 400, w!("Segoe Fluent Icons")),
            ];
        }
        if g.mem == 0 || g.size != (w, h) {
            free_dib(g);
            let mem = CreateCompatibleDC(Some(hdc));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut c_void = null_mut();
            if let Ok(bmp) = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                SelectObject(mem, HGDIOBJ(bmp.0));
                SetBkMode(mem, TRANSPARENT);
                (g.mem, g.bmp, g.bits, g.size) = (mem.0 as isize, bmp.0 as isize, bits as usize, (w, h));
            }
        }
    }
}

fn free_dib(g: &mut Gdi) {
    unsafe {
        if g.mem != 0 {
            let _ = DeleteDC(HDC(g.mem as _));
            let _ = DeleteObject(HGDIOBJ(g.bmp as _));
        }
    }
    (g.mem, g.bmp, g.bits) = (0, 0, 0);
}

pub fn release(g: &mut Gdi) {
    free_dib(g);
    for f in g.fonts.iter_mut() {
        if *f != 0 { unsafe { let _ = DeleteObject(HGDIOBJ(*f as _)); } }
        *f = 0;
    }
}

/// What the window knows that the rows do not: dpi, an active drag, meter levels, keyboard focus.
pub struct View<'a> {
    pub dpi: u32,
    pub drag: &'a Option<Option<String>>,
    pub meters: &'a HashMap<Option<String>, f32>,
    pub focus: Option<usize>,
    /// (apps scrolled off the top, apps in total)
    pub scroll: (usize, usize),
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |sh: u32| (((a >> sh & 0xff) as f32) * (1.0 - t) + ((b >> sh & 0xff) as f32) * t).round() as u32;
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

pub fn render(g: &mut Gdi, v: &View, rows: &[Row], hdc: HDC) {
    let (dpi, drag, meters) = (v.dpi, v.drag, v.meters);
    let s = dpi as f32 / 96.0;
    let (w, h) = ((W * s).round() as i32, total_h(s, rows.len()));
    ensure_gdi(g, dpi, hdc, w, h);
    if g.mem == 0 { return; }
    let t = theme();
    let buf = unsafe { std::slice::from_raw_parts_mut(g.bits as *mut u32, (w * h) as usize) };
    buf.fill(t.bg);
    let (x0, x1) = slider_x(s);
    let sep = (row_top(s, 1) - GAP * s / 2.0) as i32;
    for px in (PAD * s) as i32..((W - PAD) * s) as i32 { buf[(sep * w + px) as usize] = t.border; }
    if let Some(i) = v.focus.filter(|i| *i < rows.len()) {
        let top = row_top(s, i);
        rrect(buf, w, h, ((PAD - 8.0) * s, top - 2.0 * s, (W - PAD + 8.0) * s, top + (ROW_H - 4.0) * s), 8.0 * s, mix(t.bg, t.text, 0.07));
    }
    let shown = rows.len() - 1;
    if v.scroll.1 > shown && shown > 0 {
        // A thin thumb on the right edge shows that the app list continues.
        let (top, track) = (row_top(s, 1), shown as f32 * ROW_H * s);
        let thumb = (track * shown as f32 / v.scroll.1 as f32).max(16.0 * s);
        let y = top + (track - thumb) * v.scroll.0 as f32 / (v.scroll.1 - shown) as f32;
        rrect(buf, w, h, ((W - 7.0) * s, y, (W - 4.0) * s, y + thumb), 1.5 * s, t.dim);
    }
    for (i, r) in rows.iter().enumerate() {
        let cy = center_y(s, i);
        let frac = if r.mute { 0.0 } else { r.vol.clamp(0.0, 1.0) };
        let tx = x0 + (x1 - x0) * r.vol.clamp(0.0, 1.0);
        let fill = if r.mute { t.dim } else { t.accent };
        rrect(buf, w, h, (x0, cy - 2.0 * s, x1, cy + 2.0 * s), 2.0 * s, t.track);
        if frac > 0.0 || r.mute { rrect(buf, w, h, (x0, cy - 2.0 * s, tx.max(x0 + 4.0 * s), cy + 2.0 * s), 2.0 * s, fill); }
        let lvl = meters.get(&r.key).copied().unwrap_or(0.0);
        if lvl > 0.0 {
            rrect(buf, w, h, (x0, cy + 13.0 * s, x0 + (x1 - x0) * lvl, cy + 15.0 * s), 1.0 * s, if r.mute { t.track } else { t.accent });
        }
        circle(buf, w, h, (tx, cy), 10.0 * s, t.ring);
        circle(buf, w, h, (tx, cy), 9.0 * s, t.card);
        circle(buf, w, h, (tx, cy), 5.0 * s * if drag.as_ref().is_some_and(|k| *k == r.key) { 0.8 } else { 1.0 }, fill);
    }
    let dc = HDC(g.mem as _);
    let [f_text, f_title, f_glyph, f_small] = g.fonts;
    for (i, r) in rows.iter().enumerate() {
        let (top, cy) = (row_top(s, i), center_y(s, i));
        let is_dev = r.key.is_none();
        let mut tx = PAD * s;
        if !is_dev {
            let icon = r.path.as_deref().and_then(|p| icon_for(g, p));
            let sz = (20.0 * s) as i32;
            match icon {
                Some(ic) => unsafe { let _ = DrawIconEx(dc, tx as i32, (top + 3.0 * s) as i32, ic, sz, sz, 0, None, DI_NORMAL); },
                None => draw_text(dc, f_small, t.dim, "\u{E767}", rc(tx, top, tx + 20.0 * s, top + 26.0 * s), DT_CENTER | DT_VCENTER),
            }
            tx += 28.0 * s;
        }
        let right = (W - PAD) * s - if r.pinned { 22.0 * s } else { 0.0 };
        let (cl, cr) = chip_x(s);
        let name_right = if r.device.is_some() { cl - 6.0 * s } else { right };
        draw_text(dc, if is_dev { f_title } else { f_text }, t.text, &r.name, rc(tx, top, name_right, top + 26.0 * s), DT_VCENTER | DT_END_ELLIPSIS);
        if let Some(dev) = &r.device {
            draw_text(dc, f_small, t.dim, "\u{E70D}", rc(cr - 14.0 * s, top, cr, top + 26.0 * s), DT_CENTER | DT_VCENTER);
            draw_text(dc, f_text, t.dim, dev, rc(cl, top, cr - 16.0 * s, top + 26.0 * s), DT_RIGHT | DT_VCENTER | DT_END_ELLIPSIS);
        }
        if r.pinned {
            draw_text(dc, f_small, t.dim, "\u{E718}", rc(right + 4.0 * s, top, (W - PAD) * s, top + 26.0 * s), DT_RIGHT | DT_VCENTER);
        }
        let g = char::from_u32(glyph_for(r.vol, r.mute) as u32).unwrap().to_string();
        draw_text(dc, f_glyph, if r.mute { t.dim } else { t.text }, &g, rc(PAD * s, cy - 16.0 * s, PAD * s + 32.0 * s, cy + 16.0 * s), DT_CENTER | DT_VCENTER);
        draw_text(dc, f_text, t.dim, &format!("{}", (r.vol * 100.0).round() as i32), rc(x1 + 12.0 * s, cy - 16.0 * s, (W - PAD) * s, cy + 16.0 * s), DT_RIGHT | DT_VCENTER);
    }
    if rows.len() <= 1 {
        let top = row_top(s, 1) - GAP * s / 2.0;
        draw_text(dc, f_text, t.dim, "No apps are playing sound", rc(PAD * s, top, (W - PAD) * s, top + EMPTY_H * s), DT_CENTER | DT_VCENTER);
    }
    unsafe { let _ = BitBlt(hdc, 0, 0, w, h, Some(dc), 0, 0, SRCCOPY); }
}

#[cfg(test)]
mod tests {
    use super::level;

    #[test]
    fn level_is_a_60_db_scale() {
        assert_eq!(level(0.0), 0.0);
        assert_eq!(level(1.0), 1.0);
        assert!((level(0.1) - 2.0 / 3.0).abs() < 1e-5); // -20 dB
        assert_eq!(level(0.0005), 0.0); // below -60 dB
    }
}
