//! Island window state machine.
//!
//! The island has four modes; size and interactivity are outputs of the mode, not
//! separate settings, so the visual and the hit-testing can never disagree.

use crate::core::anim::{Spring, SpringConfig};

/// Reference geometry, verbatim from the reference design's external-display mode.
pub const PILL_W: f32 = 185.0;
pub const PILL_H: f32 = 32.0;
pub const PILL_RADIUS: f32 = 16.0;
pub const PANEL_W: f32 = 640.0;
pub const PANEL_H: f32 = 200.0;
pub const PANEL_RADIUS: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// Not shown at all (fullscreen video, or before first paint).
    Hidden,
    /// Collapsed pill.
    Collapsed,
    /// Expanded panel.
    Expanded,
}

/// Island geometry + morph springs.
pub struct IslandState {
    mode: WindowMode,
    /// What to restore to when a fullscreen app goes away.
    mode_before_hide: WindowMode,
    width: Spring,
    height: Spring,
    radius: Spring,
}

impl IslandState {
    pub fn collapsed() -> Self {
        Self {
            mode: WindowMode::Collapsed,
            mode_before_hide: WindowMode::Collapsed,
            width: Spring::new(PILL_W, SpringConfig::OPEN),
            height: Spring::new(PILL_H, SpringConfig::OPEN),
            radius: Spring::new(PILL_RADIUS, SpringConfig::GENERIC),
        }
    }

    /// Queried by the UI parity tests and later by the debug overlay (Phase 2);
    /// the run loop drives `hidden()`/`expanded()` instead.
    #[allow(dead_code)]
    pub fn mode(&self) -> WindowMode {
        self.mode
    }

    pub fn expanded(&self) -> bool {
        self.mode == WindowMode::Expanded
    }

    /// Flip between pill and panel. From `Hidden`, a toggle re-opens the pill
    /// (the panel was already dismissed), so the user never lands on a state
    /// they cannot see.
    pub fn toggle(&mut self) {
        self.mode = match self.mode {
            WindowMode::Expanded => WindowMode::Collapsed,
            _ => WindowMode::Expanded,
        };
        self.retarget();
    }

    /// Point the springs at the current mode's geometry. Springs are retargeted,
    /// never snapped, so every mode change is a morph.
    fn retarget(&mut self) {
        let (w, h, r) = match self.mode {
            WindowMode::Expanded => (PANEL_W, PANEL_H, PANEL_RADIUS),
            // `Hidden` keeps the pill's target: the window is hidden while the
            // springs settle, so when it comes back there is no jump.
            WindowMode::Collapsed | WindowMode::Hidden => (PILL_W, PILL_H, PILL_RADIUS),
        };
        self.width.set_config(SpringConfig::OPEN);
        self.height.set_config(SpringConfig::OPEN);
        self.radius.set_config(SpringConfig::GENERIC);
        self.width.set_target(w);
        self.height.set_target(h);
        self.radius.set_target(r);
    }

    /// Hide for a fullscreen app, remembering what to come back to.
    ///
    /// Restoring to `Expanded` unconditionally would pop the panel open in the
    /// user's face after every fullscreen video, so the pre-hide mode is kept.
    pub fn hide(&mut self) {
        if self.mode != WindowMode::Hidden {
            self.mode_before_hide = self.mode;
        }
        self.mode = WindowMode::Hidden;
    }

    /// Undo [`hide`](Self::hide). No-op unless currently hidden.
    pub fn restore(&mut self) {
        if self.mode == WindowMode::Hidden {
            self.mode = self.mode_before_hide;
            self.retarget();
        }
    }

    /// True while suppressed for a fullscreen app.
    pub fn hidden(&self) -> bool {
        self.mode == WindowMode::Hidden
    }

    pub fn logical_width(&self) -> f32 {
        self.width.value()
    }

    pub fn logical_height(&self) -> f32 {
        self.height.value()
    }

    /// The radius spring's live value. The UI derives radius from `openness`,
    /// so this exists for parity assertions and the debug overlay.
    #[allow(dead_code)]
    pub fn logical_radius(&self) -> f32 {
        self.radius.value()
    }

    /// Advance springs by real elapsed time. Used by the frame loop so a dropped
    /// frame shortens the animation instead of slowing it down.
    pub fn step_dt(&mut self, dt: f32) -> bool {
        let a = self.width.step(dt);
        let b = self.height.step(dt);
        let c = self.radius.step(dt);
        a || b || c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_collapsed_at_pill_geometry() {
        let s = IslandState::collapsed();
        assert_eq!(s.mode(), WindowMode::Collapsed);
        assert_eq!(s.logical_width(), PILL_W);
        assert_eq!(s.logical_height(), PILL_H);
        assert_eq!(s.logical_radius(), PILL_RADIUS);
    }

    #[test]
    fn toggle_expands_then_collapses() {
        let mut s = IslandState::collapsed();
        s.toggle();
        assert!(s.expanded());
        s.toggle();
        assert_eq!(s.mode(), WindowMode::Collapsed);
    }

    #[test]
    fn toggle_retargets_springs_not_snaps() {
        let mut s = IslandState::collapsed();
        s.toggle();
        assert!(s.step_dt(1.0 / 60.0), "expansion must animate");
        assert!(s.logical_width() > PILL_W && s.logical_width() < PANEL_W);
    }

    #[test]
    fn springs_settle_on_panel_geometry() {
        let mut s = IslandState::collapsed();
        s.toggle();
        for _ in 0..200 {
            s.step_dt(1.0 / 60.0);
        }
        assert_eq!(s.logical_width(), PANEL_W);
        assert_eq!(s.logical_height(), PANEL_H);
        assert_eq!(s.logical_radius(), PANEL_RADIUS);
        assert!(
            !s.step_dt(1.0 / 60.0),
            "settled springs must request no more frames"
        );
    }

    #[test]
    fn restore_returns_to_the_mode_we_left() {
        let mut s = IslandState::collapsed();
        s.toggle();
        assert!(s.expanded());
        s.hide();
        assert!(s.hidden());
        s.restore();
        assert!(s.expanded(), "must not silently collapse the user's panel");
    }

    #[test]
    fn restore_from_collapsed_stays_collapsed() {
        let mut s = IslandState::collapsed();
        s.hide();
        s.restore();
        assert_eq!(s.mode(), WindowMode::Collapsed);
    }

    #[test]
    fn hide_is_idempotent_and_does_not_clobber_the_remembered_mode() {
        let mut s = IslandState::collapsed();
        s.toggle();
        s.hide();
        s.hide();
        s.restore();
        assert!(s.expanded());
    }

    #[test]
    fn restore_is_a_noop_when_not_hidden() {
        let mut s = IslandState::collapsed();
        s.restore();
        assert_eq!(s.mode(), WindowMode::Collapsed);
    }

    #[test]
    fn hidden_toggle_opens_the_panel() {
        let mut s = IslandState::collapsed();
        s.hide();
        assert_eq!(s.mode(), WindowMode::Hidden);
        s.toggle();
        assert_eq!(s.mode(), WindowMode::Expanded);
    }
}
