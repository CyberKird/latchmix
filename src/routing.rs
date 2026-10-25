//! Per-app output device ("Sound settings > App volume and device preferences").
//! Uses the same undocumented WinRT factory EarTrumpet uses. Only the layout that ships
//! with Windows 10 21H2 and later is supported; on anything else `available()` is false
//! and the UI hides the device button instead of guessing at a vtable.
#![allow(non_snake_case)] // COM method names mirror the Windows headers
use std::{ffi::c_void, ptr::null_mut};
use windows::{
    core::*,
    Win32::{Media::Audio::*, System::WinRT::RoGetActivationFactory},
};

const MMDEVAPI: &str = "\\\\?\\SWD#MMDEVAPI#";
const RENDER: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";

// IInspectable's three methods and 19 more (volume groups, chat apps) precede the endpoint
// calls. None of them is ever called, they only keep the vtable slots in place.
#[interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactory: IUnknown {
    unsafe fn i1(&self) -> HRESULT;
    unsafe fn i2(&self) -> HRESULT;
    unsafe fn i3(&self) -> HRESULT;
    unsafe fn p1(&self) -> HRESULT;
    unsafe fn p2(&self) -> HRESULT;
    unsafe fn p3(&self) -> HRESULT;
    unsafe fn p4(&self) -> HRESULT;
    unsafe fn p5(&self) -> HRESULT;
    unsafe fn p6(&self) -> HRESULT;
    unsafe fn p7(&self) -> HRESULT;
    unsafe fn p8(&self) -> HRESULT;
    unsafe fn p9(&self) -> HRESULT;
    unsafe fn p10(&self) -> HRESULT;
    unsafe fn p11(&self) -> HRESULT;
    unsafe fn p12(&self) -> HRESULT;
    unsafe fn p13(&self) -> HRESULT;
    unsafe fn p14(&self) -> HRESULT;
    unsafe fn p15(&self) -> HRESULT;
    unsafe fn p16(&self) -> HRESULT;
    unsafe fn p17(&self) -> HRESULT;
    unsafe fn p18(&self) -> HRESULT;
    unsafe fn p19(&self) -> HRESULT;
    unsafe fn SetPersistedDefaultAudioEndpoint(&self, pid: u32, flow: EDataFlow, role: ERole, device: *mut c_void) -> HRESULT;
    unsafe fn GetPersistedDefaultAudioEndpoint(&self, pid: u32, flow: EDataFlow, role: ERole, device: *mut *mut c_void) -> HRESULT;
}

fn factory() -> Option<IAudioPolicyConfigFactory> {
    unsafe { RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig")).ok() }
}

pub fn available() -> bool {
    factory().is_some()
}

/// Send the given processes to a device, or back to the system default with `None`.
pub fn set(pids: &[u32], device_id: Option<&str>) {
    let Some(f) = factory() else { return };
    let h = device_id.map(|id| HSTRING::from(format!("{MMDEVAPI}{id}{RENDER}")));
    let raw = h.as_ref().map_or(null_mut(), |h| unsafe { std::mem::transmute_copy::<HSTRING, *mut c_void>(h) });
    for &pid in pids {
        unsafe {
            let _ = f.SetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, raw);
            let _ = f.SetPersistedDefaultAudioEndpoint(pid, eRender, eConsole, raw);
        }
    }
}

/// The device a process is pinned to, or None when it follows the system default.
pub fn get(pid: u32) -> Option<String> {
    let f = factory()?;
    let mut raw: *mut c_void = null_mut();
    unsafe {
        f.GetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, &mut raw).ok().ok()?;
        // Ownership of the returned HSTRING passes to us; dropping it frees it.
        let h: HSTRING = std::mem::transmute::<*mut c_void, HSTRING>(raw);
        let s = h.to_string();
        let id = s.strip_prefix(MMDEVAPI)?.strip_suffix(RENDER)?;
        Some(id.to_string())
    }
}
