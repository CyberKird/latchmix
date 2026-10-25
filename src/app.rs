//! Shared state (audio model + pinned prefs) and the operations the UI performs on it.
use crate::{
    audio::Audio,
    routing,
    store::{app_key, dev_key, Prefs},
};
use std::{cell::RefCell, time::{Duration, Instant}};

pub struct App {
    pub audio: Audio,
    pub prefs: Prefs,
    burst: u32,
    burst_start: Instant,
    pause_until: Instant,
}

thread_local!(static APP: RefCell<Option<App>> = const { RefCell::new(None) });

pub fn install(a: App) {
    APP.with(|c| *c.borrow_mut() = Some(a));
}

pub fn uninstall() {
    APP.with(|c| c.borrow_mut().take());
}

/// Short, non-reentrant access. Returns None if a borrow is already live (nested message).
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|c| c.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

/// One slider row. `key` is None for the output device, the exe name for an app.
pub struct Row {
    pub key: Option<String>,
    pub name: String,
    pub path: Option<String>,
    pub vol: f32,
    pub mute: bool,
    pub pinned: bool,
    /// Where an app plays, shown as a button. None when there is nothing to choose from.
    pub device: Option<String>,
}

/// "Speakers (Realtek Audio)" -> "Speakers": the part in brackets is the driver name.
pub fn short_name(name: &str) -> &str {
    name.split(" (").next().unwrap_or(name).trim()
}

impl App {
    pub fn new(audio: Audio, mut prefs: Prefs) -> App {
        if let Some(d) = audio.default_device() {
            prefs.migrate(&d.id);
        }
        let now = Instant::now();
        App { audio, prefs, burst: 0, burst_start: now, pause_until: now }
    }

    /// The output device an app row is on (where it plays, preferring an active session).
    fn app_device(&self, key: &str) -> Option<String> {
        self.audio.apps().into_iter().find(|a| a.key == key).map(|a| a.device_id)
    }

    fn pref_key(&self, row: Option<&str>) -> Option<String> {
        match row {
            Some(k) => self.app_device(k).map(|d| app_key(&d, k)),
            None => self.audio.default_device().map(|d| dev_key(&d.id)),
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let Some(d) = self.audio.default_device() else {
            return vec![Row { key: None, name: "No output device".into(), path: None, vol: 0.0, mute: false, pinned: false, device: None }];
        };
        let pinned = |k: String| self.prefs.get(&k) != Default::default();
        let mut rows = vec![Row {
            key: None,
            name: d.name.clone(),
            path: None,
            vol: d.vol(),
            mute: d.mute(),
            pinned: pinned(dev_key(&d.id)),
            device: None,
        }];
        let routable = self.audio.devices.len() > 1 && routing::available();
        rows.extend(self.audio.apps().into_iter().map(|a| Row {
            pinned: pinned(app_key(&a.device_id, &a.key)),
            device: routable.then(|| short_name(&a.device).to_string()),
            key: Some(a.key),
            name: a.name,
            path: a.path,
            vol: a.vol,
            mute: a.mute,
        }));
        rows
    }

    /// Raw peaks for the visible rows, same keys as `rows()`.
    pub fn peaks(&self) -> Vec<(Option<String>, f32)> {
        let Some(d) = self.audio.default_device() else { return vec![] };
        let mut v = vec![(None, d.peak())];
        v.extend(self.audio.app_peaks().into_iter().map(|(k, p)| (Some(k), p)));
        v
    }

    /// Set and pin a volume. Moving the slider while muted unmutes, like Windows does.
    pub fn pin_vol(&mut self, row: Option<&str>, pct: u8) {
        let Some(pk) = self.pref_key(row) else { return };
        let Some(d) = self.audio.default_device() else { return };
        let v = pct.min(100) as f32 / 100.0;
        let dev = row.and_then(|k| self.app_device(k)).unwrap_or_default();
        let muted = match row {
            Some(k) => {
                self.audio.set_app_vol(&dev, k, v);
                self.audio.apps().iter().any(|a| a.key == k && a.mute)
            }
            None => {
                d.set_vol(v);
                d.mute()
            }
        };
        self.prefs.set_vol(&pk, pct);
        if muted {
            match row {
                Some(k) => self.audio.set_app_mute(&dev, k, false),
                None => d.set_mute(false),
            }
            self.prefs.set_mute(&pk, None);
        }
    }

    pub fn toggle_mute(&mut self, row: Option<&str>) {
        let Some(pk) = self.pref_key(row) else { return };
        let Some(d) = self.audio.default_device() else { return };
        let dev = row.and_then(|k| self.app_device(k)).unwrap_or_default();
        let now_muted = match row {
            Some(k) => {
                let m = !self.audio.apps().iter().any(|a| a.key == k && a.mute);
                self.audio.set_app_mute(&dev, k, m);
                m
            }
            None => {
                let m = !d.mute();
                d.set_mute(m);
                m
            }
        };
        self.prefs.set_mute(&pk, Some(now_muted));
    }

    /// Output devices to offer for an app, and the one it is pinned to (None = follows default).
    pub fn route_choices(&self, key: &str) -> (Vec<(String, String)>, Option<String>) {
        let devices = self.audio.devices.iter().map(|d| (d.id.clone(), short_name(&d.name).to_string())).collect();
        let current = self.audio.pids(key).first().and_then(|p| routing::get(*p));
        (devices, current)
    }

    /// Send an app to a device (None = back to the system default). Windows remembers this itself.
    pub fn route(&self, key: &str, device_id: Option<&str>) {
        routing::set(&self.audio.pids(key), device_id);
    }

    pub fn forget(&mut self, row: Option<&str>) {
        if let Some(pk) = self.pref_key(row) {
            self.prefs.forget(&pk);
        }
    }

    /// Re-apply pinned values. A runaway fight with an app that keeps resetting itself
    /// pauses enforcement for a few seconds instead of spinning.
    pub fn enforce(&mut self) {
        let now = Instant::now();
        if now < self.pause_until {
            return;
        }
        let fixes = self.audio.enforce(&self.prefs);
        if now - self.burst_start > Duration::from_secs(1) {
            self.burst = 0;
            self.burst_start = now;
        }
        self.burst += fixes;
        if self.burst > 40 {
            self.pause_until = now + Duration::from_secs(5);
        }
    }
}
