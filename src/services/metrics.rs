//! Metrics sampler: 1 Hz system polling into ring buffers.
//!
//! Phase 1 lands a probe-only stub so the frame loop can be exercised end to end
//! without a sampler; Phase 4 replaces the body with real PDH/`GlobalMemoryStatusEx`
//! readers. CPU temperature is deliberately absent — Windows has no public API for it
//! (see docs/07-RISKS.md T8).

// Phase 4 consumes this surface (the media/metrics engine). It is written ahead
// of its consumers, like `core/`, so the unused surface is allowed per-module.
#[allow(dead_code)]
/// Snapshot the UI consumes. Fields are `None` when the counter is unavailable on
/// this machine (VMs and some drivers lack them) — never fail the tab over it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    pub cpu_percent: Option<f32>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
}

/// Ring buffer of samples, one per tick, for sparklines.
#[derive(Debug, Clone)]
pub struct History {
    samples: Vec<f32>,
    capacity: usize,
}

#[allow(dead_code)]
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
}

/// Current capability probe. Phase 1: everything unavailable.
#[allow(dead_code)]
pub fn probe() -> Metrics {
    Metrics::default()
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
    fn probe_returns_all_none_by_default() {
        let m = probe();
        assert_eq!(m.cpu_percent, None);
        assert_eq!(m.memory_used, None);
    }
}
