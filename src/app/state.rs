//! Island window state machine.
//!
//! The island has four modes; size and interactivity are outputs of the mode, not
//! separate settings, so the visual and the hit-testing can never disagree.

use crate::core::anim::{Spring, SpringConfig};

/// Reference geometry, verbatim from the reference design's external-display mode.
pub const PILL_W: f32 = 185.0;
pub const PANEL_W: f32 = 640.0;
pub const PILL_H: f32 = 32.0;
/// Collapsed bottom corner radius. The top corners are square (the island
/// hangs from the screen edge), so the bottom pair carries the whole shape:
/// `h / 2` gives a true semicircular bottom, the iOS hanging-pill look.
pub const PILL_RADIUS: f32 = 16.0;
pub const PANEL_RADIUS: f32 = 36.0;
pub const PANEL_H: f32 = 200.0;

/// Dwell time before hover expands the island, in seconds (Phase 2, hover).
/// Short enough to feel like a reaction, long enough not to fire on a mouse
/// crossing the top edge while scrolling.
pub const HOVER_DWELL: f32 = 0.08;

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

/// Who currently owns the island's visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserVis {
    /// Startup default: the fullscreen probe may hide/restore it.
    #[default]
    Auto,
    /// User hid it with Ctrl+Shift+Z; nothing may bring it back but the hotkey.
    Hidden,
    /// User brought it back with Ctrl+Shift+Z; it stays over everything.
    Pinned,
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
    /// Who decides visibility: the automatic fullscreen policy, or the user
    /// via Ctrl+Shift+Z (sticky `Hidden`/`Pinned` until toggled again).
    user_vis: UserVis,
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
            user_vis: UserVis::Auto,
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

    /// Who owns visibility right now (Ctrl+Shift+Z flips it).
    pub fn user_vis(&self) -> UserVis {
        self.user_vis
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

    /// Open the panel (from anywhere, including `Hidden`): the tray's
    /// "Settings…" path to a state the user can always see. Overrides a user
    /// hide — the tray menu is the user, asking to be seen.
    pub fn expand(&mut self) {
        self.mode = WindowMode::Expanded;
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

    /// Ctrl+Shift+Z: user-driven hide/show, overriding the fullscreen policy.
    ///
    /// `Hidden` and `Pinned` are sticky on purpose — once the user takes
    /// control, the 1 Hz fullscreen probe may no longer make the island
    /// vanish on them ("goes and comes back"): `Hidden` stays hidden through
    /// fullscreen exits, `Pinned` stays visible even over a fullscreen app.
    pub fn toggle_visibility(&mut self) {
        match self.user_vis {
            UserVis::Auto | UserVis::Pinned => {
                self.user_vis = UserVis::Hidden;
                self.hide();
            }
            UserVis::Hidden => {
                self.user_vis = UserVis::Pinned;
                self.restore();
            }
        }
    }

    /// A fullscreen app appeared on our monitor. Only the automatic policy
    /// may hide for it; a user-hidden or pinned island is left alone.
    pub fn fullscreen_enter(&mut self) {
        if self.user_vis == UserVis::Auto {
            self.hide();
        }
    }

    /// The fullscreen app went away. Restores only what the automatic policy
    /// hid — never a user-hidden island.
    pub fn fullscreen_exit(&mut self) {
        if self.user_vis == UserVis::Auto {
            self.restore();
        }
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

        // Expanded is expanded, however it got there (hover, click, hotkey):
        // cursor off the body → collapse. This was the stuck-panel bug — a
        // click-opened panel was exempt and only the tray could close it.
        if self.mode == WindowMode::Expanded && !pointer_over {
            self.hover.leave_expansion();
            self.mode = WindowMode::Collapsed;
            self.retarget();
            return true;
        }

        let was_expanded = self.expanded();
        if self.hover.update(pointer_over, dt) {
            self.hover.enter_expansion();
            self.mode = WindowMode::Expanded;
            self.retarget();
        }
        self.expanded() != was_expanded
    }

    pub fn logical_width(&self) -> f32 {
        self.width.value()
    }

    /// The window frame width. Fixed: `max(PILL_W, PANEL_W)` plus the ear wings,
    /// so the HWND and swapchain are built once and the expand/collapse morph
    /// never resizes them (that resize-per-frame was the animation jank).
    pub fn frame_width(&self) -> f32 {
        PANEL_W.max(PILL_W) + 2.0 * crate::ui::layout::EAR_W
    }

    pub fn frame_height(&self) -> f32 {
        PANEL_H.max(PILL_H)
    }

    /// Logical left inset that centres the current body inside the fixed frame.
    /// Also the horizontal hit-test offset: subtract it from a click's `x`.
    pub fn body_offset_x(&self) -> f32 {
        (self.frame_width() - self.logical_width()) / 2.0
    }

    /// Window frame width: body + ear wings on both sides.

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
    fn ctrl_shift_z_hides_then_pins_visible_everywhere() {
        let mut s = IslandState::collapsed();
        s.toggle_visibility();
        assert!(s.hidden(), "Z must hide the island");
        assert_eq!(s.user_vis(), UserVis::Hidden);
        s.toggle_visibility();
        assert!(!s.hidden(), "Z again must bring it back");
        assert_eq!(s.user_vis(), UserVis::Pinned);
    }

    #[test]
    fn user_hidden_ignores_fullscreen_exit() {
        let mut s = IslandState::collapsed();
        s.toggle_visibility(); // Hidden
        s.fullscreen_exit();
        assert!(s.hidden(), "a user-hidden island must stay hidden");
        s.toggle_visibility(); // Pinned
        s.fullscreen_enter();
        assert!(!s.hidden(), "a pinned island stays over a fullscreen app");
    }

    #[test]
    fn auto_mode_hides_and_restores_with_fullscreen() {
        let mut s = IslandState::collapsed();
        s.fullscreen_enter();
        assert!(s.hidden());
        s.fullscreen_exit();
        assert!(!s.hidden());
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
    fn the_frame_is_fixed_so_morphs_never_resize_the_window() {
        let mut s = IslandState::collapsed();
        let (fw, fh) = (s.frame_width(), s.frame_height());
        assert_eq!(fw, PANEL_W + 2.0 * crate::ui::layout::EAR_W);
        assert_eq!(fh, PANEL_H);
        s.toggle();
        for _ in 0..200 {
            s.step_dt(1.0 / 60.0);
        }
        assert_eq!(
            (s.frame_width(), s.frame_height()),
            (fw, fh),
            "frame must not move between modes — the morph is a clip offset"
        );
    }

    #[test]
    fn the_body_is_centred_in_the_frame() {
        let s = IslandState::collapsed();
        assert_eq!(
            s.body_offset_x(),
            (PANEL_W - PILL_W) / 2.0 + crate::ui::layout::EAR_W
        );
        let mut e = IslandState::collapsed();
        e.toggle();
        for _ in 0..200 {
            e.step_dt(1.0 / 60.0);
        }
        assert_eq!(e.body_offset_x(), crate::ui::layout::EAR_W);
    }

    #[test]
    fn hover_dwell_expands_and_leaving_collapses() {
        let mut s = IslandState::collapsed();
        // Below the dwell threshold nothing happens.
        for _ in 0..5 {
            assert!(!s.hover_step(true, HOVER_DWELL / 10.0));
        }
        assert_eq!(s.mode(), WindowMode::Collapsed);

        // Crossing it expands, and the change is reported once.
        assert!(s.hover_step(true, HOVER_DWELL + 0.02));
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
    fn click_opened_panel_collapses_when_the_cursor_leaves() {
        let mut s = IslandState::collapsed();
        s.toggle(); // opened by click/hotkey, not by hover
        assert!(s.expanded());
        assert!(!s.expanded_by_hover());
        // Cursor sits elsewhere: one step later the panel must fold itself,
        // with no tray click.
        assert!(s.hover_step(false, 0.016));
        assert_eq!(s.mode(), WindowMode::Collapsed);
        // ... while the cursor on the body keeps it open.
        let mut k = IslandState::collapsed();
        k.toggle();
        assert!(!k.hover_step(true, 0.016));
        assert!(k.expanded());
    }

    #[test]
    fn hover_dwell_restarts_after_leaving() {
        let mut s = IslandState::collapsed();
        // Each visit below the dwell threshold, then leave: the dwell is never
        // banked across trips outside.
        for _ in 0..2 {
            s.hover_step(true, HOVER_DWELL / 2.0);
            s.hover_step(false, 0.016);
        }
        assert_eq!(s.mode(), WindowMode::Collapsed);
        // An uninterrupted visit does fire.
        s.hover_step(true, HOVER_DWELL + 0.02);
        assert_eq!(s.mode(), WindowMode::Expanded);
    }

    #[test]
    fn a_hotkey_opened_panel_folds_when_the_cursor_leaves() {
        let mut s = IslandState::collapsed();
        s.toggle();
        assert!(s.expanded());
        // Cursor off the panel → it folds. Whatever opened it (hotkey, click,
        // hover), a panel never outlives the cursor's presence — otherwise it
        // sits there until the user finds the tray icon.
        assert!(s.hover_step(false, 0.05));
        assert!(!s.expanded());
        // Dwell must not re-expand it while the cursor is away.
        for _ in 0..10 {
            assert!(!s.hover_step(false, 0.05));
        }
        assert!(!s.expanded());
    }

    #[test]
    fn dwell_does_not_fight_an_open_panel_under_the_cursor() {
        let mut s = IslandState::collapsed();
        s.toggle();
        // Cursor stays on the body: the panel remains open and no collapse is
        // reported, so the morph is never driven backwards by hover bookkeeping.
        for _ in 0..10 {
            assert!(!s.hover_step(true, 0.05));
        }
        assert!(s.expanded());
    }

    #[test]
    fn dwell_machine_is_pure_of_springs() {
        let mut h = Hover::new();
        assert!(!h.expanded());
        // `HOVER_DWELL` exactly is the threshold and must fire (>= not >).
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
