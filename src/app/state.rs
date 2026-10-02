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
    width: Spring,
    height: Spring,
    radius: Spring,
}

impl IslandState {
    pub fn collapsed() -> Self {
        Self {
            mode: WindowMode::Collapsed,
            width: Spring::new(PILL_W, SpringConfig::OPEN),
            height: Spring::new(PILL_H, SpringConfig::OPEN),
            radius: Spring::new(PILL_RADIUS, SpringConfig::GENERIC),
        }
    }

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
        let (w, h, r) = if self.expanded() {
            (PANEL_W, PANEL_H, PANEL_RADIUS)
        } else {
            (PILL_W, PILL_H, PILL_RADIUS)
        };
        self.width.set_config(SpringConfig::OPEN);
        self.height.set_config(SpringConfig::OPEN);
        self.radius.set_config(SpringConfig::GENERIC);
        self.width.set_target(w);
        self.height.set_target(h);
        self.radius.set_target(r);
    }

    pub fn hide(&mut self) {
        self.mode = WindowMode::Hidden;
    }

    pub fn logical_width(&self) -> f32 {
        self.width.value()
    }

    pub fn logical_height(&self) -> f32 {
        self.height.value()
    }

    pub fn logical_radius(&self) -> f32 {
        self.radius.value()
    }

    /// Advance springs one frame. Returns true when another frame is needed.
    pub fn step(&mut self) -> bool {
        // 60 Hz nominal; the frame loop paces to the display refresh.
        let dt = 1.0 / 60.0;
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
        assert!(s.step(), "expansion must animate");
        assert!(s.logical_width() > PILL_W && s.logical_width() < PANEL_W);
    }

    #[test]
    fn springs_settle_on_panel_geometry() {
        let mut s = IslandState::collapsed();
        s.toggle();
        for _ in 0..200 {
            s.step();
        }
        assert_eq!(s.logical_width(), PANEL_W);
        assert_eq!(s.logical_height(), PANEL_H);
        assert_eq!(s.logical_radius(), PANEL_RADIUS);
        assert!(!s.step(), "settled springs must request no more frames");
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
