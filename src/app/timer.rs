//! Countdown timer: presets, run state, and the countdown text.
//!
//! Pure — the app thread owns the wall clock and the notification, this only
//! answers "what should the pill show" and "which preset is highlighted".

/// Preset lengths offered in the Timer tab, in seconds.
pub const PRESETS: [u32; 5] = [60, 180, 300, 600, 900];

/// A preset button's label. Every preset is under an hour, so minutes only.
pub fn preset_label(secs: u32) -> String {
    format!("{} min", secs / 60)
}

/// The running (or idle) countdown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timer {
    /// Seconds left when running, else 0.
    pub remaining: u32,
    /// The preset this run started from; equals `PRESETS[0]` by default.
    pub preset: usize,
}

impl Timer {
    /// Seconds left, formatted `MM:SS` (or `H:MM:SS` past an hour).
    pub fn label(&self) -> String {
        let (h, m, s) = (self.remaining / 3600, (self.remaining % 3600) / 60, self.remaining % 60);
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m:02}:{s:02}")
        }
    }

    /// Fraction of the run already elapsed, `0.0..=1.0`. The ring draws this.
    pub fn progress(&self) -> f32 {
        let total = PRESETS[self.preset.min(PRESETS.len() - 1)].max(1);
        1.0 - (self.remaining as f32 / total as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_timer_shows_no_countdown() {
        assert_eq!(Timer::default().label(), "00:00");
    }

    #[test]
    fn a_five_minute_run_counts_down_under_a_minute_first() {
        let t = Timer {
            remaining: 5 * 60 + 7,
            preset: 2,
        };
        assert_eq!(t.label(), "05:07");
    }

    #[test]
    fn a_run_past_an_hour_gains_the_hour_field() {
        let t = Timer {
            remaining: 3600 + 65,
            preset: 4,
        };
        assert_eq!(t.label(), "1:01:05");
    }

    #[test]
    fn the_ring_fills_as_the_run_elapses() {
        let t0 = Timer {
            remaining: 300,
            preset: 2,
        };
        let t1 = Timer {
            remaining: 150,
            preset: 2,
        };
        assert!(t0.progress() < t1.progress());
        assert!(t0.progress() < 0.01 && t1.progress() > 0.49);
    }

    #[test]
    fn a_zero_timer_reads_as_finished() {
        assert!((Timer {
            remaining: 0,
            preset: 0
        }
        .progress())
            >= 1.0);
    }

    #[test]
    fn every_preset_labels_in_minutes() {
        assert_eq!(preset_label(60), "1 min");
        assert_eq!(preset_label(900), "15 min");
    }
}