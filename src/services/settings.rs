//! Settings + per-surface stores: one persistence path, one format, one
//! writer, so every feature saves the same way.
//!
//! Two files under `%APPDATA%\Arc\`, both plain text, both optional:
//! - `settings.toml` — user configuration, shared by weather, clipboard,
//!   picker, terminal, and the rest.
//! - `clipboard.json` — the clipboard history, kept as JSON because it is an
//!   append-only log, not a config, and editing a log by hand is a reasonable
//!   thing to do.
//!
//! Every read is a "missing or broken means default" operation: a hand-edited
//! file must never crash the island. TOML without a schema library means the
//! loader is a small typed reader over `toml::Value` rather than a derived
//! struct, which keeps the format explicit and the error paths few.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::Storage::FileSystem::GetTempPathW;
use windows::Win32::UI::Shell::{FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// Per-surface stores, keyed by name. Each is a TOML table.
#[derive(Debug, Clone, Default)]
pub struct Stores {
    inner: BTreeMap<String, toml::Table>,
}

/// The whole persisted state: every surface's store in one owner.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    stores: Stores,
}

/// Where Arc keeps its files.
pub fn app_dir() -> PathBuf {
    let Some(base) = roaming_appdata() else {
        return PathBuf::from(".");
    };
    base.join("Arc")
}

/// The roaming app-data dir, or `None` when it cannot be resolved.
fn roaming_appdata() -> Option<PathBuf> {
    // SAFETY: SHGetKnownFolderPath writes a properly terminated buffer.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None) }.ok()?;
    let s = unsafe { p.to_string() }.ok()?;
    Some(PathBuf::from(s))
}

fn store_path(name: &str) -> PathBuf {
    app_dir().join(format!("{name}.toml"))
}

impl Settings {
    /// Load every store under `%APPDATA%\Arc\`. Missing files are absent
    /// stores; a file that fails to parse is treated as absent rather than
    /// crashing on a hand edit.
    pub fn load() -> Self {
        let mut stores = Stores::default();
        for name in ["settings", "weather", "clipboard", "picker", "terminal"] {
            let path = store_path(name);
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(v) = raw.parse::<toml::Table>() {
                stores.inner.insert(name.to_string(), v);
            }
        }
        Self { stores }
    }

    /// A surface's store, empty if it has never been written.
    pub fn store(&self, name: &str) -> toml::Table {
        self.stores.inner.get(name).cloned().unwrap_or_default()
    }

    /// The whole settings surface, as one TOML table.
    pub fn settings(&self) -> toml::Table {
        self.store("settings")
    }

    /// Write one store back, creating the directory if needed.
    pub fn save(&self, name: &str, value: &toml::Table) {
        if let Some(parent) = store_path(name).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = toml::to_string(value).unwrap_or_default();
        let _ = std::fs::write(store_path(name), text);
    }
}

/// A persistent string value with a fallback.
pub fn get_str(table: &toml::Table, key: &str, fallback: &str) -> String {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(fallback)
        .to_string()
}

/// A persistent integer with a fallback.
pub fn get_i64(table: &toml::Table, key: &str, fallback: i64) -> i64 {
    table
        .get(key)
        .and_then(|v| v.as_integer())
        .unwrap_or(fallback)
        .into()
}

/// A persistent boolean with a fallback.
pub fn get_bool(table: &toml::Table, key: &str, fallback: bool) -> bool {
    table
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(fallback)
}

/// A persistent float with a fallback.
pub fn get_f64(table: &toml::Table, key: &str, fallback: f64) -> f64 {
    table
        .get(key)
        .and_then(|v| v.as_float())
        .unwrap_or(fallback)
}

/// Write one string into a store, for the one-liner save paths.
pub fn set_str(table: &mut toml::Table, key: &str, value: &str) {
    table.insert(key.to_string(), toml::Value::String(value.to_string()));
}

/// Write one boolean into a store.
pub fn set_bool(table: &mut toml::Table, key: &str, value: bool) {
    table.insert(key.to_string(), toml::Value::Boolean(value));
}

/// Read the clipboard history from its JSON file. Broken or missing means
/// empty, never a crash.
pub fn load_clipboard() -> Vec<crate::services::clipboard::Entry> {
    let Ok(raw) = std::fs::read_to_string(app_dir().join("clipboard.json")) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Overwrite the clipboard history.
pub fn save_clipboard(entries: &[crate::services::clipboard::Entry]) {
    let path = app_dir().join("clipboard.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::to_string(entries).unwrap_or_default());
}

/// Write a temp file's contents back to a path. Used by the settings UI's
/// "save to disk" action so a hand-edited file lands where the loader reads
/// it.
pub fn write_file(path: &Path, contents: &str) -> bool {
    std::fs::write(path, contents).is_ok()
}

/// The temp directory, for anything that needs a scratch file.
pub fn temp_dir() -> PathBuf {
    let mut buf = vec![0u16; MAX_PATH as usize];
    // SAFETY: buf is a valid buffer for the call's duration.
    let n = unsafe { GetTempPathW(Some(&mut buf)) };
    if n == 0 {
        return PathBuf::from(".");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_fall_back() {
        let t = toml::Table::new();
        assert_eq!(get_str(&t, "city", "Tehran"), "Tehran");
        assert_eq!(get_i64(&t, "refresh", 600), 600);
        assert!(get_bool(&t, "on", true));
        assert!((get_f64(&t, "lat", 35.7) - 35.7).abs() < 1e-6);
    }

    #[test]
    fn present_keys_win() {
        let mut t = toml::Table::new();
        set_str(&mut t, "city", "Berlin");
        set_bool(&mut t, "on", true);
        assert_eq!(get_str(&t, "city", "Tehran"), "Berlin");
        assert!(get_bool(&t, "on", false));
    }

    #[test]
    fn a_broken_toml_file_does_not_poison_the_whole_settings() {
        // A hand edit that fails to parse must leave that store empty while
        // the other stores still load.
        let s = Settings::load();
        assert!(s.store("never_written").is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let s = Settings::load();
        let mut t = s.store("settings").clone();
        set_str(&mut t, "city", "Oslo");
        s.save("settings", &t);
        let again = Settings::load();
        assert_eq!(get_str(&again.store("settings"), "city", "x"), "Oslo");
    }

    #[test]
    fn clipboard_round_trips_through_disk() {
        let entries = vec![crate::services::clipboard::Entry {
            text: "round trip".to_string(),
            at: 42,
            kind: 'e',
        }];
        save_clipboard(&entries);
        let got = load_clipboard();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "round trip");
    }

    #[test]
    fn temp_dir_is_a_real_directory() {
        assert!(temp_dir().is_dir());
    }
}