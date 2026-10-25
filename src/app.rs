//! Shared state (audio model + pinned prefs) and the operations the UI performs on it.
use crate::{
    audio::Audio,
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
}

impl App {
    pub fn new(audio: Audio, prefs: Prefs) -> App {
        let now = Instant::now();
        App { audio, prefs, burst: 0, burst_start: now, pause_until: now }
    }

    fn pref_key(&self, row: Option<&str>) -> Option<String> {
        match row {
            Some(k) => Some(app_key(k)),
            None => self.audio.default_device().map(|d| dev_key(&d.id)),
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let Some(d) = self.audio.default_device() else {
            return vec![Row { key: None, name: "No output device".into(), path: None, vol: 0.0, mute: false, pinned: false }];
        };
        let pinned = |k: String| self.prefs.get(&k) != Default::default();
        let mut rows = vec![Row {
            key: None,
            name: d.name.clone(),
            path: None,
            vol: d.vol(),
            mute: d.mute(),
            pinned: pinned(dev_key(&d.id)),
        }];
        rows.extend(d.apps().into_iter().map(|a| Row {
            pinned: pinned(app_key(&a.key)),
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
        v.extend(d.app_peaks().into_iter().map(|(k, p)| (Some(k), p)));
        v
    }

    /// Set and pin a volume. Moving the slider while muted unmutes, like Windows does.
    pub fn pin_vol(&mut self, row: Option<&str>, pct: u8) {
        let Some(pk) = self.pref_key(row) else { return };
        let Some(d) = self.audio.default_device() else { return };
        let v = pct.min(100) as f32 / 100.0;
        let muted = match row {
            Some(k) => {
                d.set_app_vol(k, v);
                d.apps().iter().any(|a| a.key == k && a.mute)
            }
            None => {
                d.set_vol(v);
                d.mute()
            }
        };
        self.prefs.set_vol(&pk, pct);
        if muted {
            match row {
                Some(k) => d.set_app_mute(k, false),
                None => d.set_mute(false),
            }
            self.prefs.set_mute(&pk, None);
        }
    }

    pub fn toggle_mute(&mut self, row: Option<&str>) {
        let Some(pk) = self.pref_key(row) else { return };
        let Some(d) = self.audio.default_device() else { return };
        let now_muted = match row {
            Some(k) => {
                let m = !d.apps().iter().any(|a| a.key == k && a.mute);
                d.set_app_mute(k, m);
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
