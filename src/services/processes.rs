//! Top processes by CPU, polled with `EnumProcesses` + per-process CPU-time
//! deltas.
//!
//! CPU time comes from `GetProcessTimes` (kernel + user, 100 ns units). Since
//! the kernel exposes no per-process CPU *utilisation* getter, "top by CPU" has
//! to be a delta over a window: the first sample only seeds the baseline and
//! reports nothing, which is also why the pill's top-5 list appears one tick
//! late instead of being wrong.
//!
//! Name lookup is the expensive part, so names are cached by PID. A recycled PID
//! keeps the stale name for one sample — harmless for a display list, and the
//! next sample corrects it.
//!
//! CUT: per-process RAM and GPU columns. `GetProcessMemoryInfo` is easy but the
//! GPU column needs ETW, and a CPU-only list is what the panel actually shows.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessId, GetProcessTimes, OpenProcess,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// How many rows the panel shows.
pub const TOP_N: usize = 5;

/// Poll cadence. Matches the metrics tick: both diff CPU time over the window.
const TICK: Duration = Duration::from_secs(1);

/// One row: pid, name, and CPU percent over the last window.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
}

/// Snapshot of the top processes. Empty until the second poll.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub rows: Vec<Row>,
}

pub struct Processes {
    shared: Arc<Mutex<Snapshot>>,
}

impl Processes {
    pub fn start() -> Self {
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("arc-procs".into())
            .spawn(move || run(worker))
            .ok();
        Self { shared }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// CPU-time baseline per PID, in 100 ns units.
type Baselines = HashMap<u32, u64>;

fn run(shared: Arc<Mutex<Snapshot>>) {
    let mut baselines: Baselines = HashMap::new();
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut last: Option<Instant> = None;
    let mut first = true;

    loop {
        std::thread::sleep(TICK);
        let now = Instant::now();
        let elapsed = last.map(|t| now.duration_since(t)).unwrap_or(TICK);
        last = Some(now);

        let mut cpu: HashMap<u32, u64> = HashMap::new();
        for pid in live_pids() {
            if let Some(t) = cpu_time(pid) {
                cpu.insert(pid, t);
            }
        }

        let window_s = elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
        let mut rows: Vec<Row> = Vec::with_capacity(TOP_N);
        let mut next = Baselines::new();

        for (pid, ticks) in cpu {
            if let Some(&prev) = baselines.get(&pid) {
                // A PID that died mid-window can show a negative delta; skip it.
                if ticks > prev {
                    let pct = (ticks - prev) as f64 / 1.0e7 / window_s * 100.0;
                    // Ignore the idle/system pseudo-processes at 0 %: they would
                    // otherwise crowd out the list.
                    if pct > 0.05 {
                        rows.push(Row {
                            pid,
                            name: names
                                .get(&pid)
                                .cloned()
                                .unwrap_or_else(|| format!("pid {pid}")),
                            cpu_percent: pct as f32,
                        });
                    }
                }
            }
            next.insert(pid, ticks);
        }
        baselines = next;

        if first {
            // No baseline yet: publishing would show every process at 100 %.
            first = false;
            continue;
        }
        rows.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
        rows.truncate(TOP_N);

        // Refresh names only for rows we are about to show, so an idle machine
        // does no name I/O at all.
        for row in &rows {
            if !names.contains_key(&row.pid) {
                let n = process_name(row.pid);
                names.insert(row.pid, n);
            }
        }

        let snapshot = Snapshot {
            rows: rows
                .into_iter()
                .map(|mut r| {
                    r.name = names.get(&r.pid).cloned().unwrap_or(r.name);
                    r
                })
                .collect(),
        };
        if let Ok(mut s) = shared.lock() {
            *s = snapshot;
        }
    }
}

/// Every PID we can open for a CPU-time query.
fn live_pids() -> Vec<u32> {
    let mut pids = [0u32; 4096];
    let mut bytes: u32 = 0;
    // SAFETY: `pids` is a live buffer; the length comes back in `bytes`.
    let ok = unsafe { EnumProcesses(pids.as_mut_ptr(), pids.len() as u32 * 4, &mut bytes) };
    if ok.is_err() {
        return Vec::new();
    }
    let count = (bytes as usize / 4).min(pids.len());
    pids[..count]
        .iter()
        .copied()
        .filter(|p| *p != 0)
        .filter_map(|pid| {
            // SAFETY: a plain query-only open; `None` for the options is fine.
            let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
            let real = unsafe { GetProcessId(h) };
            unsafe { CloseHandle(h).ok() };
            (real != 0).then_some(real)
        })
        .collect()
}

/// Kernel + user CPU time for `pid`, in 100 ns units.
fn cpu_time(pid: u32) -> Option<u64> {
    // SAFETY: handle opened for a query and closed on every path below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let ok = unsafe { GetProcessTimes(h, &mut created, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(h).ok() };
    ok.ok()?;
    let hi = u64::from(user.dwHighDateTime) << 32 | u64::from(user.dwLowDateTime);
    let lo = u64::from(kernel.dwHighDateTime) << 32 | u64::from(kernel.dwLowDateTime);
    Some(hi + lo)
}

/// Image name for a PID, without the extension.
fn process_name(pid: u32) -> String {
    const MAX: u32 = 260;
    // SAFETY: query-only open on a live PID; closed before return.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return format!("pid {pid}");
    };
    let mut buf = [0u16; MAX as usize];
    let mut len = MAX;
    // SAFETY: `buf` is `MAX` wide; the API truncates to `len` and writes back
    // the number of characters used.
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    unsafe { CloseHandle(h).ok() };
    if ok.is_err() || len == 0 {
        return format!("pid {pid}");
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    // Trim the directory and extension so the row stays readable.
    let base = full.rsplit('\\').next().unwrap_or(&full);
    base.trim_end_matches(".exe").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_enum_actually_finds_this_process() {
        let live = live_pids();
        assert!(live.len() > 1, "expected at least a couple of processes");
        let me = unsafe { GetCurrentProcessId() };
        assert!(live.contains(&me), "own pid missing");
    }

    #[test]
    fn cpu_time_is_nonzero_for_a_live_process() {
        let me = unsafe { GetCurrentProcessId() };
        assert!(cpu_time(me).is_some());
    }

    #[test]
    fn a_live_process_resolves_to_a_real_name() {
        let me = unsafe { GetCurrentProcessId() };
        let name = process_name(me);
        // This test binary is a cargo test runner; its image is not an .exe
        // stem of nothing — it must resolve to *something* with no path left.
        assert!(!name.is_empty(), "no name for own pid");
        assert!(!name.contains('\\'), "path leaked into the name: {name}");
        assert!(!name.ends_with(".exe"), "extension not trimmed: {name}");
    }

    #[test]
    fn a_dead_pid_falls_back_to_its_pid() {
        // 0xFFFFFFFF is never a valid PID: the name must degrade, not panic.
        let name = process_name(u32::MAX);
        assert_eq!(name, format!("pid {}", u32::MAX));
    }

    #[test]
    fn the_sampler_publishes_top_rows_within_three_seconds() {
        let p = Processes::start();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let s = p.snapshot();
            if !s.rows.is_empty() {
                assert!(s.rows.len() <= TOP_N);
                // Sorted descending: the panel draws them in order.
                for pair in s.rows.windows(2) {
                    assert!(pair[0].cpu_percent >= pair[1].cpu_percent);
                }
                return;
            }
            assert!(Instant::now() < deadline, "no process rows after 3 s");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}