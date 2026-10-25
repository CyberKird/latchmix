//! Notification-area icon, right-click device menu and the start-with-Windows switch.
use crate::{app, audio::set_default_device, update};
use std::{cell::RefCell, ffi::c_void, mem::size_of, ptr::null_mut};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{LibraryLoader::{GetProcAddress, LoadLibraryW}, Registry::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

pub const WM_TRAY: u32 = WM_APP + 2;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_NAME: PCWSTR = w!("Latchmix");
const CMD_AUTOSTART: usize = 1;
const CMD_QUIT: usize = 2;
const CMD_AUTOUPDATE: usize = 3;
const CMD_CHECK: usize = 4;
const CMD_DEVICE: usize = 1000;

#[derive(Default)]
struct Tray {
    icon: isize,
    bucket: i32,
    tip: String,
}

thread_local!(static TRAY: RefCell<Tray> = RefCell::new(Tray::default()));

fn make_icon(glyph: u16, light: bool) -> HICON {
    unsafe {
        let sz = GetSystemMetrics(SM_CXSMICON).max(16);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: sz, biHeight: -sz, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let font = CreateFontW(-(sz * 9 / 10), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, w!("Segoe Fluent Icons"));
        let of = SelectObject(mem, HGDIOBJ(font.0));
        SetBkColor(mem, COLORREF(0));
        SetTextColor(mem, COLORREF(0xFFFFFF));
        let mut r = RECT { left: 0, top: 0, right: sz, bottom: sz };
        let mut text = [glyph];
        DrawTextW(mem, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        let _ = GdiFlush();
        // The glyph was drawn white on black: use its brightness as alpha, tint for the taskbar theme.
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (sz * sz) as usize);
        let tint = if light { 0 } else { 0x00FF_FFFF };
        for p in px.iter_mut() {
            *p = ((*p >> 8 & 0xff) << 24) | tint;
        }
        let mask_bits = vec![0u8; (sz as usize).div_ceil(16) * 2 * sz as usize];
        let mask = CreateBitmap(sz, sz, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: bmp };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        SelectObject(mem, of);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        icon
    }
}

fn nid(hwnd: HWND, icon: isize, tip: &str) -> NOTIFYICONDATAW {
    let mut n = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: HICON(icon as _),
        ..Default::default()
    };
    for (d, s) in n.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *d = s;
    }
    n
}

/// (icon bucket, tooltip) for the current default device.
fn state() -> (i32, String) {
    app::with(|a| match a.audio.default_device() {
        Some(d) if d.mute() => (4, format!("{}: muted", d.name)),
        Some(d) => {
            let v = d.vol();
            (if v <= 0.0 { 0 } else if v < 0.34 { 1 } else if v < 0.67 { 2 } else { 3 }, format!("{}: {}%", d.name, (v * 100.0).round()))
        }
        None => (0, "No output device".into()),
    })
    .unwrap_or((0, String::new()))
}

fn glyph(bucket: i32) -> u16 {
    [0xE992, 0xE993, 0xE994, 0xE995, 0xE74F][bucket as usize]
}

/// Create the icon, or refresh it when the level, mute state, tooltip or taskbar theme changed.
pub fn update(hwnd: HWND, add: bool) {
    let (bucket, tip) = state();
    TRAY.with(|t| {
        let mut t = t.borrow_mut();
        if !add && t.icon != 0 && t.bucket == bucket && t.tip == tip {
            return;
        }
        let old = t.icon;
        t.icon = make_icon(glyph(bucket), crate::draw::light_theme(true)).0 as isize;
        (t.bucket, t.tip) = (bucket, tip);
        let n = nid(hwnd, t.icon, &t.tip);
        unsafe {
            if add {
                let _ = Shell_NotifyIconW(NIM_ADD, &n);
                let mut v = n;
                v.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &v);
            } else {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &n);
            }
            if old != 0 { let _ = DestroyIcon(HICON(old as _)); }
        }
    });
}

pub fn remove(hwnd: HWND) {
    let n = nid(hwnd, 0, "");
    unsafe { let _ = Shell_NotifyIconW(NIM_DELETE, &n); }
}

fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, RRF_RT_REG_SZ, None, None, None) == ERROR_SUCCESS }
}

fn autostart_value() -> Option<String> {
    let mut buf = [0u16; 520];
    let mut bytes = (buf.len() * 2) as u32;
    unsafe {
        let r = RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut bytes));
        if r != ERROR_SUCCESS { return None; }
    }
    let len = (bytes as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Start with Windows is on by default (the point of the app is that settings survive a
/// restart): a marker file makes the first run turn it on, so a later "off" in the menu is
/// never overridden. If it is on but points at an old location of the exe, repoint it.
pub fn init_autostart() {
    let Some(dir) = crate::store::dir() else { return };
    let marker = dir.join("first-run");
    if !marker.exists() {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::write(&marker, "").is_ok() {
            set_autostart(true);
        }
    } else if let (Some(have), Ok(exe)) = (autostart_value(), std::env::current_exe()) {
        if have != format!("\"{}\"", exe.display()) {
            set_autostart(true);
        }
    }
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let v: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, REG_SZ.0, Some(v.as_ptr() as *const c_void), (v.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME);
        }
    }
}

/// Returns true when the user chose Quit.
pub fn show_menu(hwnd: HWND) -> bool {
    let devices: Vec<(String, String, bool)> = app::with(|a| a.audio.devices.iter().map(|d| (d.id.clone(), d.name.clone(), d.is_default)).collect()).unwrap_or_default();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return false };
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain([0]).collect() };
        if devices.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("No output devices"));
        }
        for (i, (_, name, is_default)) in devices.iter().enumerate() {
            let flags = if *is_default { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, CMD_DEVICE + i, PCWSTR(wide(name).as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let auto = if autostart_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, auto, CMD_AUTOSTART, w!("Start with Windows"));
        let upd = if update::auto_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, upd, CMD_AUTOUPDATE, w!("Update automatically"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_CHECK, w!("Check for updates now"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RIGHTALIGN, p.x, p.y, Some(0), hwnd, None).0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd {
            CMD_AUTOSTART => set_autostart(!autostart_enabled()),
            CMD_AUTOUPDATE => {
                let on = !update::auto_enabled();
                update::set_auto(on);
                if on { update::spawn_check(hwnd, false); }
            }
            CMD_CHECK => update::spawn_check(hwnd, true),
            CMD_QUIT => return true,
            c if c >= CMD_DEVICE => {
                if let Some((id, ..)) = devices.get(c - CMD_DEVICE) {
                    let _ = set_default_device(id);
                }
            }
            _ => {}
        }
    }
    false
}

/// Ask Windows to draw our popup menus in the system theme (uxtheme ordinals 135 and 136).
pub fn follow_system_menu_theme() {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(lib, PCSTR(135 as *const u8)) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(1); // AllowDark: follow the system setting
        }
        if let Some(f) = GetProcAddress(lib, PCSTR(136 as *const u8)) {
            let flush: extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}
