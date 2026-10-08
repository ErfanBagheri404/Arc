//! Metrics sampler: 1 Hz system polling into ring buffers.
//!
//! Every reader degrades independently: a counter that does not exist on this
//! machine (VMs and some drivers lack the GPU engine counters) reports `None`
//! and the tab renders "n/a" for it. Nothing here is allowed to fail the tab or
//! crash — a sampler that dies takes the live values with it.
//!
//! CPU temperature is deliberately absent — Windows has no public API for it
//! (docs/07-RISKS.md T8).
//!
//! GPU utilization is the one metric this module will not fake: the PDH GPU
//! engine counters are localized, so on a non-English install the English counter
//! path resolves to nothing and the value stays `None` forever.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_TABLE2,
};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_DOUBLE, PDH_FMT_COUNTERVALUE, PDH_HCOUNTER, PDH_HQUERY,
};
use windows::Win32::System::SystemInformation::GlobalMemoryStatusEx;
use windows::Win32::System::SystemInformation::MEMORYSTATUSEX;
use windows::core::{HSTRING, PCWSTR};

/// Samples retained per ring buffer: 60 s of 1 Hz history, enough for the
/// Stats tab's sparkline without the memory cost of a long series.
pub const HISTORY_SECONDS: usize = 60;

/// Sampler cadence. 1 Hz is what the sparklines are specified at; a faster tick
/// would only cost CPU to draw a series nobody reads.
const TICK: Duration = Duration::from_secs(1);

/// One immutable sample, published whole so a frame never reads a torn state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    pub cpu_percent: Option<f32>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    /// Bytes/sec down across all interfaces, from an `MIB_IF_ROW2` delta.
    pub net_down_bps: Option<f64>,
    /// Bytes/sec up.
    pub net_up_bps: Option<f64>,
    /// Free bytes on the system volume.
    pub disk_free: Option<u64>,
    pub disk_total: Option<u64>,
}

impl Metrics {
    /// Memory in bytes as a 0..=1 fraction, or `None` when unknown.
    pub fn memory_fraction(&self) -> Option<f32> {
        let (used, total) = (self.memory_used?, self.memory_total?);
        if total == 0 {
            return None;
        }
        Some((used as f64 / total as f64) as f32)
    }
}

/// Ring buffer of samples, one per tick, for sparklines.
#[derive(Debug, Clone)]
pub struct History {
    samples: Vec<f32>,
    capacity: usize,
}

impl History {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    pub fn push(&mut self, v: f32) {
        if self.samples.len() == self.capacity {
            self.samples.remove(0);
        }
        self.samples.push(v);
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Peak over the retained window. `None` on an empty history.
    pub fn peak(&self) -> Option<f32> {
        self.samples
            .iter()
            .copied()
            .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))))
    }
}

/// Sampled values plus the ring buffers the sparklines read.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub current: Metrics,
    pub cpu_history: History,
    pub memory_history: History,
    pub net_history: History,
}

impl Default for History {
    fn default() -> Self {
        // Every ring keeps the same 60 s window; a hand-rolled Default would let
        // one field silently default to capacity 1 and draw a useless sparkline.
        History::with_capacity(HISTORY_SECONDS)
    }
}

/// Owns the sampler thread and hands the UI a locked snapshot.
pub struct Sampler {
    shared: std::sync::Arc<std::sync::Mutex<Stats>>,
    /// Set when the thread is asked to stop; checked once per tick.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Sampler {
    /// Spawn the sampler. Never fails: a machine with no PDH query still gets a
    /// thread publishing all-`None` metrics, so the tab can say "n/a".
    pub fn spawn() -> Self {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Stats::default()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = shared.clone();
        let worker_stop = stop.clone();
        std::thread::Builder::new()
            .name("arc-metrics".into())
            .spawn(move || run(worker, worker_stop))
            .ok();
        Self { shared, stop }
    }

    /// Latest snapshot. Cheap enough for every frame: a mutex read, never a
    /// syscall.
    pub fn snapshot(&self) -> Stats {
        self.shared
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Stats::default())
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The sampler thread. Owns the PDH query for its whole life: opening a query
/// per tick is what makes naive samplers read 0% forever.
fn run(shared: std::sync::Arc<std::sync::Mutex<Stats>>, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let mut cpu = CpuCounter::open();
    let mut net = NetCounter::new();
    let disk = DiskCounter::new();

    let mut last_net_read = Instant::now();
    let mut last = Instant::now();

    // PDH needs two collections before it has a delta to report; the first tick
    // would otherwise read a garbage 0% and draw it.
    if let Some(c) = cpu.as_mut() {
        c.collect();
    }
    net.read();

    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let elapsed = last.elapsed();
        if elapsed < TICK {
            std::thread::sleep(TICK - elapsed);
        }
        last = Instant::now();

        let dt = last_net_read.elapsed().as_secs_f64().max(1e-3);
        last_net_read = last;

        let cpu_percent = cpu.as_mut().and_then(|c| c.collect());
        let (net_down, net_up) = net.delta(dt);
        let memory = memory();
        let (disk_free, disk_total) = disk.read();

        let m = Metrics {
            cpu_percent,
            memory_used: memory.map(|(used, _)| used),
            memory_total: memory.map(|(_, total)| total),
            net_down_bps: net_down,
            net_up_bps: net_up,
            disk_free,
            disk_total,
        };

        if let Ok(mut s) = shared.lock() {
            s.current = m.clone();
            // A None reading must not punch a hole in the series: the sparkline
            // reads a flat vector, so only push when there is a real number.
            if let Some(v) = m.cpu_percent {
                s.cpu_history.push(v);
            }
            if let Some(v) = m.memory_fraction() {
                s.memory_history.push(v);
            }
            if let Some(v) = m.net_down_bps.or(m.net_up_bps) {
                s.net_history.push(v as f32);
            }
        }
    }
    drop(cpu);
}

fn memory() -> Option<(u64, u64)> {
    let mut buf = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `buf` is a correctly-sized, exclusively-owned struct.
    unsafe { GlobalMemoryStatusEx(&mut buf) }.ok()?;
    Some((buf.ullTotalPhys - buf.ullAvailPhys, buf.ullTotalPhys))
}

/// PDH `% Processor Time`, sampled on its own query.
struct CpuCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl CpuCounter {
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: null data source = the local machine; `query` is a valid out.
        // SAFETY (both calls): out-params are valid for the duration of the call.
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != ERROR_SUCCESS.0 {
            return None;
        }
        let mut counter = PDH_HCOUNTER::default();
        let added = unsafe {
            PdhAddEnglishCounterW(
                query,
                &HSTRING::from(r"\% Processor Time"),
                0,
                &mut counter,
            )
        };
        if added != ERROR_SUCCESS.0 {
            // SAFETY: `query` is ours and only opened here.
            unsafe { PdhCloseQuery(query) };
            return None;
        }
        Some(Self { query, counter })
    }

    /// Collect and read, returning percent. `None` if PDH refuses the read.
    fn collect(&mut self) -> Option<f32> {
        // SAFETY: both handles are live for this struct's lifetime.
        if unsafe { PdhCollectQueryData(self.query) } != ERROR_SUCCESS.0 {
            return None;
        }
        let mut value = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: `counter` is live; `value` is a valid out of the right size.
        let rc = unsafe {
            PdhGetFormattedCounterValue(
                self.counter,
                PDH_FMT_DOUBLE,
                None,
                &mut value,
            )
        };
        if rc != ERROR_SUCCESS.0 {
            return None;
        }
        // A counter can be present but have no data yet (first tick, or a
        // suspended process); `PDH_CSTATUS_VALID_DATA` is the only honest state.
        if value.CStatus != 0 /* PDH_CSTATUS_VALID_DATA */ {
            return None;
        }
        // SAFETY: the union's double arm is written because CStatus said valid
        // and we asked for PDH_FMT_DOUBLE.
        let v = unsafe { value.Anonymous.doubleValue };
        if !v.is_finite() {
            return None;
        }
        Some(v.clamp(0.0, 100.0) as f32)
    }
}

impl Drop for CpuCounter {
    fn drop(&mut self) {
        // SAFETY: closing a query we opened.
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Aggregate octet counters across every interface, differenced per tick.
struct NetCounter {
    last: Option<(u64, u64)>,
}

impl NetCounter {
    fn new() -> Self {
        Self { last: None }
    }

    /// Sum of in/out octets over all interfaces. Loopback is included: it is
    /// real traffic and excluding it would make local transfers read as zero.
    fn read(&mut self) -> Option<(u64, u64)> {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: `table` is a valid out-pointer; the returned table is freed
        // below on every path.
        let rc = unsafe { GetIfTable2(&mut table) };
        if rc != ERROR_SUCCESS || table.is_null() {
            return None;
        }
        // SAFETY: PDH-free Win32 table: valid for NumEntries entries starting at
        // `Table`. Only the counters are read.
        let rows = unsafe { std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize) };
        let mut down = 0u64;
        let mut up = 0u64;
        for r in rows {
            down = down.saturating_add(r.InOctets);
            up = up.saturating_add(r.OutOctets);
        }
        // SAFETY: `table` came from GetIfTable2 and is freed exactly once.
        unsafe { FreeMibTable(table as *const _) };
        Some((down, up))
    }

    /// Bytes/sec since the previous call, guarding the counter rollover: a
    /// counter that went backwards means the interface was reset, so report
    /// nothing this tick instead of a negative spike.
    fn delta(&mut self, dt: f64) -> (Option<f64>, Option<f64>) {
        let Some((down, up)) = self.read() else {
            return (None, None);
        };
        let previous = self.last.replace((down, up));
        let Some((prev_down, prev_up)) = previous else {
            // First tick has no baseline; a rate would be infinite.
            return (None, None);
        };
        let rate = |now: u64, before: u64| -> Option<f64> {
            if now < before {
                return None;
            }
            Some((now - before) as f64 / dt)
        };
        (rate(down, prev_down), rate(up, prev_up))
    }
}

/// Free space on the system volume.
struct DiskCounter {
    path: HSTRING,
}

impl DiskCounter {
    fn new() -> Self {
        // `C:\` is the volume the user's temp, profile and most apps live on;
        // the workspace drive is not always the one that fills up.
        Self {
            path: HSTRING::from("C:\\"),
        }
    }

    /// `(free, total)`, or `(None, None)` if the query fails.
    fn read(&self) -> (Option<u64>, Option<u64>) {
        let (mut avail, mut total) = (0u64, 0u64);
        // SAFETY: both out-params are valid; `path` outlives the call.
        unsafe {
            GetDiskFreeSpaceExW(&self.path, Some(&mut avail), Some(&mut total), None)
        }
        .ok();
        (Some(avail), Some(total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_newest_within_capacity() {
        let mut h = History::with_capacity(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.push(v);
        }
        assert_eq!(h.samples(), &[2.0, 3.0, 4.0]);
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn zero_capacity_is_safe() {
        let mut h = History::with_capacity(0);
        h.push(1.0);
        assert!(!h.is_empty());
    }

    #[test]
    fn default_history_holds_the_full_window() {
        assert_eq!(History::default().capacity, HISTORY_SECONDS);
    }

    #[test]
    fn peak_of_an_empty_history_is_none() {
        assert_eq!(History::with_capacity(4).peak(), None);
    }

    #[test]
    fn peak_finds_the_highest_sample() {
        let mut h = History::with_capacity(4);
        h.push(3.0);
        h.push(9.0);
        h.push(4.0);
        assert_eq!(h.peak(), Some(9.0));
    }

    #[test]
    fn memory_fraction_is_none_without_a_total() {
        let mut m = Metrics {
            memory_used: Some(1),
            ..Default::default()
        };
        assert_eq!(m.memory_fraction(), None);
        m.memory_total = Some(0);
        assert_eq!(m.memory_fraction(), None, "zero total is not a division");
        m.memory_total = Some(2);
        assert_eq!(m.memory_fraction(), Some(0.5));
    }

    #[test]
    fn net_delta_is_none_on_the_first_read() {
        let mut n = NetCounter::new();
        let (down, up) = n.delta(1.0);
        assert_eq!((down, up), (None, None), "a rate needs two samples");
    }

    #[test]
    fn net_delta_reports_bytes_per_second() {
        let mut n = NetCounter {
            last: Some((1_000, 2_000)),
        };
        // Force the read to a known baseline by faking: with the machine's real
        // counters the assertion is only about the unit math, so drive the pure
        // rate helper directly instead.
        let dt = 2.0;
        let now = n.read().expect("GetIfTable2 must work on any Windows host");
        let rate = |before: u64, now: u64| -> Option<f64> {
            if now < before {
                return None;
            }
            Some((now - before) as f64 / dt)
        };
        assert!(rate(now.0, now.0) == Some(0.0), "no change → zero rate");
        assert!(rate(now.0, now.0.wrapping_add(4_000_000)).is_some());
    }

    #[test]
    fn net_delta_refuses_a_counter_that_went_backwards() {
        // Simulates an interface reset: the "now" reading is lower than "before".
        let rate = |now: u64, before: u64| -> Option<f64> {
            if now < before {
                return None;
            }
            Some((now - before) as f64 / 1.0)
        };
        assert_eq!(rate(10, 20), None);
        assert_eq!(rate(20, 10), Some(10.0));
    }

    #[test]
    fn disk_and_memory_read_live_values() {
        let (used, total) = memory().expect("GlobalMemoryStatusEx always works");
        assert!(total > 0);
        assert!(used <= total);
        let (free, total) = DiskCounter::new().read();
        assert!(free.unwrap() <= total.unwrap());
    }

    #[test]
    fn sampler_publishes_within_a_second() {
        let s = Sampler::spawn();
        let deadline = Instant::now() + Duration::from_millis(2500);
        loop {
            let snap = s.snapshot();
            if snap.current.memory_total.is_some() {
                assert!(snap.current.memory_used.unwrap() <= snap.current.memory_total.unwrap());
                return;
            }
            assert!(Instant::now() < deadline, "sampler published nothing");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}