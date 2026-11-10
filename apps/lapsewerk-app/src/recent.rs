//! The project manager's memory: the folders opened last, and the app's own settings.
//!
//! A project is its folder (`lapsewerk.json` beside the photos); this list only remembers which
//! folders were opened, when, how far they got, and a small picture for the card. It lives in the
//! app's settings folder (macOS: `~/Library/Application Support/Lapsewerk`; Linux:
//! `$XDG_CONFIG_HOME/lapsewerk`; Windows: `%APPDATA%\Lapsewerk`; `LAPSEWERK_CONFIG` overrides).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Most projects remembered.
const MAX: usize = 30;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub folder: PathBuf,
    pub name: String,
    /// Seconds since 1970.
    pub opened: u64,
    pub frames: usize,
    /// Furthest step reached (1-6).
    pub step: usize,
}

#[derive(Debug, Default)]
pub struct Recent {
    pub entries: Vec<Entry>,
    pub lang: Option<String>,
    /// The introduction was seen (or skipped).
    pub intro_seen: bool,
    dir: Option<PathBuf>,
}

/// The app's settings folder.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("LAPSEWERK_CONFIG") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/Lapsewerk"));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Lapsewerk"));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")))
        .map(|c| c.join("lapsewerk"))
}

#[must_use]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Recent {
    /// The settings folder this list lives in (`None`: kept nowhere, as in tests).
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The list from the settings folder (empty when there is none or it cannot be read).
    #[must_use]
    pub fn load() -> Self {
        Self::load_at(config_dir())
    }

    /// The list kept in `dir`.
    #[must_use]
    pub fn load_at(dir: Option<PathBuf>) -> Self {
        let mut r = Self {
            dir: dir.clone(),
            ..Default::default()
        };
        let Some(text) = dir.and_then(|d| std::fs::read_to_string(d.join("recent.json")).ok()) else {
            return r;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return r;
        };
        r.lang = v.get("lang").and_then(Value::as_str).map(str::to_owned);
        r.intro_seen = v.get("introSeen").and_then(Value::as_bool).unwrap_or(false);
        r.entries = v
            .get("projects")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some(Entry {
                            folder: PathBuf::from(p.get("folder")?.as_str()?),
                            name: p.get("name")?.as_str()?.to_owned(),
                            opened: p.get("opened").and_then(Value::as_u64).unwrap_or(0),
                            frames: p.get("frames").and_then(Value::as_u64).unwrap_or(0) as usize,
                            step: p.get("step").and_then(Value::as_u64).unwrap_or(1).clamp(1, 6) as usize,
                        })
                    })
                    .take(MAX)
                    .collect()
            })
            .unwrap_or_default();
        r
    }

    /// Writes the list (atomically). Errors are ignored: the list is a convenience.
    pub fn save(&self) {
        let Some(dir) = &self.dir else { return };
        let _ = std::fs::create_dir_all(dir);
        let projects: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                json!({
                    "folder": e.folder.to_string_lossy(),
                    "name": e.name,
                    "opened": e.opened,
                    "frames": e.frames,
                    "step": e.step,
                })
            })
            .collect();
        let v = json!({ "lang": self.lang, "introSeen": self.intro_seen, "projects": projects });
        let Ok(text) = serde_json::to_string_pretty(&v) else {
            return;
        };
        let tmp = dir.join(format!(".recent.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("recent.json"));
        }
    }

    /// Puts `folder` first (opened now), with what is known about it.
    pub fn touch(&mut self, folder: &Path, frames: usize, step: usize) {
        let old_step = self
            .entries
            .iter()
            .find(|e| e.folder == folder)
            .map_or(1, |e| e.step);
        self.entries.retain(|e| e.folder != folder);
        self.entries.insert(
            0,
            Entry {
                folder: folder.to_path_buf(),
                name: folder.file_name().map_or_else(
                    || folder.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                opened: now(),
                frames,
                step: step.max(old_step),
            },
        );
        self.entries.truncate(MAX);
    }

    pub fn remove(&mut self, folder: &Path) {
        self.entries.retain(|e| e.folder != folder);
    }

    /// Where the card picture of `folder` is kept.
    #[must_use]
    pub fn thumb_path(&self, folder: &Path) -> Option<PathBuf> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        folder.hash(&mut h);
        self.dir
            .as_ref()
            .map(|d| d.join("thumbs").join(format!("{:016x}.png", h.finish())))
    }
}

/// How long ago `opened` was, in words.
#[must_use]
pub fn ago(opened: u64, words: &[&str; 5]) -> String {
    let s = now().saturating_sub(opened);
    let [just, mins, hours, yesterday, days] = *words;
    if s < 60 {
        just.to_owned()
    } else if s < 3600 {
        crate::text::with_n(mins, s / 60)
    } else if s < 86_400 {
        crate::text::with_n(hours, s / 3600)
    } else if s < 2 * 86_400 {
        yesterday.to_owned()
    } else {
        crate::text::with_n(days, s / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_the_last_projects_first() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Recent {
            dir: Some(dir.clone()),
            ..Default::default()
        };
        r.touch(Path::new("/a/Sonnenaufgang"), 492, 3);
        r.touch(Path::new("/b/Nacht"), 169, 1);
        r.touch(Path::new("/a/Sonnenaufgang"), 492, 2);
        r.lang = Some("de".into());
        r.save();
        let back = Recent::load_at(Some(dir.clone()));
        let names: Vec<&str> = back.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Sonnenaufgang", "Nacht"]);
        assert_eq!(back.entries[0].step, 3, "the furthest step is kept");
        assert_eq!(back.lang.as_deref(), Some("de"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn says_how_long_ago() {
        let w = ["now", "{n} min", "{n} h", "yesterday", "{n} d"];
        assert_eq!(ago(now(), &w), "now");
        assert_eq!(ago(now() - 600, &w), "10 min");
        assert_eq!(ago(now() - 90_000, &w), "yesterday");
        assert_eq!(ago(now() - 3 * 86_400, &w), "3 d");
    }
}
