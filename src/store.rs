use std::{collections::HashMap, fs, path::PathBuf};

/// What the user pinned for one device or app. `None` means "leave it alone".
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Pref {
    pub vol: Option<u8>,
    pub mute: Option<bool>,
}

#[derive(Default)]
pub struct Prefs {
    map: HashMap<String, Pref>,
}

/// Output volume and mute of one device.
pub fn dev_key(id: &str) -> String {
    format!("dev:{id}")
}

/// An app's pin belongs to one output device: the same app can have a different volume on each.
pub fn app_key(dev_id: &str, exe: &str) -> String {
    format!("app:{dev_id}|{exe}")
}

pub fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("latchmix"))
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("prefs.tsv"))
}

impl Prefs {
    pub fn get(&self, key: &str) -> Pref {
        self.map.get(key).copied().unwrap_or_default()
    }

    /// Pins saved by older versions were not tied to a device ("master", "app:exe"):
    /// give them to the given device, once, and drop the old keys.
    pub fn migrate(&mut self, dev_id: &str) {
        let old: Vec<String> = self.map.keys().filter(|k| *k == "master" || (k.starts_with("app:") && !k.contains('|'))).cloned().collect();
        for k in old {
            let Some(p) = self.map.remove(&k) else { continue };
            let new = if k == "master" { dev_key(dev_id) } else { app_key(dev_id, &k["app:".len()..]) };
            self.map.entry(new).or_insert(p);
        }
    }

    pub fn set_vol(&mut self, key: &str, pct: u8) {
        self.map.entry(key.to_string()).or_default().vol = Some(pct.min(100));
    }

    pub fn set_mute(&mut self, key: &str, mute: Option<bool>) {
        self.map.entry(key.to_string()).or_default().mute = mute;
        self.prune(key);
    }

    pub fn forget(&mut self, key: &str) {
        self.map.remove(key);
    }

    fn prune(&mut self, key: &str) {
        if self.map.get(key) == Some(&Pref::default()) {
            self.map.remove(key);
        }
    }

    pub fn parse(text: &str) -> Prefs {
        let mut map = HashMap::new();
        for line in text.lines() {
            let mut f = line.split('\t');
            let (Some(key), Some(vol), Some(mute)) = (f.next(), f.next(), f.next()) else { continue };
            let pref = Pref {
                vol: vol.parse::<u8>().ok().filter(|v| *v <= 100),
                mute: match mute { "1" => Some(true), "0" => Some(false), _ => None },
            };
            if !key.is_empty() && pref != Pref::default() {
                map.insert(key.to_string(), pref);
            }
        }
        Prefs { map }
    }

    pub fn serialize(&self) -> String {
        let mut keys: Vec<_> = self.map.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let p = self.map[k];
                let vol = p.vol.map_or("-".into(), |v| v.to_string());
                let mute = p.mute.map_or("-", |m| if m { "1" } else { "0" });
                format!("{k}\t{vol}\t{mute}\n")
            })
            .collect()
    }

    pub fn load() -> Prefs {
        path().and_then(|p| fs::read_to_string(p).ok()).map(|t| Prefs::parse(&t)).unwrap_or_default()
    }

    /// Write to a temp file and rename, so a crash never leaves half a file.
    pub fn save(&self) {
        let Some(p) = path() else { return };
        let _ = fs::create_dir_all(p.parent().unwrap());
        let tmp = p.with_extension("tmp");
        if fs::write(&tmp, self.serialize()).is_ok() {
            let _ = fs::rename(&tmp, &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_prune() {
        let mut p = Prefs::default();
        p.set_vol(&app_key("d1", "spotify.exe"), 30);
        p.set_mute(&dev_key("d1"), Some(true));
        p.set_mute(&app_key("d1", "gone.exe"), Some(true));
        p.set_mute(&app_key("d1", "gone.exe"), None);
        let q = Prefs::parse(&p.serialize());
        assert_eq!(q.get("app:d1|spotify.exe"), Pref { vol: Some(30), mute: None });
        assert_eq!(q.get("app:d2|spotify.exe"), Pref::default());
        assert_eq!(q.get("dev:d1"), Pref { vol: None, mute: Some(true) });
        assert_eq!(q.get("app:d1|gone.exe"), Pref::default());
    }

    #[test]
    fn old_pins_go_to_one_device_once() {
        let mut p = Prefs::parse("master\t40\t-\napp:spotify.exe\t30\t-\napp:d2|kept.exe\t10\t-\n");
        p.migrate("d1");
        assert_eq!(p.get("dev:d1").vol, Some(40));
        assert_eq!(p.get("app:d1|spotify.exe").vol, Some(30));
        assert_eq!(p.get("app:d2|kept.exe").vol, Some(10));
        assert_eq!(p.get("master"), Pref::default());
        assert_eq!(p.get("app:spotify.exe"), Pref::default());
        p.migrate("d2");
        assert_eq!(p.get("dev:d2"), Pref::default());
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let q = Prefs::parse("junk\napp:a.exe\t300\t1\napp:b.exe\t-\t-\napp:c.exe\t50\t0\n");
        assert_eq!(q.get("app:a.exe").vol, None);
        assert_eq!(q.get("app:a.exe").mute, Some(true));
        assert_eq!(q.get("app:b.exe"), Pref::default());
        assert_eq!(q.get("app:c.exe"), Pref { vol: Some(50), mute: Some(false) });
    }
}
