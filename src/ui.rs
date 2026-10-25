//! The flyout: one borderless window painted with GDI into a memory DIB.
//! Shapes are anti-aliased with a small signed-distance rasteriser, text uses ClearType.
//! Nothing runs while the flyout is hidden (no timers, DIB and fonts are freed).
use crate::{
    app::{self, Row},
    draw::*,
    place::{place, side_of, slide, Side},
};
use std::{cell::RefCell, collections::HashMap, ffi::c_void, mem::size_of, time::{Duration, Instant}};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP},
            WindowsAndMessaging::*,
        },
    },
};

const CLASS: PCWSTR = w!("LatchmixFlyout");
const TIMER_ANIM: usize = 1;
const TIMER_METER: usize = 2;
const ANIM: Duration = Duration::from_millis(150);


#[derive(Default)]
struct Ui {
    hwnd: isize,
    visible: bool,
    dpi: u32,
    nrows: usize,
    drag: Option<Option<String>>,
    last_pct: Option<u8>,
    last_hide: Option<Instant>,
    menu_open: bool,
    focus: usize,
    kbd: bool,
    anim: Option<Instant>,
    pos: (i32, i32),
    work: RECT,
    side: Side,
    gdi: Gdi,
    meters: HashMap<Option<String>, f32>,
}

thread_local!(static UI: RefCell<Ui> = RefCell::new(Ui::default()));

fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|c| c.try_borrow_mut().ok().map(|mut u| f(&mut u)))
}


/// Rows that fit the work area. Extra apps are cut off (no scrolling yet).
fn rows() -> Option<Vec<Row>> {
    let mut rows = app::with(|a| a.rows())?;
    let (s, avail) = ui(|u| {
        let s = u.dpi as f32 / 96.0;
        (s, (u.work.bottom - u.work.top) as f32 - 2.0 * EDGE * s)
    })?;
    while rows.len() > 1 && total_h(s, rows.len()) as f32 > avail {
        rows.pop();
    }
    Some(rows)
}

// ---- show / hide ----------------------------------------------------------

fn place_for(u: &Ui, h: i32) -> (i32, i32) {
    let s = u.dpi as f32 / 96.0;
    place(u.work, u.side, (W * s) as i32, h, (EDGE * s) as i32)
}

pub fn is_visible() -> bool {
    ui(|u| u.visible).unwrap_or(false)
}

pub fn toggle(cursor: POINT) {
    match ui(|u| (u.visible, u.last_hide.is_some_and(|t| t.elapsed() < Duration::from_millis(250)))) {
        Some((true, _)) => hide(),
        Some((false, true)) | None => {}
        Some((false, false)) => show(cursor),
    }
}

fn show(cursor: POINT) {
    ui(|u| unsafe {
        let mon = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96, 96);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        if u.dpi != dx { release(&mut u.gdi); }
        u.dpi = dx;
        u.work = mi.rcWork;
        u.side = side_of(mi.rcMonitor, mi.rcWork);
    });
    let Some(nrows) = rows().map(|r| r.len()) else { return };
    let Some((hwnd, x, y, w, h)) = ui(|u| {
        u.nrows = nrows;
        let s = u.dpi as f32 / 96.0;
        let h = total_h(s, nrows);
        u.pos = place_for(u, h);
        u.visible = true;
        (u.focus, u.kbd) = (0, false);
        u.anim = Some(Instant::now());
        (u.hwnd, u.pos.0, u.pos.1, (W * s).round() as i32, h)
    }) else { return };
    unsafe {
        let hwnd = HWND(hwnd as _);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(Some(hwnd), TIMER_ANIM, 15, None);
        SetTimer(Some(hwnd), TIMER_METER, 33, None);
    }
}

pub fn hide() {
    let Some(hwnd) = ui(|u| {
        u.visible = false;
        u.drag = None;
        u.anim = None;
        u.meters.clear();
        u.last_hide = Some(Instant::now());
        u.hwnd
    }) else { return };
    unsafe {
        let hwnd = HWND(hwnd as _);
        let _ = KillTimer(Some(hwnd), TIMER_ANIM);
        let _ = KillTimer(Some(hwnd), TIMER_METER);
        let _ = ReleaseCapture();
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    ui(|u| release(&mut u.gdi));
}

/// Called after the audio model changed: repaint, and resize if the number of rows changed.
pub fn refresh() {
    if !is_visible() { return; }
    let Some(nrows) = rows().map(|r| r.len()) else { return };
    let moved = ui(|u| {
        let changed = u.nrows != nrows;
        u.nrows = nrows;
        let s = u.dpi as f32 / 96.0;
        let h = total_h(s, nrows);
        u.pos = place_for(u, h);
        changed.then_some((u.hwnd, u.pos, (W * s).round() as i32, h))
    }).flatten();
    unsafe {
        let hwnd = match moved {
            Some((hwnd, (x, y), w, h)) => {
                let hwnd = HWND(hwnd as _);
                let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                hwnd
            }
            None => HWND(ui(|u| u.hwnd).unwrap_or(0) as _),
        };
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

// ---- input ----------------------------------------------------------------

fn lparam_xy(l: LPARAM) -> (f32, f32) {
    ((l.0 & 0xffff) as i16 as f32, ((l.0 >> 16) & 0xffff) as i16 as f32)
}

fn slider_pct(s: f32, x: f32) -> u8 {
    let (x0, x1) = slider_x(s);
    (((x - x0) / (x1 - x0)) * 100.0).round().clamp(0.0, 100.0) as u8
}

fn mouse_down(hwnd: HWND, l: LPARAM) {
    let (x, y) = lparam_xy(l);
    let Some(s) = ui(|u| u.dpi as f32 / 96.0) else { return };
    let Some(rows) = rows() else { return };
    match hit(s, &rows, x, y) {
        Some((i, Part::Mute)) => {
            app::with(|a| { a.toggle_mute(rows[i].key.as_deref()); a.prefs.save(); });
            refresh();
        }
        Some((i, Part::Slider)) => {
            ui(|u| { u.drag = Some(rows[i].key.clone()); u.last_pct = None; });
            unsafe { SetCapture(hwnd); }
            drag_to(rows[i].key.as_deref(), s, x);
        }
        Some((i, Part::Device)) => {
            if let Some(key) = rows[i].key.clone() { route_menu(hwnd, &key, s, i); }
        }
        _ => {}
    }
}

/// Pick the output device for one app. Windows stores the choice, so nothing is saved here.
fn route_menu(hwnd: HWND, key: &str, s: f32, i: usize) {
    let Some((devices, current)) = app::with(|a| a.route_choices(key)) else { return };
    let wide = |t: &str| -> Vec<u16> { t.encode_utf16().chain([0]).collect() };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, check(current.is_none()), 1, PCWSTR(wide("Default (follow Windows)").as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        for (k, (id, name)) in devices.iter().enumerate() {
            let _ = AppendMenuW(menu, check(current.as_deref() == Some(id)), 100 + k, PCWSTR(wide(name).as_ptr()));
        }
        let mut p = POINT { x: chip_x(s).0 as i32, y: (row_top(s, i) + 26.0 * s) as i32 };
        let _ = ClientToScreen(hwnd, &mut p);
        ui(|u| u.menu_open = true);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, Some(0), hwnd, None).0 as usize;
        ui(|u| u.menu_open = false);
        let _ = DestroyMenu(menu);
        match cmd {
            1 => { app::with(|a| a.route(key, None)); }
            c if c >= 100 => { app::with(|a| a.route(key, devices.get(c - 100).map(|d| d.0.as_str()))); }
            _ => {}
        }
    }
    refresh();
}

fn drag_to(key: Option<&str>, s: f32, x: f32) {
    let pct = slider_pct(s, x);
    if ui(|u| std::mem::replace(&mut u.last_pct, Some(pct)) != Some(pct)).unwrap_or(false) {
        app::with(|a| a.pin_vol(key, pct));
        refresh();
    }
}

fn mouse_move(l: LPARAM) {
    let Some((Some(drag), s)) = ui(|u| (u.drag.clone(), u.dpi as f32 / 96.0)) else { return };
    drag_to(drag.as_deref(), s, lparam_xy(l).0);
}

fn mouse_up() {
    if ui(|u| { u.last_pct = None; u.drag.take().is_some() }).unwrap_or(false) {
        unsafe { let _ = ReleaseCapture(); }
        app::with(|a| a.prefs.save());
        refresh();
    }
}

fn wheel(hwnd: HWND, wp: WPARAM, l: LPARAM) {
    let delta = (wp.0 >> 16) as i16 as i32 / 120;
    let mut p = POINT { x: (l.0 & 0xffff) as i16 as i32, y: ((l.0 >> 16) & 0xffff) as i16 as i32 };
    unsafe { let _ = ScreenToClient(hwnd, &mut p); }
    let Some(s) = ui(|u| u.dpi as f32 / 96.0) else { return };
    let Some(rows) = rows() else { return };
    if let Some((i, _)) = hit(s, &rows, p.x as f32, p.y as f32) {
        let pct = ((rows[i].vol * 100.0).round() as i32 + delta * 2).clamp(0, 100) as u8;
        app::with(|a| { a.pin_vol(rows[i].key.as_deref(), pct); a.prefs.save(); });
        refresh();
    }
}

fn context_menu(hwnd: HWND, l: LPARAM) {
    let (x, y) = lparam_xy(l);
    let Some(s) = ui(|u| u.dpi as f32 / 96.0) else { return };
    let Some(rows) = rows() else { return };
    let Some((i, _)) = hit(s, &rows, x, y) else { return };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let flags = if rows[i].pinned { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, flags, 1, w!("Forget saved volume and mute"));
        let mut p = POINT { x: x as i32, y: y as i32 };
        let _ = ClientToScreen(hwnd, &mut p);
        ui(|u| u.menu_open = true);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, Some(0), hwnd, None);
        ui(|u| u.menu_open = false);
        let _ = DestroyMenu(menu);
        if cmd.0 == 1 {
            app::with(|a| { a.forget(rows[i].key.as_deref()); a.prefs.save(); });
            refresh();
        }
    }
}

/// Arrows move between rows and change volume, space or M mutes, Enter opens the device menu.
fn key_down(hwnd: HWND, vk: u16) {
    let Some(rows) = rows() else { return };
    let Some((focus, s)) = ui(|u| {
        u.kbd = true;
        u.focus = u.focus.min(rows.len() - 1);
        (u.focus, u.dpi as f32 / 96.0)
    }) else { return };
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let key = rows[focus].key.clone();
    match vk {
        v if v == VK_DOWN.0 || (v == VK_TAB.0 && !shift) => { ui(|u| u.focus = (focus + 1) % rows.len()); }
        v if v == VK_UP.0 || v == VK_TAB.0 => { ui(|u| u.focus = (focus + rows.len() - 1) % rows.len()); }
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
            let step = if shift { 10 } else { 2 } * if v == VK_LEFT.0 { -1 } else { 1 };
            let pct = ((rows[focus].vol * 100.0).round() as i32 + step).clamp(0, 100) as u8;
            app::with(|a| { a.pin_vol(key.as_deref(), pct); a.prefs.save(); });
        }
        v if v == VK_SPACE.0 || v == b'M' as u16 => { app::with(|a| { a.toggle_mute(key.as_deref()); a.prefs.save(); }); }
        v if v == VK_RETURN.0 => {
            if let (Some(k), true) = (key.as_deref(), rows[focus].device.is_some()) { route_menu(hwnd, k, s, focus); }
        }
        _ => {}
    }
    refresh();
}

/// Poll the peaks (only while the flyout is open) and repaint when a bar moved.
fn meter_tick(hwnd: HWND) {
    let Some(peaks) = app::with(|a| a.peaks()) else { return };
    let moved = ui(|u| {
        let mut moved = false;
        let mut next = HashMap::new();
        for (key, p) in peaks {
            let prev = u.meters.get(&key).copied().unwrap_or(0.0);
            // Rise at once, fall smoothly.
            let v = level(p).max(prev * 0.88);
            let v = if v < 0.01 { 0.0 } else { v };
            moved |= (v - prev).abs() > 0.004;
            next.insert(key, v);
        }
        u.meters = next;
        moved
    });
    if moved == Some(true) {
        unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
    }
}

fn animate(hwnd: HWND) {
    let Some((start, (x, y), s, side)) = ui(|u| u.anim.map(|a| (a, u.pos, u.dpi as f32 / 96.0, u.side))).flatten() else {
        unsafe { let _ = KillTimer(Some(hwnd), TIMER_ANIM); }
        return;
    };
    let t = (start.elapsed().as_secs_f32() / ANIM.as_secs_f32()).min(1.0);
    let e = 1.0 - (1.0 - t).powi(3);
    let (dx, dy) = slide(side);
    let off = 14.0 * s * (1.0 - e);
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (255.0 * e) as u8, LWA_ALPHA);
        let _ = SetWindowPos(hwnd, None, x + (off * dx) as i32, y + (off * dy) as i32, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        if t >= 1.0 {
            let _ = KillTimer(Some(hwnd), TIMER_ANIM);
            ui(|u| u.anim = None);
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(rows) = rows() {
                ui(|u| {
                    let v = View { dpi: u.dpi, drag: &u.drag, meters: &u.meters, focus: u.kbd.then_some(u.focus) };
                    render(&mut u.gdi, &v, &rows, hdc)
                });
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => { mouse_down(hwnd, l); LRESULT(0) }
        WM_MOUSEMOVE => { mouse_move(l); LRESULT(0) }
        WM_LBUTTONUP => { mouse_up(); LRESULT(0) }
        WM_MOUSEWHEEL => { wheel(hwnd, wp, l); LRESULT(0) }
        WM_RBUTTONUP => { context_menu(hwnd, l); LRESULT(0) }
        WM_TIMER if wp.0 == TIMER_ANIM => { animate(hwnd); LRESULT(0) }
        WM_TIMER if wp.0 == TIMER_METER => { meter_tick(hwnd); LRESULT(0) }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => { hide(); LRESULT(0) }
        WM_KEYDOWN => { key_down(hwnd, wp.0 as u16); LRESULT(0) }
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            if !ui(|u| u.menu_open).unwrap_or(false) { hide(); }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, l) },
    }
}

pub fn create() -> Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            style: CS_DROPSHADOW,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED, CLASS, w!("Volume"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32);
        ui(|u| { u.hwnd = hwnd.0 as isize; u.dpi = 96; });
    }
    Ok(())
}
