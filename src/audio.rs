//! Core Audio model. All COM callbacks only post a coalesced message to the
//! main window; every real read or write happens on the main thread.
#![allow(non_snake_case)] // COM method names mirror the Windows headers
use crate::store::{app_key, Prefs, MASTER};
use std::{cell::RefCell, collections::HashMap, ffi::c_void, ptr, sync::atomic::{AtomicU32, Ordering::SeqCst}};
use windows::{
    core::*,
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::*,
        Media::Audio::{Endpoints::*, *},
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::{Com::*, Threading::*},
        UI::WindowsAndMessaging::*,
    },
};

pub const WM_AUDIO: u32 = WM_APP + 1;
pub const EV_TOPOLOGY: u32 = 1;
pub const EV_SESSIONS: u32 = 2;
pub const EV_VOLUME: u32 = 4;
/// Drift below this is treated as "already at the pinned value" (devices quantise).
const TOLERANCE: f32 = 0.015;

static PENDING: AtomicU32 = AtomicU32::new(0);

fn post(hwnd: isize, ev: u32) {
    // Only the first event after a drain posts; the handler drains all bits at once.
    if PENDING.fetch_or(ev, SeqCst) == 0 {
        unsafe { let _ = PostMessageW(Some(HWND(hwnd as _)), WM_AUDIO, WPARAM(0), LPARAM(0)); }
    }
}

pub fn take_events() -> u32 {
    PENDING.swap(0, SeqCst)
}

#[implement(IAudioEndpointVolumeCallback)]
struct EpCb(isize);
impl IAudioEndpointVolumeCallback_Impl for EpCb_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        post(self.0, EV_VOLUME);
        Ok(())
    }
}

#[implement(IAudioSessionEvents)]
struct SessEv(isize);
impl IAudioSessionEvents_Impl for SessEv_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> Result<()> { Ok(()) }
    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> Result<()> { Ok(()) }
    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> Result<()> {
        post(self.0, EV_VOLUME);
        Ok(())
    }
    fn OnChannelVolumeChanged(&self, _: u32, _: *const f32, _: u32, _: *const GUID) -> Result<()> { Ok(()) }
    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> Result<()> { Ok(()) }
    fn OnStateChanged(&self, state: AudioSessionState) -> Result<()> {
        // Expired means the session is gone (for example the app was moved to another device).
        post(self.0, if state == AudioSessionStateExpired { EV_SESSIONS } else { EV_VOLUME });
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> Result<()> {
        post(self.0, EV_SESSIONS);
        Ok(())
    }
}

#[implement(IAudioSessionNotification)]
struct SessNotif(isize);
impl IAudioSessionNotification_Impl for SessNotif_Impl {
    fn OnSessionCreated(&self, _: Ref<IAudioSessionControl>) -> Result<()> {
        post(self.0, EV_SESSIONS);
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DevNotif(isize);
impl IMMNotificationClient_Impl for DevNotif_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> { post(self.0, EV_TOPOLOGY); Ok(()) }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> { post(self.0, EV_TOPOLOGY); Ok(()) }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> { post(self.0, EV_TOPOLOGY); Ok(()) }
    fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> Result<()> { post(self.0, EV_TOPOLOGY); Ok(()) }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> { Ok(()) }
}

pub struct Session {
    pub key: String,
    pub name: String,
    pub path: Option<String>,
    pid: u32,
    ctl: IAudioSessionControl,
    vol: ISimpleAudioVolume,
    meter: IAudioMeterInformation,
    ev: IAudioSessionEvents,
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { let _ = self.ctl.UnregisterAudioSessionNotification(&self.ev); }
    }
}

pub struct AppRow {
    pub key: String,
    pub name: String,
    pub path: Option<String>,
    pub vol: f32,
    pub mute: bool,
    pub device_id: String,
    pub device: String,
}

pub struct Device {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    hwnd: isize,
    ep: IAudioEndpointVolume,
    meter: IAudioMeterInformation,
    epcb: IAudioEndpointVolumeCallback,
    mgr: IAudioSessionManager2,
    notif: IAudioSessionNotification,
    sessions: Vec<Session>,
}

impl Drop for Device {
    fn drop(&mut self) {
        self.sessions.clear();
        unsafe {
            let _ = self.ep.UnregisterControlChangeNotify(&self.epcb);
            let _ = self.mgr.UnregisterSessionNotification(&self.notif);
        }
    }
}

fn take_pwstr(p: PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as _)) };
    s
}

impl Device {
    fn open(hwnd: isize, dev: &IMMDevice, default_id: &str) -> Result<Device> {
        unsafe {
            let id = take_pwstr(dev.GetId()?);
            let name = dev
                .OpenPropertyStore(STGM_READ)
                .and_then(|s| s.GetValue(&PKEY_Device_FriendlyName))
                .map(|v| v.to_string())
                .unwrap_or_else(|_| "Audio device".into());
            let ep: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
            let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None)?;
            let meter: IAudioMeterInformation = dev.Activate(CLSCTX_ALL, None)?;
            let epcb: IAudioEndpointVolumeCallback = EpCb(hwnd).into();
            ep.RegisterControlChangeNotify(&epcb)?;
            let notif: IAudioSessionNotification = SessNotif(hwnd).into();
            mgr.RegisterSessionNotification(&notif)?;
            let mut d = Device { is_default: id == default_id, id, name, hwnd, ep, meter, epcb, mgr, notif, sessions: vec![] };
            d.sync_sessions();
            Ok(d)
        }
    }

    pub fn sync_sessions(&mut self) {
        self.sessions.clear();
        unsafe {
            let Ok(en) = self.mgr.GetSessionEnumerator() else { return };
            for i in 0..en.GetCount().unwrap_or(0) {
                if let Some(s) = en.GetSession(i).ok().and_then(|c| Session::open(self.hwnd, c)) {
                    self.sessions.push(s);
                }
            }
        }
    }

    pub fn vol(&self) -> f32 { unsafe { self.ep.GetMasterVolumeLevelScalar().unwrap_or(0.0) } }
    pub fn mute(&self) -> bool { unsafe { self.ep.GetMute().map(|b| b.as_bool()).unwrap_or(false) } }
    pub fn set_vol(&self, v: f32) { unsafe { let _ = self.ep.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), ptr::null()); } }
    pub fn set_mute(&self, m: bool) { unsafe { let _ = self.ep.SetMute(m, ptr::null()); } }

    /// Current output peak of the device, 0.0 to 1.0.
    pub fn peak(&self) -> f32 {
        unsafe { self.meter.GetPeakValue().unwrap_or(0.0) }
    }
}

impl Session {
    fn open(hwnd: isize, ctl: IAudioSessionControl) -> Option<Session> {
        unsafe {
            if ctl.GetState().ok()? == AudioSessionStateExpired { return None; }
            let c2: IAudioSessionControl2 = ctl.cast().ok()?;
            let vol: ISimpleAudioVolume = ctl.cast().ok()?;
            let meter: IAudioMeterInformation = ctl.cast().ok()?;
            let (key, name, path, pid) = if c2.IsSystemSoundsSession() == S_OK {
                ("system sounds".to_string(), "System sounds".to_string(), None, 0)
            } else {
                let pid = c2.GetProcessId().ok()?;
                let path = process_path(pid)?;
                let exe = path.rsplit('\\').next().unwrap_or(&path).to_lowercase();
                let name = describe(&path, &exe);
                (exe, name, Some(path), pid)
            };
            let ev: IAudioSessionEvents = SessEv(hwnd).into();
            ctl.RegisterAudioSessionNotification(&ev).ok()?;
            Some(Session { key, name, path, pid, ctl, vol, meter, ev })
        }
    }
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

thread_local!(static NAMES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new()));

fn describe(path: &str, exe: &str) -> String {
    if let Some(n) = NAMES.with(|c| c.borrow().get(path).cloned()) { return n; }
    let n = file_description(path).filter(|d| !d.trim().is_empty()).unwrap_or_else(|| {
        let file = path.rsplit('\\').next().unwrap_or(exe);
        let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
        let mut c = stem.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    });
    NAMES.with(|c| c.borrow_mut().insert(path.to_string(), n.clone()));
    n
}

fn file_description(path: &str) -> Option<String> {
    let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(w.as_ptr()), None);
        if size == 0 { return None; }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(w.as_ptr()), None, size, buf.as_mut_ptr() as *mut c_void).ok()?;
        let (mut p, mut len) = (ptr::null_mut::<c_void>(), 0u32);
        if !VerQueryValueW(buf.as_ptr() as _, w!("\\VarFileInfo\\Translation"), &mut p, &mut len).as_bool() || len < 4 {
            return None;
        }
        let t = *(p as *const [u16; 2]);
        let q: Vec<u16> = format!("\\StringFileInfo\\{:04x}{:04x}\\FileDescription", t[0], t[1]).encode_utf16().chain([0]).collect();
        if !VerQueryValueW(buf.as_ptr() as _, PCWSTR(q.as_ptr()), &mut p, &mut len).as_bool() || len == 0 {
            return None;
        }
        PCWSTR(p as *const u16).to_string().ok()
    }
}

pub struct Audio {
    hwnd: isize,
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    pub devices: Vec<Device>,
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.devices.clear();
        unsafe { let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client); }
    }
}

impl Audio {
    pub fn new(hwnd: HWND) -> Result<Audio> {
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let client: IMMNotificationClient = DevNotif(hwnd.0 as isize).into();
            enumerator.RegisterEndpointNotificationCallback(&client)?;
            let mut a = Audio { hwnd: hwnd.0 as isize, enumerator, client, devices: vec![] };
            a.rebuild();
            Ok(a)
        }
    }

    pub fn rebuild(&mut self) {
        self.devices.clear();
        unsafe {
            let default_id = self
                .enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|d| d.GetId())
                .map(take_pwstr)
                .unwrap_or_default();
            let Ok(list) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return };
            for i in 0..list.GetCount().unwrap_or(0) {
                if let Some(d) = list.Item(i).ok().and_then(|d| Device::open(self.hwnd, &d, &default_id).ok()) {
                    self.devices.push(d);
                }
            }
        }
    }

    pub fn sync_sessions(&mut self) {
        self.devices.iter_mut().for_each(Device::sync_sessions);
    }

    fn sessions_of<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Session> {
        self.devices.iter().flat_map(|d| d.sessions.iter()).filter(move |s| s.key == key)
    }

    /// One row per executable across every output device (Chrome often owns several sessions).
    /// The device shown is where it is actually playing, preferring an active session.
    pub fn apps(&self) -> Vec<AppRow> {
        let mut rows: Vec<(AppRow, bool)> = vec![];
        for d in &self.devices {
            for s in &d.sessions {
                let state = unsafe { s.ctl.GetState() };
                if state == Ok(AudioSessionStateExpired) { continue; }
                let active = state == Ok(AudioSessionStateActive);
                match rows.iter_mut().find(|(r, _)| r.key == s.key) {
                    Some((r, was_active)) => {
                        if active && !*was_active {
                            (r.device_id, r.device, *was_active) = (d.id.clone(), d.name.clone(), true);
                        }
                    }
                    None => rows.push((
                        AppRow {
                            key: s.key.clone(),
                            name: s.name.clone(),
                            path: s.path.clone(),
                            vol: unsafe { s.vol.GetMasterVolume().unwrap_or(0.0) },
                            mute: unsafe { s.vol.GetMute().map(|b| b.as_bool()).unwrap_or(false) },
                            device_id: d.id.clone(),
                            device: d.name.clone(),
                        },
                        active,
                    )),
                }
            }
        }
        let mut rows: Vec<AppRow> = rows.into_iter().map(|(r, _)| r).collect();
        rows.sort_by_key(|r| r.name.to_lowercase());
        rows
    }

    /// Peak per executable (the loudest of its sessions).
    pub fn app_peaks(&self) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = vec![];
        for s in self.devices.iter().flat_map(|d| d.sessions.iter()) {
            let p = unsafe { s.meter.GetPeakValue().unwrap_or(0.0) };
            match out.iter_mut().find(|(k, _)| *k == s.key) {
                Some(e) => e.1 = e.1.max(p),
                None => out.push((s.key.clone(), p)),
            }
        }
        out
    }

    pub fn set_app_vol(&self, key: &str, v: f32) {
        for s in self.sessions_of(key) {
            unsafe { let _ = s.vol.SetMasterVolume(v.clamp(0.0, 1.0), ptr::null()); }
        }
    }

    pub fn set_app_mute(&self, key: &str, m: bool) {
        for s in self.sessions_of(key) {
            unsafe { let _ = s.vol.SetMute(m, ptr::null()); }
        }
    }

    /// Process ids that currently own a session for this executable.
    pub fn pids(&self, key: &str) -> Vec<u32> {
        let mut v: Vec<u32> = self.sessions_of(key).map(|s| s.pid).filter(|p| *p != 0).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn default_device(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.is_default)
    }

    /// Put back anything that drifted from what the user pinned. Returns how many fixes were made.
    pub fn enforce(&self, prefs: &Prefs) -> u32 {
        let mut fixes = 0;
        for d in &self.devices {
            // The output pin follows the default device, so switching outputs keeps the same volume.
            let p = if d.is_default { prefs.get(MASTER) } else { Default::default() };
            if let Some(v) = p.vol.map(|v| v as f32 / 100.0).filter(|v| (d.vol() - v).abs() > TOLERANCE) {
                d.set_vol(v);
                fixes += 1;
            }
            if let Some(m) = p.mute.filter(|m| d.mute() != *m) {
                d.set_mute(m);
                fixes += 1;
            }
            for s in &d.sessions {
                let p = prefs.get(&app_key(&s.key));
                let cur = unsafe { s.vol.GetMasterVolume().unwrap_or(0.0) };
                if let Some(v) = p.vol.map(|v| v as f32 / 100.0).filter(|v| (cur - v).abs() > TOLERANCE) {
                    unsafe { let _ = s.vol.SetMasterVolume(v, ptr::null()); }
                    fixes += 1;
                }
                let cm = unsafe { s.vol.GetMute().map(|b| b.as_bool()).unwrap_or(false) };
                if let Some(m) = p.mute.filter(|m| cm != *m) {
                    unsafe { let _ = s.vol.SetMute(m, ptr::null()); }
                    fixes += 1;
                }
            }
        }
        fixes
    }
}

// The same undocumented interface EarTrumpet uses to change the default endpoint.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    unsafe fn u1(&self) -> HRESULT;
    unsafe fn u2(&self) -> HRESULT;
    unsafe fn u3(&self) -> HRESULT;
    unsafe fn u4(&self) -> HRESULT;
    unsafe fn u5(&self) -> HRESULT;
    unsafe fn u6(&self) -> HRESULT;
    unsafe fn u7(&self) -> HRESULT;
    unsafe fn u8(&self) -> HRESULT;
    unsafe fn u9(&self) -> HRESULT;
    unsafe fn u10(&self) -> HRESULT;
    unsafe fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
}

pub fn set_default_device(id: &str) -> Result<()> {
    unsafe {
        let pc: IPolicyConfig = CoCreateInstance(&GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9), None, CLSCTX_ALL)?;
        let w: Vec<u16> = id.encode_utf16().chain([0]).collect();
        for role in [eConsole, eMultimedia, eCommunications] {
            pc.SetDefaultEndpoint(PCWSTR(w.as_ptr()), role).ok()?;
        }
    }
    Ok(())
}
