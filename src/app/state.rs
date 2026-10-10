//! Island window state machine.
//!
//! The island has four modes; size and interactivity are outputs of the mode, not
//! separate settings, so the visual and the hit-testing can never disagree.

use crate::core::anim::{Spring, SpringConfig};

/// Reference geometry, verbatim from the reference design's external-display mode.
pub const PILL_W: f32 = 185.0;
pub const PILL_H: f32 = 32.0;
/// Collapsed bottom corner radius. The top corners are square (the island
/// hangs from the screen edge), so the bottom pair carries the whole shape:
/// `h / 2` gives a true semicircular bottom, the iOS hanging-pill look.
pub const PILL_RADIUS: f32 = 16.0;
pub const PANEL_W: f32 = 640.0;
pub const PANEL_H: f32 = 200.0;
pub const PANEL_RADIUS: f32 = 28.0;

/// Dwell time before hover expands the island, in seconds (Phase 2, hover).
/// Short enough to feel like a reaction, long enough not to fire on a mouse
/// crossing the top edge while scrolling.
pub const HOVER_DWELL: f32 = 0.3;

/// Pure hover-dwell state machine. Owns *when* hover asks for expansion;
/// [`IslandState`] owns the resulting geometry springs.
///
/// Separate from the springs on purpose: dwell timing must stay testable without
/// integrating a spring, and the springs must stay testable without a clock.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Hover {
    /// Cursor is currently over the island.
    inside: bool,
    /// Expanded because of hover (as opposed to hotkey or click).
    expanded_by_hover: bool,
    /// Seconds the cursor has been inside, accumulated by the caller.
    dwell: f32,
}

impl Hover {
    pub const fn new() -> Self {
        Self {
            inside: false,
            expanded_by_hover: false,
            dwell: 0.0,
        }
    }

    /// Feed the current pointer state each frame. Returns `true` when the
    /// dwell threshold has just been crossed (one edge, so the caller can
    /// consume it as a discrete event).
    pub fn update(&mut self, inside: bool, dt: f32) -> bool {
        if inside {
            if !self.inside {
                // Re-entering restarts the clock: the dwell must be earned
                // without interruption, not banked across a trip outside.
                self.dwell = 0.0;
            }
            self.inside = true;
            self.dwell += dt.max(0.0);
        } else {
            self.inside = false;
            self.dwell = 0.0;
        }
        self.dwell >= HOVER_DWELL && !self.expanded_by_hover
    }

    /// The hover-expanded state, latched until [`Self::leave_expansion`].
    pub fn expanded(&self) -> bool {
        self.expanded_by_hover
    }

    /// Latch the hover expansion.
    pub fn enter_expansion(&mut self) {
        self.expanded_by_hover = true;
    }

    /// Unlatch, e.g. because the user toggled the panel closed by hand.
    pub fn leave_expansion(&mut self) {
        self.expanded_by_hover = false;
        self.dwell = 0.0;
    }
}

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
    hover: Hover,
}

impl IslandState {
    pub fn collapsed() -> Self {
        Self {
            mode: WindowMode::Collapsed,
            mode_before_hide: WindowMode::Collapsed,
            width: Spring::new(PILL_W, SpringConfig::OPEN),
            height: Spring::new(PILL_H, SpringConfig::OPEN),
            radius: Spring::new(PILL_RADIUS, SpringConfig::GENERIC),
            hover: Hover::new(),
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

    /// True while expanded *by hover*, which collapses again the moment
    /// the cursor leaves.
    #[allow(dead_code)]
    pub fn expanded_by_hover(&self) -> bool {
        self.hover.expanded()
    }

    /// Fold hover state into the mode machine. Returns `true` when the
    /// mode changed this call (caller needs a repaint).
    ///
    /// `pointer_over` is window-relative hit-testing, done by the caller
    /// from live cursor coordinates: while collapsed the window is
    /// click-through, so `WM_MOUSEMOVE` never arrives — only polling
    /// sees the pointer.
    pub fn hover_step(&mut self, pointer_over: bool, dt: f32) -> bool {
        // Hidden for a fullscreen app: hover must not pull the island back
        // over the video the user is watching.
        if self.mode == WindowMode::Hidden {
            self.hover.leave_expansion();
            return false;
        }

        // Once the panel is open (hotkey or click) hover stops mattering:
        // it must not collapse a panel the user asked to keep.
        if self.mode == WindowMode::Expanded && !self.hover.expanded() {
            self.hover.leave_expansion();
            return false;
        }

        let was_expanded = self.expanded();
        if self.hover.update(pointer_over, dt) {
            self.hover.enter_expansion();
            self.mode = WindowMode::Expanded;
            self.retarget();
        } else if self.hover.expanded() && !pointer_over {
            self.hover.leave_expansion();
            self.mode = WindowMode::Collapsed;
            self.retarget();
        }
        self.expanded() != was_expanded
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

    #[test]
    fn hover_dwell_expands_and_leaving_collapses() {
        let mut s = IslandState::collapsed();
        // Below the dwell threshold nothing happens.
        for _ in 0..5 {
            assert!(!s.hover_step(true, 0.05));
        }
        assert_eq!(s.mode(), WindowMode::Collapsed);

        // Crossing it expands, and the change is reported once.
        assert!(s.hover_step(true, 0.10));
        assert_eq!(s.mode(), WindowMode::Expanded);
        assert!(s.expanded_by_hover());
        // ... and only once: dwelling longer must not re-fire.
        assert!(!s.hover_step(true, 0.10));

        // Cursor leaves → collapse.
        assert!(s.hover_step(false, 0.016));
        assert_eq!(s.mode(), WindowMode::Collapsed);
        assert!(!s.expanded_by_hover());
    }

    #[test]
    fn hover_dwell_restarts_after_leaving() {
        let mut s = IslandState::collapsed();
        s.hover_step(true, 0.2);
        s.hover_step(false, 0.016);
        // Two more short visits must not add up to the dwell: it is not banked.
        s.hover_step(true, 0.2);
        assert_eq!(s.mode(), WindowMode::Collapsed);
        s.hover_step(false, 0.016);
        s.hover_step(true, 0.2);
        assert_eq!(s.mode(), WindowMode::Collapsed);
        // An uninterrupted visit does fire.
        s.hover_step(true, 0.2);
        assert_eq!(s.mode(), WindowMode::Expanded);
    }

    #[test]
    fn hover_never_collapses_a_hotkey_opened_panel() {
        let mut s = IslandState::collapsed();
        s.toggle();
        assert!(s.expanded());
        // Cursor leaves the (now clickable) panel: it must stay open, because
        // the user asked for it with the hotkey, not with a stray hover.
        for _ in 0..10 {
            assert!(!s.hover_step(false, 0.05));
        }
        assert!(s.expanded(), "hover must not steal a manually opened panel");
    }

    #[test]
    fn dwell_machine_is_pure_of_springs() {
        let mut h = Hover::new();
        assert!(!h.expanded());
        // 0.3 s exactly is the threshold and must fire (>= not >).
        assert!(!h.update(true, HOVER_DWELL - 0.01));
        assert!(h.update(true, 0.02));
        h.enter_expansion();
        assert!(h.expanded());
        // Latched: further dwell cannot re-fire.
        assert!(!h.update(true, 10.0));
        h.leave_expansion();
        assert!(!h.expanded());
    }

    #[test]
    fn hover_is_suppressed_while_hidden() {
        let mut s = IslandState::collapsed();
        s.hide();
        // Dwell while fullscreen-hid must not open the panel on restore.
        for _ in 0..20 {
            s.hover_step(true, 0.05);
        }
        assert!(s.hidden(), "hover must not un-hide the island");
    }
}
