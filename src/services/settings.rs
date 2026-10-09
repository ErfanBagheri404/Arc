//! Persistence: one directory, one format, one rule — a missing or broken file
//! means default, never a crash on a hand edit.
//!
//! `settings.toml` holds the clipboard history today; later surfaces append
//! their own top-level keys. The reader is a small typed getter over
//! `toml::Table` rather than a derived struct, which keeps the on-disk format
//! explicit and the error paths few.

use std::path::PathBuf;

use windows::Win32::UI::Shell::{FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

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

/// The whole settings file, or an empty table when it is missing or broken.
pub fn load() -> toml::Table {
    let Ok(raw) = std::fs::read_to_string(app_dir().join("settings.toml")) else {
        return toml::Table::new();
    };
    raw.parse().unwrap_or_default()
}

/// Overwrite the settings file, creating the directory if needed.
pub fn save(table: &toml::Table) {
    let path = app_dir().join("settings.toml");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = toml::to_string(table).unwrap_or_default();
    let _ = std::fs::write(path, text);
}

/// A persistent string with a fallback.
pub fn get_str(table: &toml::Table, key: &str, fallback: &str) -> String {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(fallback)
        .to_string()
}

/// A persistent boolean with a fallback.
pub fn get_bool(table: &toml::Table, key: &str, fallback: bool) -> bool {
    table
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(fallback)
}

/// Write one string into a table, for the one-liner save paths.
pub fn set_str(table: &mut toml::Table, key: &str, value: &str) {
    table.insert(key.to_string(), toml::Value::String(value.to_string()));
}

/// Write one boolean into a table.
pub fn set_bool(table: &mut toml::Table, key: &str, value: bool) {
    table.insert(key.to_string(), toml::Value::Boolean(value));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_fall_back() {
        let t = toml::Table::new();
        assert_eq!(get_str(&t, "city", "Tehran"), "Tehran");
        assert!(!get_bool(&t, "on", false));
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
    fn a_broken_file_loads_as_empty() {
        let path = app_dir().join("settings.toml");
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        std::fs::write(&path, "not = = toml").ok();
        assert!(load().is_empty());
        std::fs::remove_file(&path).ok();
    }
}
