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
pub const PANEL_H: f32 = 248.0;

/// The velocity the pop spring is kicked with. With `SpringConfig::POP`
/// (response 0.38 s, damping 0.33) this rings the scale to 1.113 at +100 ms
/// and back down to 0.963 at +300 ms before settling: the reference's
/// inflate-then-squash, to three decimal places.
const POP_VELOCITY: f32 = 3.0;

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
    /// The hover state: the minininized pill grown into a fuller pill.
    Peek,
    /// Expanded panel.
    Expanded,
}

/// Peek corner radius: `h / 2`, a true capsule. The collapsed pill's bottom is
/// already a semicircle; the hover pill rounds its top corners the same way.
/// Mirrors `ui::layout::PEEK_H_REF / 2`.
pub const PEEK_RADIUS: f32 = crate::ui::layout::PEEK_H_REF / 2.0;
/// How far the hover (peek) pill drops below the screen top, in logical px.
pub const PEEK_DROP: f32 = 4.0;

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
    drop: Spring,
    /// The pop: a scale on the island's size, 1.0 at rest, kicked with a
    /// velocity when the island grows. Rings once (inflate, dip, settle).
    pop: Spring,
    /// Opened automatically by a playback start; survives cursor-leave and
    /// collapses again when playback stops. Cleared on any explicit user action.
    auto_expanded: bool,
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
            drop: Spring::new(0.0, SpringConfig::GENERIC),
            pop: Spring::new(1.0, SpringConfig::POP),
            auto_expanded: false,
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

    /// True when the island is currently expanded because playback started.
    #[allow(dead_code)]
    pub fn auto_expanded(&self) -> bool {
        self.auto_expanded
    }

    /// Auto-expand when a track starts playing. Unused now that playback no
    /// longer opens the island; kept for tests that pin the policy.
    #[allow(dead_code)]
    /// Auto-expand when a track starts playing. No-op if already expanded or
    /// currently hidden by a fullscreen policy. Returns whether the mode changed.
    pub fn open_for_playback(&mut self) -> bool {
        if self.mode == WindowMode::Hidden || self.expanded() {
            return false;
        }
        self.mode = WindowMode::Expanded;
        self.auto_expanded = true;
        self.retarget();
        true
    }

    #[allow(dead_code)]
    /// Collapse again once playback stops, but only if WE opened it. Returns
    /// whether the mode changed.
    pub fn close_after_playback(&mut self) -> bool {
        if !self.auto_expanded || self.mode != WindowMode::Expanded {
            return false;
        }
        self.auto_expanded = false;
        self.mode = WindowMode::Collapsed;
        self.retarget();
        true
    }

    /// Open the panel because files were just dropped on the island
    /// (NotchDrop's "the notch answers the drop" behaviour).
    ///
    /// A drop is a deliberate gesture aimed at the notch, so unlike hover it
    /// opens immediately with no dwell. The panel is NOT marked
    /// `auto_expanded`: a drop is a one-shot action, and the normal
    /// cursor-leave rule should close it once the user moves away — parking a
    /// file must not pin a panel over their desktop.
    pub fn open_for_drop(&mut self) -> bool {
        if self.mode == WindowMode::Hidden || self.expanded() {
            return false;
        }
        self.auto_expanded = false;
        self.mode = WindowMode::Expanded;
        self.retarget();
        true
    }

    /// Flip between pill and panel. From `Hidden`, a toggle re-opens the pill
    /// (the panel was already dismissed), so the user never lands on a state
    /// they cannot see.
    pub fn toggle(&mut self) {
        self.auto_expanded = false;
        self.mode = match self.mode {
            WindowMode::Expanded => WindowMode::Collapsed,
            _ => WindowMode::Expanded,
        };
        self.retarget();
    }

    /// Point the springs at the current mode's geometry. Springs are retargeted,
    /// never snapped, so every mode change is a morph.
    ///
    /// VNotch's staged morph: the height (the "drop") leads, the width (the
    /// "spread") follows, the corner radius trails both. Collapse runs about
    /// 2/3 of the expand duration (faster, critically damped) per the VNotch
    /// frame analysis — the reference design's OPEN config was shared across
    /// both directions, which left collapse as slow as expand.
    fn retarget(&mut self) {
        let (w, h, r, d) = match self.mode {
            WindowMode::Expanded => (PANEL_W, PANEL_H, PANEL_RADIUS, 0.0),
            WindowMode::Peek => (
                crate::ui::layout::PEEK_W_REF,
                crate::ui::layout::PEEK_H_REF,
                PEEK_RADIUS,
                PEEK_DROP,
            ),
            // `Hidden` keeps the pill's target: the window is hidden while the
            // springs settle, so when it comes back there is no jump.
            WindowMode::Collapsed | WindowMode::Hidden => (PILL_W, PILL_H, PILL_RADIUS, 0.0),
        };
        match self.mode {
            // VNotch's morph BOUNCES: height drops first and overshoots, width
            // spreads after it, radius trails — each springs past its target and
            // settles back, which is what makes the island read as alive instead
            // of as a box that changed size.
            WindowMode::Expanded => {
                self.width.set_config(SpringConfig::new(0.34, 1.0));
                self.height.set_config(SpringConfig::new(0.36, 1.0));
                self.radius.set_config(SpringConfig::new(0.40, 1.0));
                self.drop.set_config(SpringConfig::new(0.30, 1.0));
            }
            // Hover: a short bob. The drop is the only spring that moves, so
            // its overshoot is what the pill's bounce is made of; peek_t clamps
            // at 1 while the body keeps dipping, so the shape stays a capsule
            // through the whole bounce.
            WindowMode::Peek => {
                // No bounce: the hover drop is one monotonic glide from the
                // notch down into the floating pill. The reference's hover
                // reads as the body detaching and the top corners rounding
                // off, never as a spring ringing past its rest spot.
                self.width.set_config(SpringConfig::new(0.26, 1.0));
                self.height.set_config(SpringConfig::new(0.30, 1.0));
                self.radius.set_config(SpringConfig::new(0.30, 1.0));
                self.drop.set_config(SpringConfig::new(0.34, 1.0));
            }
            // Collapse into the smaller state including a peek release:
            // critically damped, so the body glides straight back into the pill
            // with no ring. The height is the slower axis on purpose: the panel
            // reads as coming down into the pill instead of snapping shut,
            // which is what the reference's ~500 ms collapse looks like.
            _ => {
                self.width.set_config(SpringConfig::new(0.26, 1.0));
                self.height.set_config(SpringConfig::new(0.32, 1.0));
                self.radius.set_config(SpringConfig::new(0.28, 1.0));
                self.drop.set_config(SpringConfig::new(0.22, 1.0));
            }
        }
        self.width.set_target(w);
        self.height.set_target(h);
        self.radius.set_target(r);
        self.drop.set_target(d);

        // The reference notch INFLATES before it morphs: the pill pops ~11%
        // wider, dips ~4% below its rest size, then settles (measured off
        // vnotch-dynamic.gif frames 115-130). A spring at rest that is given a
        // velocity rings exactly that way, so kick it instead of hand-rolling
        // a second animation. Only on the way UP: the reference collapse is
        // straight, with no pop.
        if matches!(self.mode, WindowMode::Expanded) {
            self.pop.set_config(SpringConfig::POP);
            self.pop.set_velocity(POP_VELOCITY);
        }
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

    /// Fold the cursor's presence into the mode machine. Returns `true` when
    /// the mode changed this call (caller needs a repaint).
    ///
    /// Three-way ladder, the user's explicit model: minimized pill is the rest
    /// state, hover grows it into the peek pill, a press opens the panel.
    /// Hover therefore never reaches [`WindowMode::Expanded`]. Leaving the
    /// body folds whatever is open; an auto-expanded (playback) panel stays
    /// until playback stops.
    pub fn hover_step(&mut self, pointer_over: bool) -> bool {
        // Hidden for a fullscreen app: hover must not pull the island back
        // over the video the user is watching.
        if self.mode == WindowMode::Hidden {
            return false;
        }

        // The panel folds when the cursor leaves, however it was opened
        // (click, hotkey). Playback keeps it.
        if self.mode == WindowMode::Expanded && !pointer_over && !self.auto_expanded {
            self.mode = WindowMode::Collapsed;
            self.retarget();
            return true;
        }

        // Peek tracks the cursor: over the pill it grows, away it shrinks.
        if self.mode == WindowMode::Collapsed && pointer_over {
            self.mode = WindowMode::Peek;
            self.retarget();
            return true;
        }
        if self.mode == WindowMode::Peek && !pointer_over {
            self.mode = WindowMode::Collapsed;
            self.retarget();
            return true;
        }
        false
    }

    pub fn logical_width(&self) -> f32 {
        self.width.value() * self.pop_scale()
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

    pub fn body_offset_y(&self) -> f32 {
        self.drop.value().max(0.0)
    }

    /// Window frame width: body + ear wings on both sides.

    pub fn logical_height(&self) -> f32 {
        self.height.value() * self.pop_scale()
    }

    /// How much the pop is inflating the island right now. The pop only
    /// touches the pill: its influence fades out as the island morphs into
    /// the panel, so an expanding panel never breathes.
    fn pop_scale(&self) -> f32 {
        let open = crate::ui::layout::openness(self.width.value(), self.height.value());
        1.0 + (self.pop.value() - 1.0) * (1.0 - open)
    }

    /// The radius spring's live value. The UI derives radius from `openness`,
    /// so this exists for parity assertions and the debug overlay.
    #[allow(dead_code)]
    pub fn logical_radius(&self) -> f32 {
        self.radius.value()
    }

    /// Advance springs by real elapsed time. Used by the frame loop so a dropped
    /// frame shortens the animation instead of slowing it down.
    /// Whether any morph spring is still moving — a read-only peek for code
    /// that must not stall a live morph (the net/WLAN poll costs ~80 ms).
    pub fn busy(&self) -> bool {
        self.width.is_animating()
            || self.height.is_animating()
            || self.radius.is_animating()
            || self.drop.is_animating()
            || self.pop.is_animating()
    }

    pub fn step_dt(&mut self, dt: f32) -> bool {
        let a = self.width.step(dt);
        let b = self.height.step(dt);
        let c = self.radius.step(dt);
        let d = self.drop.step(dt);
        let e = self.pop.step(dt);
        a || b || c || d || e
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
    fn hover_peeks_the_pill_and_leaving_folds_it_back() {
        let mut s = IslandState::collapsed();
        // Hover grows the minimized pill into the peek pill — and stops there.
        assert!(s.hover_step(true));
        assert_eq!(s.mode(), WindowMode::Peek);
        // Settled: it stays a pill, never the panel.
        for _ in 0..50 {
            assert!(!s.hover_step(true));
        }
        assert_eq!(s.mode(), WindowMode::Peek);
        assert!(!s.expanded());
        assert!(s.logical_width() <= crate::ui::layout::PEEK_W_REF + 0.5);

        // Away from it → back to the minimized pill.
        assert!(s.hover_step(false));
        assert_eq!(s.mode(), WindowMode::Collapsed);
        assert!(!s.expanded());

        // Press the pill: that is the panel.
        s.hover_step(true);
        s.toggle();
        assert!(s.expanded());
        // The panel stays under the cursor.
        assert!(!s.hover_step(true));
        assert!(s.expanded());
        // Cursor leaves → all the way back to the minimized pill.
        assert!(s.hover_step(false));
        assert_eq!(s.mode(), WindowMode::Collapsed);
    }

    #[test]
    fn click_opened_panel_collapses_when_the_cursor_leaves() {
        let mut s = IslandState::collapsed();
        s.toggle(); // opened by click/hotkey, not by hover
        assert!(s.expanded());
        // Cursor sits elsewhere: one step later the panel must fold itself,
        // with no tray click.
        assert!(s.hover_step(false));
        assert_eq!(s.mode(), WindowMode::Collapsed);
        // ... while the cursor on the body keeps it open.
        let mut k = IslandState::collapsed();
        k.toggle();
        assert!(!k.hover_step(true));
        assert!(k.expanded());
    }

    #[test]
    fn a_hotkey_opened_panel_folds_when_the_cursor_leaves() {
        let mut s = IslandState::collapsed();
        s.toggle();
        assert!(s.expanded());
        // Cursor off the panel → it folds. Whatever opened it (hotkey, click),
        // a panel never outlives the cursor's presence — otherwise it sits
        // there until the user finds the tray icon.
        assert!(s.hover_step(false));
        assert!(!s.expanded());
        // Returning to the body peeks the pill but must NOT reopen the panel.
        assert!(s.hover_step(true));
        for _ in 0..10 {
            assert!(!s.hover_step(true));
        }
        assert_eq!(s.mode(), WindowMode::Peek);
    }

    #[test]
    fn hover_is_suppressed_while_hidden() {
        let mut s = IslandState::collapsed();
        s.hide();
        // Hover while fullscreen-hid must not open the panel on restore.
        for _ in 0..20 {
            s.hover_step(true);
        }
        assert!(s.hidden(), "hover must not un-hide the island");
    }

    #[test]
    fn starting_playback_opens_the_island_and_ending_it_closes_it() {
        let mut s = IslandState::collapsed();
        assert!(s.open_for_playback(), "a track starting must expand the pill");
        assert!(s.expanded());
        assert!(s.auto_expanded());
        // While auto-expanded, the cursor leaving the body must not collapse it.
        for _ in 0..20 {
            s.hover_step(false);
        }
        assert!(s.expanded(), "an auto-opened panel survives a cursor-leave");
        assert!(s.close_after_playback(), "playback ending must close it again");
        assert!(!s.expanded());
    }

    #[test]
    fn a_user_toggle_beats_the_playback_policy() {
        let mut s = IslandState::collapsed();
        assert!(s.open_for_playback());
        s.toggle();
        assert!(!s.expanded(), "the user collapsed it");
        // Playback ending after a manual close must not re-open anything.
        assert!(!s.close_after_playback());
        assert!(!s.expanded());
    }

    #[test]
    fn playback_never_opens_over_a_hidden_island() {
        let mut s = IslandState::collapsed();
        s.hide();
        assert!(!s.open_for_playback(), "fullscreen video wins");
        assert!(s.hidden());
    }

    /// VNotch's staged morph: the height (the drop) leads, the width (the
    /// spread) follows. Both springs leave the pill together, so the one with
    /// the slower config must be behind a few frames in.
    ///
    /// Measured on the raw springs, not `logical_*`: those multiply in the pop
    /// scale, which inflates both axes and skews progress toward the wider
    /// target. This assertion is about the morph, not the pop.
    #[test]
    fn the_morph_stages_height_before_width() {
        let mut s = IslandState::collapsed();
        s.toggle();
        for _ in 0..4 {
            s.step_dt(1.0 / 60.0);
        }
        let hp = (s.height.value() - PILL_H) / (PANEL_H - PILL_H);
        let wp = (s.width.value() - PILL_W) / (PANEL_W - PILL_W);
        assert!(hp > wp, "height must lead: hp={hp} wp={wp}");
    }

    /// VNotch: collapse runs about 2/3 of the expand duration. Measured as
    /// frames to within half a px of the target — the spring's own settle flag
    /// scales its velocity threshold with ω₀, so a faster spring needs to be
    /// moving *less* to count as settled and the frame counts are not
    /// comparable across configs.
    #[test]
    fn collapse_is_faster_than_expand() {
        fn frames_to(s: &mut IslandState, target: (f32, f32, f32)) -> u32 {
            let near = |v: f32, t: f32| (v - t).abs() < 0.5;
            let mut frames = 0;
            while !(near(s.logical_width(), target.0)
                && near(s.logical_height(), target.1)
                && near(s.logical_radius(), target.2))
            {
                s.step_dt(1.0 / 60.0);
                frames += 1;
                assert!(frames < 2_000, "never converged");
            }
            frames
        }
        let mut open = IslandState::collapsed();
        open.toggle();
        let open_frames = frames_to(&mut open, (PANEL_W, PANEL_H, PANEL_RADIUS));
        let mut closing = IslandState::collapsed();
        closing.toggle();
        frames_to(&mut closing, (PANEL_W, PANEL_H, PANEL_RADIUS));
        closing.toggle();
        let close_frames = frames_to(&mut closing, (PILL_W, PILL_H, PILL_RADIUS));
        assert!(
            close_frames < open_frames,
            "collapse must be the faster morph: {close_frames} >= {open_frames}"
        );
    }

    /// The hover glide must be monotonic: no inflation past the pill's rest
    /// width, no squash below it. The reference's hover drop reads as the
    /// body detaching and the top corners rounding off, never as a spring
    /// ringing past its rest spot. It still settles on the exact 185 width.
    #[test]
    fn hovering_never_bounces_the_pill_past_its_rest_width() {
        let mut s = IslandState::collapsed();
        assert!(s.hover_step(true), "cursor over the pill must peek it");
        let (mut peak, mut trough) = (s.logical_width(), s.logical_width());
        let mut t = 0.0;
        while s.step_dt(1.0 / 240.0) && t < 2.0 {
            t += 1.0 / 240.0;
            peak = peak.max(s.logical_width());
            trough = trough.min(s.logical_width());
        }
        let ratio_peak = peak / crate::ui::layout::PEEK_W_REF;
        let ratio_trough = trough / crate::ui::layout::PEEK_W_REF;
        assert!(
            ratio_peak <= 1.0 + 1e-3,
            "hover must not overshoot: peak {ratio_peak}"
        );
        assert!(
            ratio_trough >= 1.0 - 1e-3,
            "hover must not squash: trough {ratio_trough}"
        );
        assert!(
            (s.logical_width() - crate::ui::layout::PEEK_W_REF).abs() < 0.06,
            "must settle on exactly 185: {}",
            s.logical_width()
        );
    }
}
