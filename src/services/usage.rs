//! LLM usage: what Claude Code and Codex have burned in a rolling window.
//!
//! Both runners keep an append-only JSONL transcript per session; we read
//! theirs, not the shell's. There is no quota API to call — the only
//! honest source of "how much have I used" is the transcript itself, so
//! this module sums the `usage` blocks a session reports per turn and
//! drops everything older than [`WINDOW_HOURS`].
//!
//! Claude Code: `~/.claude/projects/**\/*.jsonl`, one `usage` object per
//! assistant message with `input_tokens`, `output_tokens`,
//! `cache_creation_input_tokens`, `cache_read_input_tokens`.
//! Codex: `~/.codex/sessions/**\/*.jsonl`, same shape under `payload`.
//!
//! A runner that is not installed is not an error — it renders as a
//! disabled row, so a machine with only one of the two still gets a
//! useful tab.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// How often the worker re-scans the transcripts.
const REFRESH_SECS: u64 = 60;

/// A live handle: a worker thread rescans every [`REFRESH_SECS`] and the
/// UI reads the last snapshot off the mutex. File reads never land on
/// the app thread — a busy Claude day is hundreds of transcripts.
pub struct Usage {
    shared: Arc<Mutex<Snapshot>>,
    alive: Arc<AtomicBool>,
}

impl Usage {
    /// Start the worker.
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let alive = Arc::new(AtomicBool::new(true));
        let (sh, al) = (shared.clone(), alive.clone());
        std::thread::Builder::new()
            .name("usage".into())
            .spawn(move || {
                while al.load(Ordering::Relaxed) {
                    let snap = snapshot(now_secs());
                    if let Ok(mut g) = sh.lock() {
                        *g = snap;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(REFRESH_SECS));
                }
            })
            .ok();
        Self { shared, alive }
    }

    /// The latest snapshot the worker published.
    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Drop for Usage {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl Default for Usage {
    fn default() -> Self {
        Self::new()
    }
}

/// How far back usage is counted. Anthropic's rolling window is 5 h;
/// keep the same span for every runner.
pub const WINDOW_HOURS: u64 = 5;

/// Files newer than this are worth reading. Older transcripts cannot
/// contribute to the window, and they are the bulk of the bytes.
const MAX_AGE_SECS: u64 = WINDOW_HOURS * 3600;

/// Cap on transcripts read per refresh, newest first. A heavy day can
/// leave hundreds of files; the window only needs the recent tail.
const MAX_FILES: usize = 200;

/// Tokens this runner is assumed to be allowed per window, when the
/// transcript itself does not say. Only used for the bar's denominator.
pub const DEFAULT_LIMIT: u64 = 1_000_000;

/// Fraction of the limit at which a bar turns amber, then red.
pub const WARN: f64 = 0.80;
pub const DANGER: f64 = 0.95;

/// One runner's usage inside the window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Runner {
    /// Display name shown on the row.
    pub name: &'static str,
    /// Tokens actually consumed in the window.
    pub tokens: u64,
    /// The runner's own window limit, if its transcript reported one.
    pub limit: Option<u64>,
    /// False when the runner's home directory is missing — draws as
    /// "not installed" instead of a zero-token bar.
    pub installed: bool,
    /// Transcripts actually read this refresh (diagnostics in tests).
    pub files: usize,
}

impl Runner {
    /// Tokens as a fraction of the limit, clamped to `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT).max(1) as f32;
        (self.tokens as f32 / limit).clamp(0.0, 1.0)
    }

    /// Whether the bar is amber, red, or plain.
    ///
    /// Compared in integer space on purpose: `95u64 as f32 / 100.0` is
    /// 0.9499999, so a float `>= 0.95` check would miss the exact
    /// boundary the thresholds are named for.
    pub fn severity(&self) -> Severity {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT).max(1);
        if self.tokens * 100 >= limit * (DANGER * 100.0) as u64 {
            Severity::Danger
        } else if self.tokens * 100 >= limit * (WARN * 100.0) as u64 {
            Severity::Warn
        } else {
            Severity::Ok
        }
    }
}

/// Bar colour band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Danger,
}

/// Everything the Usage tab draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub claude: Runner,
    pub codex: Runner,
}

impl Snapshot {
    /// Runners that are installed, in display order.
    pub fn rows(&self) -> Vec<&Runner> {
        [
            &self.claude,
            &self.codex,
        ]
        .into_iter()
        .filter(|r| r.installed)
        .collect()
    }
}

/// The `usage` block a runner reports on an assistant turn. Both runners
/// use these names, so one shape covers both; unknown fields are ignored
/// because both tools add new ones freely.
#[derive(Debug, Deserialize, Default, Clone, Copy)]
struct UsageBlock {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

impl UsageBlock {
    /// Billed tokens for one turn. Cached reads count: they are quota.
    fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

/// A transcript line: the usage block may sit at the top level (Claude
/// Code) or nested under `payload` (Codex).
#[derive(Debug, Deserialize)]
struct Line {
    #[serde(default)]
    payload: Option<Payload>,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize, Default)]
struct Payload {
    #[serde(default)]
    usage: Option<UsageBlock>,
}

/// Sum usage blocks from one transcript, ignoring lines that do not
/// carry any. Returns `None` when the file could not be read.
fn scan(path: &Path) -> Option<(u64, Option<u64>)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut tokens = 0u64;
    for l in raw.lines() {
        // A transcript line is a single JSON object; skipping lines that
        // fail to parse keeps one truncated tail line from losing the
        // rest of the file.
        let Ok(line) = serde_json::from_str::<Line>(l) else {
            continue;
        };
        let u = line.usage.or_else(|| line.payload.and_then(|p| p.usage));
        if let Some(u) = u {
            tokens = tokens.saturating_add(u.total());
        }
    }
    Some((tokens, None))
}

/// Newest-first list of transcript files under `root`, capped at
/// [`MAX_FILES`]. Recursion is hand-rolled: both runners nest sessions
/// under a date tree, and this repo has no directory-walk dependency.
fn transcripts(root: &Path, out: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            transcripts(&path, out);
        } else if path.extension().is_some_and(|x| x == "jsonl") {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                out.push((t, path));
            }
        }
    }
}

/// Seconds since the UNIX epoch, or 0 if the clock is before it (which
/// only happens in a broken test fixture).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Total a runner's usage over the window, reading from `home`.
///
/// `now` is injected so tests are deterministic; production passes
/// [`now_secs`].
pub fn scan_runner(
    name: &'static str,
    home: &Path,
    now: u64,
) -> Runner {
    let installed = home.is_dir();
    if !installed {
        return Runner {
            name,
            installed: false,
            ..Runner::default()
        };
    }
    let mut files = Vec::new();
    transcripts(home, &mut files);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let cutoff = now.saturating_sub(MAX_AGE_SECS);
    let mut tokens = 0u64;
    let mut limit = None;
    let mut read = 0usize;
    for (mtime, path) in files.into_iter().take(MAX_FILES) {
        if mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) < cutoff {
            continue;
        }
        let Some((t, l)) = scan(&path) else { continue };
        tokens = tokens.saturating_add(t);
        limit = limit.or(l);
        read += 1;
    }
    Runner {
        name,
        tokens,
        limit,
        installed: true,
        files: read,
    }
}

/// Refresh both runners' usage.
pub fn snapshot(now: u64) -> Snapshot {
    Snapshot {
        claude: scan_runner("Claude Code", &claude_dir(), now),
        codex: scan_runner("Codex", &codex_dir(), now),
    }
}

/// `~/.claude/projects`, where Claude Code writes transcripts.
fn claude_dir() -> PathBuf {
    home_dir().join(".claude").join("projects")
}

/// `~/.codex/sessions`, where Codex writes rollouts.
fn codex_dir() -> PathBuf {
    home_dir().join(".codex").join("sessions")
}

/// `%USERPROFILE%`, or an empty path when unset (tests pass paths in).
fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arc-usage-{name}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn sums_a_claude_style_transcript() {
        let d = tmp("claude");
        write(
            &d,
            "a.jsonl",
            concat!(
                r#"{"type":"assistant","usage":{"input_tokens":10,"output_tokens":5}}"#,
                "\n",
                r#"{"type":"assistant","usage":{"input_tokens":1,"cache_read_input_tokens":100}}"#,
                "\n",
                r#"{"type":"user","message":"hi"}"#,
            ),
        );
        let r = scan_runner("Claude Code", &d, now_secs());
        assert!(r.installed);
        assert_eq!(r.tokens, 116, "cached reads are quota");
    }

    #[test]
    fn reads_nested_codex_payloads() {
        let d = tmp("codex");
        let nested = d.join("2026").join("02").join("26");
        fs::create_dir_all(&nested).unwrap();
        write(
            &nested,
            "rollout.jsonl",
            concat!(
                r#"{"timestamp":"2026-02-26T05:49:24.894Z","type":"event_msg","payload":{"type":"task_started"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"usage":{"input_tokens":7,"output_tokens":3}}}"#,
            ),
        );
        let r = scan_runner("Codex", &d, now_secs());
        assert_eq!(r.tokens, 10, "nested jsonl must be found");
    }

    #[test]
    fn missing_runner_is_not_installed() {
        let r = scan_runner("Codex", Path::new("Z:/definitely-not-here"), now_secs());
        assert!(!r.installed);
        assert_eq!(r.tokens, 0);
    }

    #[test]
    fn skips_transcripts_older_than_the_window() {
        let d = tmp("old");
        write(&d, "old.jsonl", r#"{"usage":{"input_tokens":999}}"#);
        let r = scan_runner("Claude Code", &d, now_secs() + MAX_AGE_SECS * 2);
        assert_eq!(r.tokens, 0, "expired transcripts must not count");
    }

    #[test]
    fn bar_bands_follow_the_fraction() {
        let mk = |t: u64, l: u64| Runner {
            name: "x",
            tokens: t,
            limit: Some(l),
            installed: true,
            files: 1,
        };
        assert_eq!(mk(79, 100).severity(), Severity::Ok);
        assert_eq!(mk(80, 100).severity(), Severity::Warn);
        assert_eq!(mk(95, 100).severity(), Severity::Danger);
        assert!(mk(200, 100).fraction() <= 1.0, "bar never overflows");
        assert!(mk(10, 0).fraction() <= 1.0, "zero limit must not divide by zero");
    }

    #[test]
    fn snapshot_rows_hide_absent_runners() {
        let s = Snapshot {
            claude: Runner { name: "Claude Code", tokens: 5, installed: true, ..Runner::default() },
            codex: Runner::default(),
        };
        let rows = s.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Claude Code");
    }

    #[test]
    fn a_truncated_tail_line_does_not_lose_the_file() {
        let d = tmp("tail");
        let body = format!(
            "{}\n{}",
            r#"{"usage":{"input_tokens":4}}"#,
            r#"{"usage":{"input_tok"# // killed mid-write
        );
        write(&d, "s.jsonl", &body);
        let r = scan_runner("Claude Code", &d, now_secs());
        assert_eq!(r.tokens, 4);
    }
}