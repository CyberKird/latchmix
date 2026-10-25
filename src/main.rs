#![windows_subsystem = "windows"]
mod app;
mod audio;
mod draw;
mod store;
mod tray;
mod place;
mod routing;
mod ui;

use audio::{take_events, Audio, EV_SESSIONS, EV_TOPOLOGY, EV_VOLUME, WM_AUDIO};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        System::{Com::*, LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
        UI::WindowsAndMessaging::*,
    },
};

const TIMER_TOPOLOGY: usize = 1;
const WM_SHOW_FLYOUT: u32 = WM_APP + 3;
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn on_audio(hwnd: HWND) {
    let ev = take_events();
    if ev & EV_TOPOLOGY != 0 {
        // Default-device changes arrive in bursts; rebuild once after they settle.
        unsafe { SetTimer(Some(hwnd), TIMER_TOPOLOGY, 250, None) };
    }
    if ev & EV_SESSIONS != 0 {
        app::with(|a| a.audio.sync_sessions());
    }
    if ev & (EV_SESSIONS | EV_VOLUME) != 0 {
        app::with(|a| a.enforce());
        tray::update(hwnd, false);
        ui::refresh();
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_AUDIO => on_audio(hwnd),
        WM_SHOW_FLYOUT => {
            let mut p = POINT::default();
            unsafe { let _ = GetCursorPos(&mut p); }
            ui::toggle(p);
        }
        WM_TIMER if wp.0 == TIMER_TOPOLOGY => {
            unsafe { let _ = KillTimer(Some(hwnd), TIMER_TOPOLOGY); }
            app::with(|a| { a.audio.rebuild(); a.enforce(); });
            tray::update(hwnd, false);
            ui::refresh();
        }
        tray::WM_TRAY => match (l.0 & 0xffff) as u32 {
            NIN_SELECT | NIN_KEYSELECT => {
                let mut p = POINT::default();
                unsafe { let _ = GetCursorPos(&mut p); }
                ui::toggle(p);
            }
            WM_MBUTTONUP => {
                app::with(|a| { a.toggle_mute(None); a.prefs.save(); });
                tray::update(hwnd, false);
                ui::refresh();
            }
            WM_CONTEXTMENU => {
                if tray::show_menu(hwnd) {
                    unsafe { PostQuitMessage(0) };
                }
            }
            _ => {}
        },
        WM_SETTINGCHANGE => tray::update(hwnd, false),
        m if m == TASKBAR_CREATED.load(Relaxed) => tray::update(hwnd, true),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, l) },
    }
    LRESULT(0)
}

fn main() -> Result<()> {
    unsafe {
        let _single = CreateMutexW(None, true, w!("Local\\LatchmixSingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Launched again (Start menu, shortcut): show the flyout of the running copy.
            if let Ok(h) = FindWindowW(w!("LatchmixMain"), None) {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = PostMessageW(Some(h), WM_SHOW_FLYOUT, WPARAM(0), LPARAM(0));
            }
            return Ok(());
        }
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        tray::follow_system_menu_theme();
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);

        let hinst = GetModuleHandleW(None)?;
        let class = w!("LatchmixMain");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() });
        // A hidden top-level window, not message-only: it must receive TaskbarCreated and setting broadcasts.
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("Latchmix"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
        ui::create()?;

        app::install(app::App::new(Audio::new(hwnd)?, store::Prefs::load()));
        app::with(|a| a.enforce());
        tray::update(hwnd, true);
        tray::init_autostart();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        tray::remove(hwnd);
        app::uninstall();
        CoUninitialize();
    }
    Ok(())
}
