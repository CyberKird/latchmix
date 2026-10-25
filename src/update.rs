//! Update check and install from GitHub releases. Off until the user turns it on or asks for a
//! check; the only network traffic is to api.github.com and github.com.
use crate::store;
use std::{
    ffi::c_void,
    fs,
    path::PathBuf,
    sync::{atomic::{AtomicBool, Ordering::SeqCst}, Mutex},
    time::Duration,
};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Networking::WinHttp::*,
        Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
        UI::WindowsAndMessaging::*,
    },
};

pub const WM_UPDATE: u32 = WM_APP + 4;
pub const CHECK_EVERY_MS: u32 = 24 * 60 * 60 * 1000;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_EXE: usize = 8 << 20;

pub enum Outcome {
    UpToDate,
    Available(String),
    Installed,
    Failed(String),
}

/// An outcome plus whether the user asked for it (a manual check reports back, an automatic one is silent).
static RESULT: Mutex<Option<(Outcome, bool)>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn take() -> Option<(Outcome, bool)> {
    RESULT.lock().ok()?.take()
}

fn flag() -> Option<PathBuf> {
    Some(store::dir()?.join("auto-update"))
}

pub fn auto_enabled() -> bool {
    flag().is_some_and(|p| p.exists())
}

pub fn set_auto(on: bool) {
    let Some(p) = flag() else { return };
    if on {
        let _ = fs::create_dir_all(p.parent().unwrap());
        let _ = fs::write(p, "");
    } else {
        let _ = fs::remove_file(p);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// GET over HTTPS, following redirects (WinHTTP does that by default).
fn get(host: &str, path: &str) -> std::result::Result<Vec<u8>, String> {
    unsafe {
        let session = WinHttpOpen(w!("latchmix"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() { return Err("network unavailable".into()); }
        let r = get_on(session, host, path);
        let _ = WinHttpCloseHandle(session);
        r
    }
}

unsafe fn get_on(session: *mut c_void, host: &str, path: &str) -> std::result::Result<Vec<u8>, String> {
    unsafe {
        let _ = WinHttpSetTimeouts(session, 10_000, 10_000, 15_000, 30_000);
        let h = wide(host);
        let conn = WinHttpConnect(session, PCWSTR(h.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT as u16, 0);
        if conn.is_null() { return Err("could not connect".into()); }
        let p = wide(path);
        let req = WinHttpOpenRequest(conn, w!("GET"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
        let out = if req.is_null() { Err("could not connect".into()) } else { read_reply(req) };
        if !req.is_null() { let _ = WinHttpCloseHandle(req); }
        let _ = WinHttpCloseHandle(conn);
        out
    }
}

unsafe fn read_reply(req: *mut c_void) -> std::result::Result<Vec<u8>, String> {
    unsafe {
        let hdr: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
        WinHttpSendRequest(req, Some(&hdr), None, 0, 0, 0).map_err(|_| "request failed")?;
        WinHttpReceiveResponse(req, std::ptr::null_mut()).map_err(|_| "no response")?;
        let mut status = 0u32;
        let mut len = 4u32;
        WinHttpQueryHeaders(req, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut())
            .map_err(|_| "no status")?;
        if status != 200 { return Err(format!("server answered {status}")); }
        let mut body = vec![];
        let mut buf = [0u8; 16384];
        loop {
            let mut n = 0u32;
            WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut n).map_err(|_| "download interrupted")?;
            if n == 0 { return Ok(body); }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > MAX_EXE { return Err("download too large".into()); }
        }
    }
}

/// "0.3.2" from the release JSON's "v0.3.2", only if it is a plain dotted number (it ends up in a URL).
fn tag_of(json: &str) -> Option<String> {
    let rest = json.split("\"tag_name\"").nth(1)?;
    let rest = &rest[rest.find('"')? + 1..];
    let tag = &rest[..rest.find('"')?];
    let v = tag.strip_prefix('v')?;
    (!v.is_empty() && v.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))).then(|| v.to_string())
}

fn parts(v: &str) -> Vec<u32> {
    v.split('.').filter_map(|p| p.parse().ok()).collect()
}

fn newer(latest: &str, current: &str) -> bool {
    parts(latest) > parts(current)
}

fn sha256(data: &[u8]) -> Option<String> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out).ok().ok()? };
    Some(out.iter().map(|b| format!("{b:02x}")).collect())
}

fn check() -> Outcome {
    let body = match get("api.github.com", "/repos/CyberKird/latchmix/releases/latest") {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => return Outcome::Failed(e),
    };
    match tag_of(&body) {
        Some(v) if newer(&v, VERSION) => Outcome::Available(v),
        Some(_) => Outcome::UpToDate,
        None => Outcome::Failed("unexpected reply from GitHub".into()),
    }
}

/// Download, verify against the published SHA-256, then swap the files. A running exe can be
/// renamed but not overwritten, so the old one becomes latchmix.old (removed on the next start).
fn fetch(ver: &str) -> std::result::Result<Vec<u8>, String> {
    let base = format!("/CyberKird/latchmix/releases/download/v{ver}/latchmix.exe");
    let exe = get("github.com", &base)?;
    let sum = String::from_utf8_lossy(&get("github.com", &format!("{base}.sha256"))?).into_owned();
    let want = sum.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if exe.len() < 100_000 || !exe.starts_with(b"MZ") || sha256(&exe).as_deref() != Some(want.as_str()) {
        return Err("the downloaded file failed verification".into());
    }
    Ok(exe)
}

fn install(ver: &str) -> Outcome {
    let run = || -> std::result::Result<(), String> {
        let exe = fetch(ver)?;
        let cur = std::env::current_exe().map_err(|e| e.to_string())?;
        let (new, old) = (cur.with_extension("new"), cur.with_extension("old"));
        fs::write(&new, &exe).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&old);
        if let Err(e) = fs::rename(&cur, &old) {
            let _ = fs::remove_file(&new);
            return Err(e.to_string());
        }
        if let Err(e) = fs::rename(&new, &cur) {
            let _ = fs::rename(&old, &cur);
            return Err(e.to_string());
        }
        Ok(())
    };
    match run() {
        Ok(()) => Outcome::Installed,
        Err(e) => Outcome::Failed(e),
    }
}

fn spawn(hwnd: HWND, manual: bool, job: impl FnOnce() -> Outcome + Send + 'static) {
    if BUSY.swap(true, SeqCst) { return; }
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let out = job();
        if let Ok(mut r) = RESULT.lock() { *r = Some((out, manual)); }
        BUSY.store(false, SeqCst);
        unsafe { let _ = PostMessageW(Some(HWND(h as _)), WM_UPDATE, WPARAM(0), LPARAM(0)); }
    });
}

/// `manual` = the user asked, so the answer is shown even when there is nothing new.
pub fn spawn_check(hwnd: HWND, manual: bool) {
    spawn(hwnd, manual, move || {
        if !manual { std::thread::sleep(Duration::from_secs(20)); }
        check()
    });
}

pub fn spawn_install(hwnd: HWND, ver: String, manual: bool) {
    spawn(hwnd, manual, move || install(&ver));
}

/// Returns true for OK / Yes.
pub fn message(text: &str, ask: bool) -> bool {
    let t = wide(text);
    let flags = MB_TOPMOST | MB_SETFOREGROUND | if ask { MB_YESNO | MB_ICONQUESTION } else { MB_OK | MB_ICONINFORMATION };
    unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), w!("Latchmix"), flags) != IDNO }
}

pub fn version() -> &'static str {
    VERSION
}

/// Start the new exe; it waits for this copy to release the single-instance mutex.
pub fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--updated").spawn();
    }
}

/// Remove the exe left behind by the last update.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(exe.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tag_and_rejects_odd_ones() {
        assert_eq!(tag_of("{\"tag_name\": \"v0.3.10\",\"x\":1}").as_deref(), Some("0.3.10"));
        assert_eq!(tag_of("{\"tag_name\":\"v1.0\"}").as_deref(), Some("1.0"));
        assert_eq!(tag_of("{\"tag_name\":\"v1/../x\"}"), None);
        assert_eq!(tag_of("{\"message\":\"rate limited\"}"), None);
    }

    #[test]
    fn compares_numerically() {
        assert!(newer("0.3.10", "0.3.9"));
        assert!(newer("0.4.0", "0.3.2"));
        assert!(!newer("0.3.2", "0.3.2"));
        assert!(!newer("0.3.1", "0.3.2"));
    }

    /// Needs the network: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_check_and_download() {
        assert!(matches!(check(), Outcome::UpToDate | Outcome::Available(_)));
        assert!(fetch("0.3.2").is_ok());
    }

    #[test]
    fn sha256_of_abc() {
        assert_eq!(sha256(b"abc").as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    }
}
