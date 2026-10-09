//! Downloads watcher: a size-delta heuristic over FOLDERID_Downloads.
//!
//! Phase 6 calls this out as **beta-quality** and the UI must label it as
//! such: there is no API that says "a download started", so Arc sums file
//! sizes on a poll and reports the delta. A file being written shows up as a
//! rising total; a finished download shows as one positive delta; a delete
//! shows negative. Nothing here can distinguish a browser's `.crdownload`
//! temp file from an extractor writing output — hence beta.
//!
//! No new crates: `read_dir` over one folder. No recursive walk either —
//! downloads land flat, and a tree walk costs proportional to subfolder depth
//! nobody asked to watch.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// How often the folder is summed, in seconds.
pub const REFRESH_SECS: u64 = 5;

/// One poll's observation: total bytes and the change since the last poll.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// Sum of file sizes in Downloads, bytes.
    pub total: u64,
    /// `total` minus the previous poll's `total`. Positive = grew.
    pub delta: i64,
    /// Polls taken so far. 0 means "not yet sampled".
    pub polls: u32,
}

impl Snapshot {
    /// The pill text: `Downloads +12.3 MB (beta)`. The word beta is part of
    /// the label, not a tooltip — the heuristic cannot tell a finished
    /// download from a temp file still being written.
    pub fn label(&self) -> String {
        format!("Downloads +{} (beta)", human(self.delta.max(0) as u64))
    }
}

/// Bytes as a short human string: `12.3 MB`, `900 kB`, `512 B`.
fn human(b: u64) -> String {
    const KB: f64 = 1024.0;
    let b = b as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} kB", b / KB)
    } else {
        format!("{b:.0} B")
    }
}

/// The Downloads folder path, or `None` when Windows will not name it.
fn downloads_dir() -> Option<PathBuf> {
    // SAFETY: SHGetKnownFolderPath writes a properly terminated buffer we
    // copy out of before freeing.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) }.ok()?;
    let s = unsafe { p.to_string() }.ok()?;
    PathBuf::from(s).into()
}

/// Sum the sizes of the files directly in `dir`. Subdirectories are counted
/// as their own entry size only (0 for a dir in Windows), not recursed.
fn total_bytes(dir: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Sum the Downloads folder now. Missing folder reads as 0, not as an error:
/// a user with no Downloads folder has nothing to watch.
pub fn sample() -> u64 {
    downloads_dir().map(|d| total_bytes(&d)).unwrap_or(0)
}

/// Watcher: samples on a worker thread, exposes the latest delta.
pub struct Downloads {
    shared: Arc<Mutex<Snapshot>>,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl Downloads {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (sh, al) = (shared.clone(), alive.clone());
        std::thread::Builder::new()
            .name("downloads".into())
            .spawn(move || {
                let mut last = 0u64;
                while al.load(std::sync::atomic::Ordering::Relaxed) {
                    let total = sample();
                    let delta = total as i64 - last as i64;
                    if let Ok(mut g) = sh.lock() {
                        g.total = total;
                        g.delta = delta;
                        g.polls += 1;
                    }
                    last = total;
                    std::thread::sleep(Duration::from_secs(REFRESH_SECS));
                }
            })
            .expect("spawn downloads thread");
        Self { shared, alive }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Drop for Downloads {
    fn drop(&mut self) {
        self.alive
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_sizes_the_files_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("arc-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("b.bin"), vec![0u8; 50]).unwrap();
        assert_eq!(total_bytes(&dir), 150);
        // A subdirectory contributes nothing — no recursion.
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("c.bin"), vec![0u8; 700]).unwrap();
        assert_eq!(total_bytes(&dir), 150);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn label_states_the_delta_and_the_beta_caveat() {
        let s = Snapshot {
            total: 0,
            delta: 1024 * 1024 * 3 / 2,
            polls: 2,
        };
        assert_eq!(s.label(), "Downloads +1.5 MB (beta)");
    }

    #[test]
    fn missing_folder_reads_as_zero() {
        assert_eq!(total_bytes(&std::path::Path::new("Q:/no/such/folder")), 0);
    }

    #[test]
    fn watcher_starts_with_an_empty_snapshot() {
        let w = Downloads::new();
        let s = w.snapshot();
        assert_eq!(s, Snapshot::default(), "no polls yet");
    }
}
