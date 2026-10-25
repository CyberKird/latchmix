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

pub fn dev_key(id: &str) -> String {
    format!("dev:{id}")
}

pub fn app_key(exe: &str) -> String {
    format!("app:{exe}")
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
        p.set_vol(&app_key("spotify.exe"), 30);
        p.set_mute(&dev_key("{0.0.0}.{abc}"), Some(true));
        p.set_mute(&app_key("gone.exe"), Some(true));
        p.set_mute(&app_key("gone.exe"), None);
        let q = Prefs::parse(&p.serialize());
        assert_eq!(q.get("app:spotify.exe"), Pref { vol: Some(30), mute: None });
        assert_eq!(q.get("dev:{0.0.0}.{abc}"), Pref { vol: None, mute: Some(true) });
        assert_eq!(q.get("app:gone.exe"), Pref::default());
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
