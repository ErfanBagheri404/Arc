//! HUD state: transient pill content that replaces the idle pill for a few
//! seconds, then dismisses (Atoll's live-activity behaviour).
//!
//! The service polls cheap Win32 getters on their own thread and publishes a
//! plain snapshot; the app decides when a change is worth showing. Arming and
//! expiry live here, in one clock, so a HUD can never get stuck on screen.

use std::time::{Duration, Instant};

/// How long a HUD stays up after it is armed (Atoll: 3 s for battery).
pub const HUD_DURATION: Duration = Duration::from_secs(3);

/// What the pill is currently showing instead of the idle dot.
#[derive(Debug, Clone, PartialEq)]
pub enum Hud {
    Battery {
        percent: u8,
        /// Plugged in — draws the bolt and the charging fill.
        charging: bool,
        /// At or below the low threshold: the fill turns amber.
        low: bool,
    },
    Volume {
        percent: u8,
        muted: bool,
    },
}

/// Battery/AC state, straight from `GetSystemPowerStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Power {
    /// `None` on a machine with no battery (desktop), which must not show 0 %.
    pub percent: Option<u8>,
    pub charging: bool,
}

/// The HUD layer: at most one HUD at a time, with an expiry.
#[derive(Debug, Default)]
pub struct HudLayer {
    active: Option<(Hud, Instant)>,
}

impl HudLayer {
    /// Show `hud`, or refresh its timer if the same HUD is already up (holding
    /// the volume key must not stack timers or flicker the panel).
    pub fn arm(&mut self, hud: Hud, now: Instant) {
        if self.active.as_ref().is_none_or(|(cur, _)| *cur != hud) {
            self.active = Some((hud, now + HUD_DURATION));
        } else if let Some((_, until)) = self.active.as_mut() {
            *until = now + HUD_DURATION;
        }
    }

    /// Drop the HUD once its timer expires. Returns `true` when it went away,
    /// so the caller knows a repaint is due.
    pub fn step(&mut self, now: Instant) -> bool {
        if self.active.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.active = None;
            return true;
        }
        false
    }

    pub fn current(&self) -> Option<Hud> {
        self.active.as_ref().map(|(h, _)| h.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bat(p: u8) -> Hud {
        Hud::Battery {
            percent: p,
            charging: false,
            low: false,
        }
    }

    #[test]
    fn a_hud_dismisses_after_its_window() {
        let mut l = HudLayer::default();
        let t0 = Instant::now();
        l.arm(bat(42), t0);
        assert!(l.current().is_some());
        assert!(!l.step(t0 + Duration::from_millis(2999)));
        assert!(l.step(t0 + HUD_DURATION), "expiry must report the change");
        assert!(l.current().is_none());
    }

    #[test]
    fn re_arming_the_same_hud_extends_its_timer() {
        let mut l = HudLayer::default();
        let t0 = Instant::now();
        l.arm(bat(42), t0);
        // Held volume key: re-arm just before expiry...
        l.arm(bat(42), t0 + Duration::from_millis(2900));
        // ...must not expire at the original deadline.
        assert!(!l.step(t0 + Duration::from_millis(3100)));
        assert!(l.current().is_some());
    }

    #[test]
    fn a_different_hud_replaces_the_current_one_immediately() {
        let mut l = HudLayer::default();
        let t0 = Instant::now();
        l.arm(bat(42), t0);
        l.arm(Hud::Volume { percent: 42, muted: true }, t0 + Duration::from_millis(100));
        assert_eq!(l.current(), Some(Hud::Volume { percent: 42, muted: true }));
    }
}
