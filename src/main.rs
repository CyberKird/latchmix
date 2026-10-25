#![windows_subsystem = "windows"]
mod app;
mod audio;
mod draw;
mod store;
mod tray;
mod place;
mod routing;
mod ui;
mod update;

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
const TIMER_UPDATE: usize = 2;
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

fn on_update(hwnd: HWND) {
    let Some((out, manual)) = update::take() else { return };
    match out {
        update::Outcome::UpToDate if manual => {
            update::message(&format!("Latchmix {} is up to date.", update::version()), false);
        }
        update::Outcome::Available(v) => {
            let ask = format!("Latchmix {v} is available (you have {}).\n\nInstall it and restart now?", update::version());
            if !manual || update::message(&ask, true) {
                update::spawn_install(hwnd, v, manual);
            }
        }
        update::Outcome::Installed => {
            update::relaunch();
            unsafe { PostQuitMessage(0) };
        }
        update::Outcome::Failed(e) if manual => {
            update::message(&format!("The update did not work: {e}."), false);
        }
        _ => {}
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
        WM_TIMER if wp.0 == TIMER_UPDATE => {
            if update::auto_enabled() { update::spawn_check(hwnd, false); }
        }
        update::WM_UPDATE => on_update(hwnd),
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
        let mut _single = CreateMutexW(None, true, w!("Local\\LatchmixSingleInstance"))?;
        // After an update the old copy is still shutting down: wait for it instead of handing over.
        if std::env::args().any(|a| a == "--updated") {
            for _ in 0..50 {
                if GetLastError() != ERROR_ALREADY_EXISTS { break; }
                let _ = CloseHandle(_single);
                std::thread::sleep(std::time::Duration::from_millis(100));
                _single = CreateMutexW(None, true, w!("Local\\LatchmixSingleInstance"))?;
            }
        }
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
        update::clean_up();
        SetTimer(Some(hwnd), TIMER_UPDATE, update::CHECK_EVERY_MS, None);
        if update::auto_enabled() { update::spawn_check(hwnd, false); }

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
