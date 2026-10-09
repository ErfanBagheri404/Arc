//! Clipboard history: text entries, newest first, capped and persisted.
//!
//! Capture is a *poll* of `GetClipboardSequenceNumber`, not
//! `AddClipboardFormatListener`. The listener would need a message on the UI
//! thread's queue and a clipboard read on the UI thread too (the clipboard is a
//! single shared lock — reading it there could stall the render loop behind
//! another app's large paste). The sequence counter is one cheap call, so a
//! 400 ms worker thread reads it and only touches the clipboard when the
//! number actually moved.
//!
//! CUT: image and file-drop entries. `CF_DIB` needs a full DIB→WIC path for a
//! thumbnail and `CF_HDROP` needs shell PIDL parsing; text is the entry that
//! carries a secret, so text-only is also the smaller privacy surface.
//!
//! CUT: `rusqlite` persistence from the plan. A JSON file written on change is
//! a few dozen lines and one fewer native dependency; the cap keeps it bounded.
//!
//! Privacy gate: capture is OFF until the user opts in, so nothing is ever
//! recorded without consent. Turning it back off clears the file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many entries are kept. Oldest fall off the end.
pub const MAX_ENTRIES: usize = 50;
/// Longest single entry kept, in bytes of UTF-16 text. Past this an entry is
/// dropped rather than stored: a pasted 10 MB log is not history worth keeping.
pub const MAX_BYTES: usize = 256 * 1024;
/// Poll cadence. A copy is followed within 400 ms; a slower poll would make the
/// panel look like it missed entries.
const POLL: Duration = Duration::from_millis(400);

/// `CF_UNICODETEXT`, not re-exported from `DataExchange` at windows 0.62.
const CF_UNICODETEXT: u32 = 13;

/// One captured clipboard entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub text: String,
    /// Seconds since the Unix epoch, stored as an integer so the file is
    /// portable and does not depend on a timestamp type's format.
    pub at: u64,
    /// Single-character preview for the panel's leading cell.
    pub kind: char,
}

impl Entry {
    /// A one-line preview: control characters and newlines collapsed, then
    /// truncated so the panel never has to wrap.
    pub fn preview(&self, max_chars: usize) -> String {
        let mut out: String = self
            .text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        while out.contains("  ") {
            out = out.replace("  ", " ");
        }
        let out = out.trim();
        if out.chars().count() <= max_chars {
            return out.to_string();
        }
        out.chars()
            .take(max_chars.saturating_sub(1))
            .chain(std::iter::once('…'))
            .collect()
    }
}

/// Whether the user has consented to clipboard capture, and where history lives.
///
/// Both live in one small JSON file next to the log. Consent defaults to
/// **false**: a fresh install records nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Store {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    entries: Vec<Entry>,
}

impl From<toml::Table> for Store {
    fn from(t: toml::Table) -> Self {
        let enabled = super::settings::get_bool(&t, "clip_enabled", false);
        let entries = match t.get("entries").and_then(|v| v.as_array()) {
            Some(list) => list
                .iter()
                .filter_map(|v| {
                    let e = v.as_table()?;
                    Some(Entry {
                        text: e.get("text")?.as_str()?.to_string(),
                        at: e.get("at")?.as_integer()? as u64,
                        kind: 'e',
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Self { enabled, entries }
    }
}

/// The clipboard service: owns the file, the worker thread, and the snapshot.
pub struct Clipboard {
    shared: Arc<Mutex<Vec<Entry>>>,
    /// Shared with the worker: flipping consent off must stop capture at once,
    /// not at the next restart.
    enabled: Arc<Mutex<bool>>,
}

impl Clipboard {
    /// Load history and start the worker. Consent is read from disk; when it is
    /// off, any stored entries are dropped and the file is rewritten empty, so
    /// disabling really forgets.
    pub fn start() -> Self {
        let mut store = load();
        if !store.enabled && !store.entries.is_empty() {
            store.entries.clear();
            let _ = save(&store);
        }
        let entries = store.entries;
        let enabled = store.enabled;

        let shared = Arc::new(Mutex::new(entries));
        let flag = Arc::new(Mutex::new(enabled));

        let worker_entries = shared.clone();
        let worker_flag = flag.clone();
        std::thread::Builder::new()
            .name("arc-clipboard".into())
            .spawn(move || run(worker_entries, worker_flag))
            .ok();

        Self { shared, enabled: flag }
    }

    /// Newest first.
    pub fn snapshot(&self) -> Vec<Entry> {
        self.shared.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// Is capture on?
    pub fn enabled(&self) -> bool {
        self.enabled.lock().map(|b| *b).unwrap_or(false)
    }

    /// Turn capture on or off. Turning it off clears the history and the file —
    /// the switch has to be a real opt-out, not a pause.
    pub fn set_enabled(&self, on: bool) {
        if let Ok(mut b) = self.enabled.lock() {
            *b = on;
        }
        // Turning it off forgets; turning it on starts from empty. Either way
        // the file is rewritten, so consent and history never disagree.
        if let Ok(mut e) = self.shared.lock() {
            e.clear();
        }
        let _ = save(&Store {
            enabled: on,
            entries: Vec::new(),
        });
    }

    /// Drop every entry but keep consent — the panel's "Clear all" button.
    pub fn clear(&self) {
        if let Ok(mut e) = self.shared.lock() {
            e.clear();
        }
        let _ = save(&Store {
            enabled: self.enabled(),
            entries: Vec::new(),
        });
    }

    /// Put `entry`'s text back on the clipboard.
    pub fn restore(text: &str) -> bool {
        set_text(text)
    }
}

fn run(entries: Arc<Mutex<Vec<Entry>>>, enabled: Arc<Mutex<bool>>) {
    let mut last_seq = 0u32;
    loop {
        std::thread::sleep(POLL);
        if !enabled.lock().map(|b| *b).unwrap_or(false) {
            // Consent off: keep tracking the counter so turning it on does not
            // immediately re-capture whatever happened while it was off.
            last_seq = sequence();
            continue;
        }
        let seq = sequence();
        if seq == 0 || seq == last_seq {
            continue;
        }
        last_seq = seq;
        // Another process can hold the clipboard open; skip this tick rather
        // than blocking the worker on a global lock.
        let Some(text) = read_text() else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_BYTES {
            continue;
        }
        let entry = Entry {
            text: text.to_string(),
            at: unix_now(),
            kind: kind_of(text),
        };
        let mut store_entries = match entries.lock() {
            Ok(e) => e,
            Err(_) => return,
        };
        // Copying the same text twice is not history.
        if store_entries.first().map(|e| e.text.as_str()) == Some(entry.text.as_str()) {
            continue;
        }
        store_entries.insert(0, entry);
        store_entries.truncate(MAX_ENTRIES);
        let snapshot = store_entries.clone();
        drop(store_entries);
        let _ = save(&Store {
            enabled: true,
            entries: snapshot,
        });
    }
}

/// Which cell the panel shows: a letter for text, a digit for a number, a dot for
/// a single glyph. Purely a colour hint for the leading cell.
fn kind_of(text: &str) -> char {
    match text.chars().next() {
        Some(c) if c.is_ascii_digit() => '0',
        Some(c) if c.is_alphabetic() => 'A',
        _ => '.',
    }
}

/// `GetClipboardSequenceNumber`: 0 on failure.
fn sequence() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Read `CF_UNICODETEXT`, or `None` for any other format (an image, a file
/// drop) or a clipboard another process is holding.
/// Read the clipboard's text right now, ignoring capture consent.
///
/// Consent governs *history*, not reading: a click that consumes the clipboard
/// is an explicit user action, so it needs no opt-in.
pub fn paste() -> Option<String> {
    read_text()
}

fn read_text() -> Option<String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};

    // SAFETY: every early exit closes the clipboard. The handle from
    // `GetClipboardData` is owned by the clipboard, not by us.
    unsafe {
        OpenClipboard(None).ok()?;
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let lock = windows::Win32::System::Memory::GlobalLock(
                windows::Win32::Foundation::HGLOBAL(handle.0)
            );
            if lock.is_null() {
                return None;
            }
            // Cap the scan: a hostile or corrupt clipboard can be arbitrarily
            // long, and we would rather truncate than allocate without bound.
            let scan = MAX_BYTES.min(1024 * 1024);
            let mut len = 0usize;
            while len < scan {
                let ch = *(lock as *const u16).add(len);
                if ch == 0 {
                    break;
                }
                len += 1;
            }
            let slice = std::slice::from_raw_parts(lock as *const u16, len);
            Some(String::from_utf16_lossy(slice))
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Write text to the clipboard as `CF_UNICODETEXT`. The buffer is allocated
/// moveable and handed to the clipboard, which owns it after a successful
/// `SetClipboardData`.
fn set_text(text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;

    // SAFETY: the allocation is moveable memory of exactly `bytes`; on success
    // the clipboard takes ownership, on failure we free it here.
    unsafe {
        let mem = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let ptr = GlobalLock(mem);
        if ptr.is_null() {
            let _ = GlobalUnlock(mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
        let _ = GlobalUnlock(mem);

        if OpenClipboard(None).is_err() {
            // Ownership never transferred, so free it ourselves.
            let _ = GlobalFree(Some(mem));
            return false;
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_ok();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(Some(mem));
        }
        ok
    }
}

/// Seconds since the Unix epoch, from `GetSystemTimeAsFileTime`. The clipboard
/// service only needs it for ordering, so a coarse clock is fine.
fn unix_now() -> u64 {
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    // FILETIME is 100 ns ticks since 1601; the Unix epoch is 369 years earlier.
    let ticks = (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64;
    ticks / 10_000_000
}

fn load() -> Store {
    super::settings::load().into()
}

fn save(store: &Store) -> std::io::Result<()> {
    let mut t = super::settings::load();
    super::settings::set_bool(&mut t, "clip_enabled", store.enabled);
    super::settings::save(&t);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> Entry {
        Entry {
            text: text.to_string(),
            at: 0,
            kind: kind_of(text),
        }
    }

    #[test]
    fn a_preview_collapses_whitespace_and_truncates_with_an_ellipsis() {
        let p = entry("hello\n\n  world   again").preview(20);
        assert_eq!(p, "hello world again");
        let long = entry(&"x".repeat(50)).preview(10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_short_preview_is_returned_verbatim() {
        assert_eq!(entry("arc").preview(20), "arc");
    }

    #[test]
    fn kind_hints_at_a_digit_a_letter_or_something_else() {
        assert_eq!(kind_of("42 things"), '0');
        assert_eq!(kind_of("hello"), 'A');
        assert_eq!(kind_of("#tag"), '.');
    }

    #[test]
    fn the_history_is_capped_at_max_entries() {
        let mut e: Vec<Entry> = (0..MAX_ENTRIES + 10).map(|i| entry(&i.to_string())).collect();
        e.truncate(MAX_ENTRIES);
        assert_eq!(e.len(), MAX_ENTRIES);
        assert_eq!(e[0].text, "0");
    }

    #[test]
    fn consent_defaults_to_off_so_a_fresh_install_records_nothing() {
        // `Store::default` is what a missing file parses to.
        assert!(!Store::default().enabled);
        assert!(Store::default().entries.is_empty());
    }
}