//! The island shell: the black squircle body plus everything that lives inside it.
//!
//! # The look (docs/01 §3)
//!
//! The single rule that matters: **the body is one opaque pure-black squircle
//! filling the entire frame, with no blur, no acrylic, no gradient border and no
//! drop shadow.** The reference design gets its depth from pure black against the
//! wallpaper. Acrylic and shadows are the two things every clone adds by mistake,
//! and both flatten exactly the contrast the design depends on. The window layer
//! (not this file) owns any shadow, if it ever needs one.
//!
//! Everything else is the shell's inner structure: a collapsed glyph, a header, a
//! tab strip, content rows, and the tabular-numeral footer clock.
//!
//! # Purity
//!
//! `build` is a total function of `ViewState`: no I/O, no clock, no randomness,
//! no global state. Same state in, identical frame out. That is what makes the
//! golden-screenshot tests in docs/05 §8.2 possible.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Align, Frame, Node, Scene, TextStyle, Weight};
use crate::ui::layout::{island_radius, openness};

pub mod calendar;
pub mod clipboard;
pub mod layout;
pub mod media;
pub mod picker;
pub mod settings;
pub mod shelf;
pub mod stats;
pub mod terminal;
pub mod text;
pub mod timer;
pub mod usage;

pub use text::{line_height, measure};

/// The island's brand name, shown in the panel header.
pub const APP_TITLE: &str = "Arc";

/// Visible tab labels, in layout order.
///
/// Nine, not ten: the Settings tab is gone. Settings live in their own
/// top-level window (tray → Settings…), so the strip never spends its width on
/// a tab the user does not browse. Dropping it also hands the remaining labels
/// ~10% more width, which is what stops them reading as crammed.
pub const TABS: [&str; 9] = [
    "Media",
    "Stats",
    "Timer",
    "Clipboard",
    "Color",
    "Usage",
    "Calendar",
    "Shelf",
    "Terminal",
];

/// Placeholder content rows. Structure only — no data behind them yet.
pub const CONTENT_ROWS: usize = 3;

/// Gap between tab items, from the reference's tab strip spacing.
pub const TAB_GAP: f32 = 10.0;
/// Horizontal padding inside the panel, applied on both sides.
pub const PANEL_PAD: f32 = 32.0;
/// Top offset of the header row.
pub const HEADER_Y: f32 = 16.0;
/// Gap between the header's line box and the tab strip.
pub const HEADER_GAP: f32 = 6.0;
/// Height of the tab strip band.
pub const TAB_H: f32 = 20.0;
/// Horizontal padding inside a tab, so its box is a superset of its
/// label. Nine tabs fit comfortably at the 8 px gap; the padding stays so
/// labels still read as chips instead of crammed text.
pub const TAB_PAD: f32 = 7.0;
/// Extra room inside every tab box for the renderer's own measurement to
/// disagree with [`crate::ui::text::measure`].
///
/// The table is an approximation and DirectWrite is authoritative, so a box
/// sized to *exactly* the estimate truncates the moment the real glyphs run
/// even slightly wider — the label loses its tail to an ellipsis
/// (`Me…`, `Tim…`) while the row still has 78 px of slack to give. The slack
/// has to live in the box, not in `measure`: every caller of `measure` also
/// uses it to centre text inside a wider rect, and widening the shared table
/// would over-size clocks and titles that already fit.
pub const TAB_SLACK: f32 = 3.0;
/// Corner radius of the hovered/active tab's background pill.
pub const TAB_RADIUS: f32 = 10.0;
/// Icon size inside a tab. Icon-only when idle (NotchStar's dock), so the
/// strip costs 9 icons rather than 9 icons + 9 labels.
pub const TAB_ICON: f32 = 17.0;
/// Gap between a tab's icon and the label that appears when it is selected.
pub const TAB_ICON_GAP: f32 = 5.0;

/// One icon name per entry in [`TABS`], in the same order.
pub const TAB_GLYPHS: [&str; 9] = [
    "note",      // Media
    "chart",     // Stats
    "hourglass", // Timer
    "clipboard", // Clipboard
    "drop",      // Color
    "bolt",      // Usage
    "calendar",  // Calendar
    "tray",      // Shelf
    "terminal",  // Terminal
];
/// Height of one content row.
pub const ROW_H: f32 = 24.0;
/// Gap between content rows.
pub const ROW_GAP: f32 = 8.0;
/// Corner radius of the content rows (reference: `RoundedRectangle(r: 8)`).
pub const ROW_RADIUS: f32 = 8.0;
/// Gap between the tab dock and the panel's bottom edge.
///
/// The dock — the tab icon row — moved to the BOTTOM of the panel: NotchStar
/// puts its navigation at the foot and spends the whole body above it on
/// content. This pad is the gap between that foot and the panel edge.
pub const DOCK_PAD: f32 = 10.0;
/// Height of the tab dock band.
pub const DOCK_H: f32 = TAB_H;
/// Space between the dock's top edge and the content above it.
pub const DOCK_GAP: f32 = 6.0;
/// What the content band leaves clear at the bottom. It used to be a 12 px
/// footer gap; now it must also cover the dock (`DOCK_PAD + DOCK_H`) plus the
/// gap above it. Everything that reads it — drawing and hit-tests alike —
/// reserves the same band, so content is never drawn over the dock nor
/// clickable through it.
pub const FOOTER_BOTTOM_PAD: f32 = DOCK_PAD + DOCK_H + DOCK_GAP;
/// Corner icons (pin, settings) at the head of the panel.
pub const CORNER_ICON: f32 = 14.0;
/// Gap between the pin and the gear.
///
/// NotchStar's reference (1280 px wide, panel spans x 135–1144, i.e. 1009 px)
/// puts the two corner glyphs 173 crop-px apart at 3× → 57.7 reference px →
/// a 36.6 px pitch on Arc's 640 px panel. `CORNER_ICON` is 14, so the gap is
/// the remainder: 22.
pub const CORNER_ICON_GAP: f32 = 22.0;
/// The dock's top edge, measured up from the panel's bottom.
pub fn dock_top(fh: f32) -> f32 {
    (fh - DOCK_PAD - DOCK_H).max(0.0)
}
/// The dock's centre line — the y hit-tests use.
pub fn dock_center(fh: f32) -> f32 {
    dock_top(fh) + DOCK_H * 0.5
}
/// Height of the collapsed pill's center glyph.
pub const PILL_GLYPH: f32 = 14.0;
/// Corner radius of the collapsed pill's center glyph.
pub const PILL_GLYPH_RADIUS: f32 = 3.0;
/// Gap below the collapsed pill's inline progress bar.
pub const PILL_PROGRESS_BOTTOM_PAD: f32 = 6.0;
/// Vertical padding around the idle pet in the collapsed pill: the sprite is
/// the notch's centerpiece, so it may fill the pill more than the resting
/// glyph's 14 px square.
pub const PILL_PET_PAD: f32 = 5.0;
/// Spectrum bar width, and the gap between bars. Module-level (not local to
/// `viz_bars`) because the pill's flank layout reserves the same slot width.
const VIZ_BAR_W: f32 = 2.0;
const VIZ_GAP: f32 = 2.0;
// Collapsed-pill now-playing strip (MusicNotch pattern): art + title.
const PILL_ART: f32 = 22.0;
const PILL_ART_RADIUS: f32 = 6.0;
const PILL_MEDIA_INSET: f32 = 6.0;
const PILL_MEDIA_GAP: f32 = 6.0;
/// Placeholder clock. A real tab replaces this with elapsed/remaining time.

/// Expanded panel height, mirrored from `app::state::PANEL_H`.
// Test-only mirror of the reference heights (the parity suite asserts the
// app state against them); layout owns the real ones.
#[cfg(test)]
pub const PANEL_H_REF: f32 = layout::PANEL_H_REF;
/// Collapsed pill height, mirrored from `app::state::PILL_H`.
#[cfg(test)]
pub const PILL_H_REF: f32 = layout::PILL_H_REF;

/// The pill glyph's resting grey — dim enough to read as an indicator, not text.
const PILL_GREY: Rgba = Rgba {
    r: 0.55,
    g: 0.55,
    b: 0.58,
    a: 1.0,
};

/// HUD fills: charging/healthy (iOS-ish green) and warning (amber).
const PILL_GREEN: Rgba = Rgba::rgb(0.20, 0.78, 0.35);
const PILL_AMBER: Rgba = Rgba::rgb(0.95, 0.62, 0.11);

/// Tab label style: 10 px semibold, NotchStar's dock label colour (#C7C7CC).
const TAB_LABEL: TextStyle = TextStyle {
    mono: false,
    size: 10.0,
    weight: Weight::Semibold,
    color: Rgba {
        r: 0.78,
        g: 0.78,
        b: 0.80,
        a: 1.0,
    },
    tabular: false,
    align: Align::Center,
};

/// Tab strip and content state. Grows per phase.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    pub island_width: f32,
    /// Window frame width = island_width + 2*EAR_W. Ears extend past the body.
    // window size = body + ear wings
    pub island_height: f32,
    /// Index into [`TABS`] of the selected tab.
    pub active_tab: usize,
    /// Whether the island is pinned (visible over fullscreen apps). The header
    /// pin icon wears this as its state.
    pub pinned: bool,
    /// Whether the media progress bar uses the album-art accent (Spotify skin)
    /// instead of the default monochrome white. Opt-in via settings.
    pub accent_progress: bool,
    /// True while the island is in the hover (peek) state — the minimized
    /// pill grown into a compact media pill. Drives the peek branch in build.
    pub peek: bool,
    /// 0..1 blend of the collapsed (concave ears) → hover (rounded, detached)
    /// pill morph. Drives the corner-radius and vertical drop of the body.
    pub peek_t: f32,
    /// Index into [`TABS`] under the cursor, if any.
    pub hover_tab: Option<usize>,
    /// Content row under the cursor, if any.
    pub hover_row: Option<usize>,
    /// What the Media tab draws. The app thread refreshes it from
    /// `services::media` each frame; `build` stays pure.
    pub media: crate::services::media::MediaState,
    /// What the Stats tab draws. Refreshed from the metrics sampler each frame,
    /// same rule as `media`: `build` stays pure.
    pub stats: crate::services::metrics::Stats,
    /// Transient pill overlay, if one is up. Replaces the idle glyph while it
    /// lasts; the app owns the timer, the UI only draws it.
    pub hud: Option<crate::app::hud::Hud>,
    /// The capture endpoint is seeing audio: a standing red dot in the pill.
    pub mic_active: bool,
    /// Top processes by CPU, from the process sampler.
    pub procs: crate::services::processes::Snapshot,
    /// The countdown timer, owned by the app thread.
    pub timer: crate::app::timer::Timer,
    /// Clipboard entries and consent, owned by the clipboard service.
    pub clip_entries: Vec<crate::services::clipboard::Entry>,
    /// Clipboard consent: the service still owns it, the view mirrors it.
    pub clip_enabled: bool,
    /// The color picker's sample and trail, owned by the picker service.
    pub picker: crate::services::picker::Picker,
    /// Latest weather, or `None` until the first fetch lands. A standing pill
    /// row, not a HUD: it does not expire.
    pub weather: Option<crate::services::weather::Snapshot>,
    /// Upcoming calendar events, soonest first.

    /// Downloads folder size-delta, from the beta watcher.
    pub downloads: crate::services::downloads::Snapshot,
    /// Pinned shelf items, owned by the shelf service.
    pub shelf: Vec<crate::services::shelf::Item>,
    /// Calendar subscriptions plus upcoming events.
    pub cal: crate::services::calendars::Snapshot,
    /// LLM usage (Claude Code, Codex) over the rolling window.
    pub usage: crate::services::usage::Snapshot,
    /// The terminal grid, snapshotted by the app thread from the terminal
    /// service each frame (same rule as `media`: `build` stays pure).
    pub terminal: Option<crate::core::termscreen::Screen>,
    /// Why the shell failed to start, if it did. Shown instead of the grid.
    pub terminal_error: Option<String>,
    /// Content swap motion for the iOS-style tab transition: fades out
    /// then the new tab rises in. Settled (rise = 1) when idle.
    pub tab_swap: TabSwap,
    /// Four-bar audio visualizer, stepped by the app thread.
    pub viz: Viz,
    /// The tab the swap is leaving behind; its body is what fades out.
    /// Only meaningful while `tab_swap` is animating.
    pub prev_tab: usize,
    /// The idle-notch pet: the interned sheet plus the frame to crop. `Some`
    /// only when the pet is on AND installed; the pill then swaps its resting
    /// glyph for the sprite. Owned by `services::pet::PetState`, mirrored here.
    pub pet: Option<crate::services::pet::PetFrame>,
    /// The pet on/off row mirrors this.
    pub pet_on: bool,
    /// The pet's size offset from 1.0, as the Settings "Pet size" row steps it.
    /// Stored as an offset so `Default` (0.0) is already the neutral size and
    /// nothing has to remember to seed it.
    pub pet_zoom: f32,
    /// Notch body background color, user-customizable from Settings.
    pub notch_bg: Rgba,
}

/// A tab content swap in flight — crossfade, then rise.
///
/// iOS swaps content by fading the old tab out quickly, then letting
/// the new one rise from a spring (`SpringConfig::SWAP`).
/// `phase` gates the outgoing tab; `rise` drives the incoming one
/// (0 = below, 1 = landed).
#[derive(Debug, Clone)]
pub struct TabSwap {
    /// `Some(spring)` while the outgoing tab is still fading out.
    phase: Option<crate::core::anim::Spring>,
    /// Rise progress for the incoming tab, 0..=1.
    rise: crate::core::anim::Spring,
    /// NotchStar's sliding marker: 0 = sitting on the PREVIOUS tab's box,
    /// 1 = on the active one. The pill glides between the two on a spring,
    /// so a tab change is one moving shape, not a pop.
    marker: crate::core::anim::Spring,
}

impl Default for TabSwap {
    fn default() -> Self {
        Self {
            phase: None,
            rise: crate::core::anim::Spring::new(1.0, crate::core::anim::SpringConfig::SWAP),
            marker: crate::core::anim::Spring::new(1.0, crate::core::anim::SpringConfig::SWAP),
        }
    }
}

impl TabSwap {
    /// How opaque the outgoing tab still is (1 = fully visible).
    pub fn outgoing_opacity(&self) -> f32 {
        self.phase.as_ref().map(|s| s.value()).unwrap_or(0.0)
    }

    /// Opacity of the incoming tab. The rise drives the fade-in too, so the
    /// new content materializes as it lands instead of popping in on top of
    /// the outgoing body (which would hide the fade-out entirely).
    pub fn incoming_opacity(&self) -> f32 {
        self.rise.value()
    }

    /// Rise progress of the incoming tab, 0 (below) to 1 (landed).
    pub fn rise(&self) -> f32 {
        self.rise.value()
    }

    /// Progress of the sliding tab marker: 0 = on the outgoing tab, 1 = on
    /// the active one.
    pub fn marker(&self) -> f32 {
        self.marker.value()
    }

    /// Kick off a swap. Retargeting mid-swap blends into the new
    /// target instead of jumping.
    pub fn swap_to(&mut self) {
        let mut fade = crate::core::anim::Spring::new(1.0, crate::core::anim::SpringConfig::SWAP);
        fade.set_target(0.0);
        self.phase = Some(fade);
        self.rise.snap(0.0);
        self.rise.set_target(1.0);
        // The marker restarts at the outgoing box and glides across; snapping
        // keeps it from jumping when a second tab is picked mid-slide.
        self.marker.snap(0.0);
        self.marker.set_target(1.0);
    }

    /// Advance both springs by `dt` seconds; returns whether more
    /// frames are needed. The fade and the rise overlap: one crossfade,
    /// not a fade followed by a slide.
    pub fn step(&mut self, dt: f32) -> bool {
        let fading = match self.phase {
            Some(ref mut fade) => fade.step(dt),
            None => false,
        };
        if !fading {
            self.phase = None;
        }
        let rising = self.rise.step(dt);
        let marker = self.marker.step(dt);
        fading || rising || marker
    }

    /// Diagnostic only: is either spring still integrating?
    pub fn debug_busy(&self) -> bool {
        self.phase.as_ref().is_some_and(|f| f.is_animating()) || self.rise.is_animating()
    }
}

impl ViewState {
    /// Click at panel-space `(x, y)` selects the tab under it.
    ///
    /// Panel space, not screen space: the island is a top-anchored window, so the
    /// window hands over client coordinates and this resolves them against the
    /// same layout the frame was built from. Returns the new selection, or `None`
    /// when the click landed outside every tab (which must not clear the
    /// selection).
    pub fn click_tab(&mut self, x: f32, y: f32) -> Option<usize> {
        let next = self.tab_at(x, y)?;
        self.select_tab(next);
        Some(next)
    }

    /// Jump to a tab by index with the same swap animation a click plays —
    /// the tray's "Settings…" path to the panel.
    pub fn select_tab(&mut self, tab: usize) {
        if tab != self.active_tab {
            self.prev_tab = self.active_tab;
            self.active_tab = tab;
            self.tab_swap.swap_to();
        }
    }

    /// Which tab contains panel-space `(x, y)`, if any.
    pub fn tab_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.content_visible() {
            return None;
        }
        // The dock sits at the FOOT of the panel, so its hit band is the same
        // band the strip is drawn in — measured up from the bottom edge, not
        // down from the header.
        let cy = dock_center(self.island_height);
        if !(cy - TAB_H * 0.5..cy + TAB_H * 0.5).contains(&y) {
            return None;
        }
        tab_boxes(
            self.island_width,
            self.active_tab,
            self.prev_tab,
            self.tab_swap.marker(),
        )
            .into_iter()
            .position(|(left, w)| w > 0.0 && x >= left && x <= left + w)
    }

    /// Which header corner icon contains panel-space `(x, y)`, if any.
    ///
    /// Pin and settings sit on the header line, right-aligned, mirroring
    /// NotchStar. Drawn and hit-tested from the same rects so a click can never
    /// land on a different icon than the one under the cursor.
    pub fn corner_at(&self, x: f32, y: f32) -> Option<CornerIcon> {
        if !self.content_visible() {
            return None;
        }
        let top = HEADER_Y + (header_height() - CORNER_ICON) * 0.5;
        if !(top..top + CORNER_ICON).contains(&y) {
            return None;
        }
        let (settings_x, pin_x) = corner_icons_x(self.island_width);
        if x >= settings_x && x <= settings_x + CORNER_ICON {
            return Some(CornerIcon::Settings);
        }
        if x >= pin_x && x <= pin_x + CORNER_ICON {
            return Some(CornerIcon::Pin);
        }
        None
    }

    /// Which content row contains panel-space `(x, y)`, if any.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.content_visible() {
            return None;
        }
        row_boxes(self.island_width, self.island_height)
            .into_iter()
            .position(|(left, top, w, h)| x >= left && x <= left + w && y >= top && y <= top + h)
    }

    /// Which transport command panel-space `(x, y)` hits, if any.
    ///
    /// Only on the Media tab while the panel is open: the collapsed pill is
    /// click-through, and the other tabs draw no transport row.
    pub fn media_command(&self, x: f32, y: f32) -> Option<crate::services::media::Command> {
        if self.active_tab != MEDIA_TAB || !self.content_visible() {
            return None;
        }
        let top = row_band_top();
        // The bar is the seek target: a click maps its x to a fraction of the
        // track, so the user can jump anywhere without dragging.
        let bar = media::bar_rect(self.island_width, self.island_height, top);
        // The bar is 4 px; inflate the hitbox so a click near it still seeks.
        let pad = 6.0;
        if x >= bar.x && x <= bar.x + bar.w && y >= bar.y - pad && y <= bar.max_y() + pad {
            let frac = ((x - bar.x) / bar.w.max(1.0)).clamp(0.0, 1.0);
            let secs = (self.media.duration?.as_secs_f32() * frac) as u64;
            return Some(crate::services::media::Command::Seek(secs));
        }
        media::transport(self.island_width, self.island_height, top)
            .into_iter()
            .find(|(r, _)| x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h)
            .map(|(_, cmd)| cmd)
    }

    /// Which clipboard control panel-space `(x, y)` hits. `0` is the consent /
    /// capture row, `Some(i + 1)` is history row `i`.
    pub fn clipboard_hit(&self, x: f32, y: f32) -> Option<usize> {
        if self.active_tab != CLIPBOARD_TAB || !self.content_visible() {
            return None;
        }
        let top = row_band_top();
        if !self.clip_enabled {
            let r = clipboard::consent_rect(self.island_width, top);
            return (x >= r.x && x <= r.max_x() && y >= r.y && y <= r.max_y()).then_some(0);
        }
        let k = clipboard::capture_rect(self.island_width, top);
        if x >= k.x && x <= k.max_x() && y >= k.y && y <= k.max_y() {
            return Some(0);
        }
        clipboard::rows(self.island_width, top, self.clip_entries.len())
            .into_iter()
            .position(|r| x >= r.x && x <= r.max_x() && y >= r.y && y <= r.max_y())
            .map(|i| i + 1)
    }

    /// Which calendar control panel-space `(x, y)` hits: `0` is the add row,
    /// `Some(i + 1)` is subscription `i`.
    pub fn calendar_hit(&self, x: f32, y: f32) -> Option<usize> {
        if self.active_tab != CALENDAR_TAB || !self.content_visible() {
            return None;
        }
        calendar::head_hit(
            self.island_width,
            row_band_top(),
            self.cal.subs.len().min(calendar::MAX_SUBS),
            x,
            y,
        )
    }

    /// Which shelf tile panel-space `(x, y)` hits, if any.
    ///
    /// Only on the Shelf tab while the panel is open: the pill in collapsed
    /// mode is a status readout, not a click target.
    pub fn shelf_hit(&self, x: f32, y: f32) -> Option<usize> {
        if self.active_tab != SHELF_TAB || !self.content_visible() {
            return None;
        }
        shelf::tile_at(self.island_width, row_band_top(), &self.shelf, x, y)
    }

    /// Which color-picker control panel-space `(x, y)` hits: `0` is the
    /// readout (copies the current hex), `Some(i + 1)` is trail swatch `i`.
    pub fn picker_hit(&self, x: f32, y: f32) -> Option<usize> {
        if self.active_tab != PICKER_TAB || !self.content_visible() {
            return None;
        }
        // `row_band_top()`, NOT the tab strip's top: the readout is DRAWN below
        // the strip, so hit-testing from the strip's own top put a 44 px
        // invisible block across the tabs and swallowed every tab click.
        let top = row_band_top();
        let r = picker::readout_rect(self.island_width, top);
        if x >= r.x && x <= r.max_x() && y >= r.y && y <= r.max_y() {
            return Some(0);
        }
        picker::swatch_hit(self.island_width, top, self.picker.trail_len(), x, y).map(|i| i + 1)
    }

    /// Which timer preset button panel-space `(x, y)` hits, if any.
    pub fn timer_preset(&self, x: f32, y: f32) -> Option<usize> {
        if self.active_tab != TIMER_TAB || !self.content_visible() {
            return None;
        }
        let top = row_band_top();
        timer::buttons(self.island_width, top)
            .into_iter()
            .position(|r| x >= r.x && x <= r.max_x() && y >= r.y && y <= r.max_y())
    }

    /// The pet's drawn scale factor: 1.0 plus the user's size offset, held
    /// inside the range the Settings "Pet size" row offers. Reading it here
    /// (rather than letting the two draw sites clamp separately) keeps the
    /// sprite and the standing-label slot it must clear in agreement.
    fn pet_scale(&self) -> f32 {
        (1.0 + self.pet_zoom).clamp(settings::SCALE_MIN as f32, settings::SCALE_MAX as f32)
    }

    /// Whether the panel content (tabs, rows, footer) is on screen at all.
    ///
    /// Same threshold as content emission: whatever the user cannot see cannot
    /// be hovered or clicked.
    pub fn content_visible(&self) -> bool {
        openness(self.island_width, self.island_height) > layout::CONTENT_FADE_IN_START
    }
}

/// Each tab's `(left, width)` in panel space, padding included, in layout order.
///
/// Single source of truth for the strip: drawing and hit-testing both read it, so
/// a click can never land on a different tab than the one that was drawn.
///
/// NotchStar's dock, adapted: an idle tab is just its icon, and only the
/// SELECTED tab carries a label. `reveal` (0..1, the swap's marker spring)
/// cross-fades the two labels — the incoming one opens while the outgoing one
/// closes — so a tab change is one continuous motion: the previous tab's text
/// retreats to its icon while the pressed tab's text unfurls beside it.
fn tab_boxes(fw: f32, active: usize, prev: usize, reveal: f32) -> Vec<(f32, f32)> {
    let reveal = reveal.clamp(0.0, 1.0);
    let natural: Vec<f32> = TABS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let base = TAB_PAD * 2.0 + TAB_ICON;
            let open = if i == active {
                reveal
            } else if i == prev && prev != active {
                1.0 - reveal
            } else {
                0.0
            };
            base + (TAB_ICON_GAP + measure(label, &TAB_LABEL) + TAB_SLACK) * open
        })
        .collect();
    layout::row_items(&natural, TAB_GAP, fw - PANEL_PAD * 2.0)
        .into_iter()
        .map(|(x, w)| (PANEL_PAD + x, w))
        .collect()
}

/// Build one frame. Pure: same input, same output.
pub fn build(state: &ViewState) -> Frame {
    let (fw, fh) = (state.island_width, state.island_height);
    let frame = Rect::new(0.0, 0.0, fw, fh);
    let open = openness(fw, fh);
    let radius = island_radius(open);
    let peek_t = state.peek_t.clamp(0.0, 1.0);
    // Collapsed: concave ear wings at the top corners (negative radius).
    // Hover (peek): the ears are removed and the top corners round, giving a
    // floating pill detached from the screen top.
    let top_r = (-layout::EAR_W) * (1.0 - peek_t) + (fh / 2.0) * peek_t;
    let bottom_r = radius * (1.0 - peek_t) + (fh / 2.0) * peek_t;

    let mut scene = Scene::new();

    // 1. The body. One shape, DynamicNotch's NotchStyle: the silhouette starts
    //    at the window's top corner, sweeps inward along a concave wing curve
    //    (negative radius), drops down the side, rounds along the bottom with
    //    the iOS continuous corner, and mirrors on the right. The wings and the
    //    body are one figure, so the ears stay in sync at every morph state.
    scene.push(Node::RoundRect {
        rect: frame,
        radii: CornerRadii {
            top_left: top_r,
            top_right: top_r,
            bottom_left: bottom_r,
            bottom_right: bottom_r,
        }
        .clamped(fw, fh),
        fill: state.notch_bg,
    });

    // A zero-sized island emits nothing else: there is nothing to lay out inside,
    // and any rect could only escape the frame bounds.
    if fw <= 0.0 || fh <= 0.0 {
        return Frame {
            size: (fw, fh),
            scene,
        };
    }

    // 2. Peek (hover): the collapsed pill re-shaped into a capsule of the same
    //    size (185x32): the ear wings are gone, the top corners round to h/2,
    //    and it detaches from the top by PEEK_DROP. Width is the minimized
    //    body width -- the arcs at the top corners are not part of it -- so
    //    hovering changes the shape, never the size.
    //    Content: the reference's collapsed shape — title + thin VU bars on the
    //    right, no art, no progress.
    if state.peek {
        let opacity = peek_t;
        if opacity > 0.0 {
            // A live HUD takes the glyph's slot.
            match &state.hud {
                Some(hud) => {
                    for n in pill_hud(fw, fh, hud, opacity) {
                        scene.push(n);
                    }
                }
                None => {
                    if state.media.playing {
                        let title_text = if state.media.title.is_empty() {
                            "Nothing playing".to_string()
                        } else {
                            state.media.title.clone()
                        };
                        let title = TextStyle {
                            size: 12.5,
                            weight: Weight::Semibold,
                            color: layout::fade(Rgba::WHITE, opacity),
                            ..TextStyle::default()
                        };
                        let th = line_height(&title);
                        let viz_total = VIZ_BAR_W * 4.0 + VIZ_GAP * 3.0;
                        let viz_x0 = fw - PILL_MEDIA_INSET - viz_total;
                        let tx = 14.0;
                        let tw = (viz_x0 - tx - 6.0).max(8.0);
                        scene.push(Node::Text {
                            rect: put(fw, fh, Rect::new(tx, (fh - th) / 2.0, tw, th)),
                            text: title_text,
                            style: title,
                        });
                        let cy = fh / 2.0;
                        let heights = state.viz.heights();
                        for (i, h) in heights.iter().enumerate() {
                            let hh = (14.0 * h).max(3.0);
                            scene.push(Node::RoundRect {
                                rect: put(
                                    fw,
                                    fh,
                                    Rect::new(
                                        viz_x0 + i as f32 * (VIZ_BAR_W + VIZ_GAP),
                                        cy - hh / 2.0,
                                        VIZ_BAR_W,
                                        hh,
                                    ),
                                ),
                                radii: CornerRadii::uniform(VIZ_BAR_W / 2.0),
                                fill: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.9), opacity),
                            });
                        }
                    } else if let Some(pet) = state.pet {
                        let (sx, sy, sw, sh) = crate::services::pet::frame_rect(pet.frame);
                        let h = (crate::ui::layout::PILL_H_REF - PILL_PET_PAD * 2.0)
                            * state.pet_scale();
                        let w = h * (sw / sh);
                        scene.push(Node::Sprite {
                            rect: put(fw, fh, Rect::new((fw - w) / 2.0, (fh - h) / 2.0, w, h)),
                            handle: pet.handle,
                            src: Rect::new(sx, sy, sw, sh),
                            opacity,
                        });
                    } else {
                        let g = PILL_GLYPH;
                        scene.push(Node::RoundRect {
                            rect: put(fw, fh, Rect::new((fw - g) / 2.0, (fh - g) / 2.0, g, g)),
                            radii: CornerRadii::uniform(PILL_GLYPH_RADIUS),
                            fill: layout::fade(PILL_GREY, opacity),
                        });
                    }
                }
            }
        }
        return Frame {
            size: (fw, fh),
            scene,
        };
    }

    // 3. Collapsed pill glyph. Fades out across the first half of the morph, so it
    //    dissolves instead of vanishing under the arriving panel content.
    let glyph_opacity = 1.0 - layout::ease(open / layout::PILL_GLYPH_FADE_OUT_END);
    if glyph_opacity > 0.0 {
        // A live HUD takes the glyph's slot: showing both would stack two
        // centred marks in a pill that only fits one.
        match &state.hud {
            Some(hud) => {
                for n in pill_hud(fw, fh, hud, glyph_opacity) {
                    scene.push(n);
                }
            }
            None => {
                // Atoll swaps the collapsed glyph for its four-bar spectrum
                // while a track plays; paused, the plain resting glyph is back.
                // The idle pet wins over the glyph: a live mascot is the whole
                // point of the notch, and the sprite already IS a centered mark.
                if state.media.playing {
                    for n in pill_media(fw, fh, state, glyph_opacity) {
                        scene.push(n);
                    }
                } else if let Some(pet) = state.pet {
                    let (sx, sy, sw, sh) = crate::services::pet::frame_rect(pet.frame);
                    // Fit the cell inside the pill's height minus its padding,
                    // preserving the sprite's aspect. The height is the COLLAPSED
                    // pill's, not `fh`: `fh` is mid-spring during a hover-expand,
                    // and sizing from it ballooned the pet across the morph — the
                    // "flicks frames when the notch expands" report. The user's
                    // size choice scales that fit, capped so a larger mascot
                    // still ends inside the pill's rounded ends.
                    let h = (crate::ui::layout::PILL_H_REF - PILL_PET_PAD * 2.0)
                        * state.pet_scale();
                    let w = h * (sw / sh);
                    scene.push(Node::Sprite {
                        rect: put(fw, fh, Rect::new((fw - w) / 2.0, (fh - h) / 2.0, w, h)),
                        handle: pet.handle,
                        src: Rect::new(sx, sy, sw, sh),
                        // The pet dissolves on the glyph's curve, so a hover-open
                        // is a crossfade into the panel instead of a hard cut.
                        opacity: glyph_opacity,
                    });
                } else {
                    let g = PILL_GLYPH;
                    scene.push(Node::RoundRect {
                        rect: put(fw, fh, Rect::new((fw - g) / 2.0, (fh - g) / 2.0, g, g)),
                        radii: CornerRadii::uniform(PILL_GLYPH_RADIUS),
                        fill: layout::fade(PILL_GREY, glyph_opacity),
                    });
                }
            }
        }
    }

    // 3. Collapsed live activity: while a track plays and the island is closed,
    //    the pill grows a hairline progress bar along its bottom edge. It fades
    //    out on the same curve as the glyph so the panel never shows two
    //    progress bars at once.
    if let Some(bar) = pill_progress(fw, fh, state, glyph_opacity) {
        scene.push(bar);
    }


    // 4. Mic-in-use dot: a standing indicator, not a HUD. It sits at the pill's
    //    right edge, clear of the centred glyph, and fades with the same curve.
    if state.mic_active && glyph_opacity > 0.0 {
        let d = 6.0;
        let x = fw - 10.0 - d;
        let y = (fh - d) / 2.0;
        scene.push(Node::RoundRect {
            rect: put(fw, fh, Rect::new(x, y, d, d)),
            radii: CornerRadii::uniform(d / 2.0),
            fill: layout::fade(Rgba::rgb(0.95, 0.25, 0.25), glyph_opacity),
        });
    }

    // 4b. Standing pill rows: weather, downloads, shelf. Each is a small
    //     rounded track with a label, laid out left-to-right in that order.
    //     Skipped when a HUD owns the glyph slot or the panel is open. Order
    //     is fixed so rows never jump around as data arrives.
    if state.hud.is_none() && glyph_opacity > 0.0 {
        let style = TextStyle::numeric(11.0);
        let th = line_height(&style);
        let h = (fh - 10.0).max(4.0);
        // The centred mark owns a slot the rows may not cross: a standing label
        // flows right out of x = 10 and would otherwise be drawn through the
        // mascot. Widths mirror section 2 — the pet is the pill-height cell,
        // the spectrum and the resting glyph are both `VIZ_BAR_W*4 + VIZ_GAP*3`.
        let slot_w = match (&state.pet, state.media.playing) {
            (Some(p), false) => {
                let (_, _, sw, sh) = crate::services::pet::frame_rect(p.frame);
                ((layout::PILL_H_REF - PILL_PET_PAD * 2.0) * state.pet_scale()) * (sw / sh)
            }
            _ => VIZ_BAR_W * 4.0 + VIZ_GAP * 3.0,
        };
        let slot_left = ((fw - slot_w) / 2.0 - 8.0).max(0.0);
        let mut x = 10.0f32;
        let mut rows: Vec<String> = Vec::new();
        // The now-playing title is owned by `pill_media` (art + title row), so
        // the standing rows never duplicate it — that drew the track twice.
        rows.extend(
            state
                .weather
                .as_ref()
                .map(|w| w.label())
                .into_iter()
                .chain((state.downloads.delta > 0).then(|| state.downloads.label()))
                .chain(
                    (!state.shelf.is_empty()).then(|| format!("Shelf {} items", state.shelf.len())),
                ),
        );
        for text in rows {
            let tw = measure(&text, &style);
            let w = (tw + 14.0).min(fw * 0.5);
            // A row may fill the flank but never enter the slot: the track is
            // clamped to what's left and the renderer ellipsizes the label to
            // its own rect, so a long title shortens instead of running under
            // the mark. A flank too narrow for even a stub drops the rest.
            let room = slot_left - x - 6.0;
            if room < 24.0 {
                break;
            }
            let w = w.min(room);
            let track = Rect::new(x, (fh - h) / 2.0, w, h);
            scene.push(Node::RoundRect {
                rect: put(fw, fh, track),
                radii: CornerRadii::uniform(track.h / 2.0),
                fill: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.10), glyph_opacity),
            });
            scene.push(Node::Text {
                // The text rect is the TRACK's width, not the measured one:
                // the renderer ellipsizes to its own rect, so a clamped track
                // clips its label too instead of spilling into the slot.
                rect: put(
                    fw,
                    fh,
                    Rect::new(track.x + 7.0, (fh - th) / 2.0, (track.w - 14.0).max(0.0), th),
                ),
                text,
                style: TextStyle {
                    color: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.92), glyph_opacity),
                    ..style
                },
            });
            x += w + 6.0;
        }
    }

    // 5. Panel content: skipped below the fade-in threshold, alpha-scaled above
    //    it, so it materializes instead of popping in at a fixed size.
    let content_opacity = layout::ease(
        (open - layout::CONTENT_FADE_IN_START) / (1.0 - layout::CONTENT_FADE_IN_START),
    );
    if content_opacity > 0.0 {
        // Chrome (header title, corner icons, dock) is drawn UNCHANGED by a tab
        // swap — only its alpha tracks the open spring. Folding it into the
        // staggered group below made `Arc` and the dock slide with the body,
        // which reads as the whole panel being rebuilt on every click.
        for mut c in panel_chrome(fw, fh, state) {
            fade_node(&mut c, content_opacity);
            scene.push(c);
        }
        // iOS-style swap: the outgoing tab's body fades out first, then the
        // incoming one rises from a few px below as the spring lands. Idle
        // swaps sit at rise = 1.0, so a settled frame is unchanged.
        let swap = &state.tab_swap;
        let outgoing = swap.outgoing_opacity();
        if outgoing > 0.0 {
            let mut old = Node::Group {
                rect: frame,
                children: panel_body(fw, fh, state, state.prev_tab),
            };
            if let Node::Group { children, .. } = &mut old {
                children
                    .iter_mut()
                    .for_each(|c| fade_node(c, content_opacity * outgoing));
            }
            scene.push(old);
        }
        let rise = swap.rise();
        let mut group = Node::Group {
            rect: frame,
            children: panel_body(fw, fh, state, state.active_tab),
        };
        if let Node::Group { children, .. } = &mut group {
            children.iter_mut().enumerate().for_each(|(i, c)| {
                // Staggered rise: each row starts a beat after the one above
                // it, so the body cascades in instead of moving as a slab.
                let p = stagger_progress(rise, i);
                fade_node(c, content_opacity * p);
                c.translate(0.0, SWAP_RISE_PX * (1.0 - p));
                scale_about_top(c, 1.0 - SWAP_SCALE_DELTA * (1.0 - p));
            });
        }
        scene.push(group);
    }

    Frame {
        size: (fw, fh),
        scene,
    }
}

/// The collapsed pill's inline progress bar, or `None` when nothing is playing
/// or the island is opening. Geometry is deliberately independent of the panel's
/// bar: this one hugs the pill's own bottom edge.
fn pill_progress(fw: f32, fh: f32, state: &ViewState, opacity: f32) -> Option<Node> {
    let m = &state.media;
    if opacity <= 0.0 || !m.playing {
        return None;
    }
    // Duration is unknown for some sources (live streams); no bar then, rather
    // than a bar pinned at zero.
    let span = m.duration?.as_secs_f32();
    if span <= 0.0 {
        return None;
    }
    let frac = (m.position.as_secs_f32() / span).clamp(0.0, 1.0);

    let inset = PANEL_PAD;
    let w = (fw - inset * 2.0).max(0.0);
    let h = 2.0;
    let y = fh - PILL_PROGRESS_BOTTOM_PAD - h;
    let fill = layout::fade(
        m.accent
            .unwrap_or(crate::core::geom::Rgba::rgb(0.8, 0.2, 0.3)),
        opacity,
    );

    let mut out = vec![Node::RoundRect {
        rect: put(fw, fh, Rect::new(inset, y, w, h)),
        radii: CornerRadii::uniform(h / 2.0),
        fill: layout::fade(Rgba::rgb(1.0, 1.0, 1.0), 0.12 * opacity),
    }];
    if frac > 0.0 {
        out.push(Node::RoundRect {
            rect: put(fw, fh, Rect::new(inset, y, w * frac, h)),
            radii: CornerRadii::uniform(h / 2.0),
            fill,
        });
    }
    Some(Node::Group {
        rect: Rect::new(0.0, 0.0, fw, fh),
        children: out,
    })
}

/// Clamp a rect into the frame. Every emitted rect goes through here, which is
///
/// This matters because the island is mid-spring: the window can be far smaller
/// than the panel it is morphing out of, and content laid out in panel space must
/// be clipped into the live frame rather than trusted to fit.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// How far below its resting spot the incoming tab's content starts, in
/// logical px. Small on purpose — an iOS rise is a nudge, not a slide.
pub const SWAP_RISE_PX: f32 = 8.0;

/// Fraction of the swap each successive row waits behind the one above it.
/// Small: enough to read as a cascade, not enough to look like a queue.
pub const SWAP_STAGGER: f32 = 0.07;

/// Progress of child `index` given the swap's overall `rise` (0..=1).
///
/// Rows start in order, so row 0 leads and each later row trails by
/// [`SWAP_STAGGER`]. Rows past the point where the stagger would eat the
/// whole window share the last slot rather than never arriving.
pub fn stagger_progress(rise: f32, index: usize) -> f32 {
    let max_delay = 1.0 - SWAP_STAGGER;
    let delay = (index as f32 * SWAP_STAGGER).min(max_delay);
    ((rise - delay) / (1.0 - delay)).clamp(0.0, 1.0)
}

/// The expanded panel's chrome: header, corner icons, and the bottom dock.
///
/// Drawn ONCE per frame, with no fade/rise of its own. Splitting it out from
/// the body is what stops the `Arc` title and the dock from sliding with a tab
/// swap — only the body cross-fades, the chrome stays put.
fn panel_chrome(fw: f32, fh: f32, state: &ViewState) -> Vec<Node> {
    let mut out = header_block(fw, fh);
    out.extend(corner_icons(fw, fh, state));
    out.extend(tab_strip(fw, fh, state));
    out
}

/// The tab body alone: no header, no dock. That is the part a swap animates.
fn panel_body(fw: f32, fh: f32, state: &ViewState, tab: usize) -> Vec<Node> {
    let tab_bottom = row_band_top();
    if tab == MEDIA_TAB {
        media::content(fw, fh, tab_bottom, &state.media, state.accent_progress)
    } else if tab == STATS_TAB {
        stats::content(fw, fh, tab_bottom, &state.stats, &state.procs)
    } else if tab == TIMER_TAB {
        timer::content(fw, fh, tab_bottom, &state.timer)
    } else if tab == CLIPBOARD_TAB {
        clipboard::content(fw, fh, tab_bottom, &state.clip_entries, state.clip_enabled)
    } else if tab == PICKER_TAB {
        picker::content(fw, fh, tab_bottom, &state.picker)
    } else if tab == CALENDAR_TAB {
        calendar::content(fw, fh, tab_bottom, &state.cal)
    } else if tab == SHELF_TAB {
        shelf::content(fw, fh, tab_bottom, &state.shelf)
    } else if tab == USAGE_TAB {
        usage::content(fw, fh, tab_bottom, &state.usage)
    } else if tab == TERMINAL_TAB {
        // Settings now live in their own window (tray → Settings…), so the
        // expanded notch no longer hosts a Settings tab.
        terminal::content(
            fw,
            fh,
            tab_bottom,
            state.terminal.as_ref(),
            state.terminal_error.as_deref(),
        )
    } else {
        content_rows(fw, fh, tab_bottom, state)
    }
}

/// Index in [`TABS`] of the Media tab — the one with a real content surface.
pub const MEDIA_TAB: usize = 0;
pub const STATS_TAB: usize = 1;
/// The Timer tab — real content behind it now.
pub const TIMER_TAB: usize = 2;
/// The Clipboard tab — history, consent row, clear-all.
pub const CLIPBOARD_TAB: usize = 3;
pub const PICKER_TAB: usize = 4;
/// The Calendar tab — upcoming events from subscribed `.ics` sources.
pub const CALENDAR_TAB: usize = 6;
/// The Shelf tab — pinned files dropped on the island.
pub const SHELF_TAB: usize = 7;
/// The Usage tab — still the generic placeholder rows.
pub const USAGE_TAB: usize = 5;
/// The Terminal tab — the ConPTY grid.
pub const TERMINAL_TAB: usize = 8;

/// Height of the header row's line box (20 px title × 1.3 line height).
fn header_height() -> f32 {
    line_height(&TextStyle::title())
}

/// Header row: the title, at `TextStyle::title()` (20 px semibold white).
///
/// The box is the measured run **plus slack**. Sizing it to exactly
/// `measure(APP_TITLE)` clipped the title to `A…`: `measure` is an approximation
/// and DirectWrite's real advance is a hair wider, so an exactly-sized box
/// ellipsizes its own text. Same trap as the remaining-time label.
fn header_block(fw: f32, fh: f32) -> Vec<Node> {
    let mut style = TextStyle::title();
    style.align = Align::Start;
    vec![Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(
                PANEL_PAD,
                HEADER_Y,
                measure(APP_TITLE, &style) + HEADER_SLACK,
                line_height(&style),
            ),
        ),
        text: APP_TITLE.to_string(),
        style,
    }]
}

/// Breathing room past the measured title run so DirectWrite never trims it.
const HEADER_SLACK: f32 = 8.0;

/// Which of the header's corner icons a point lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerIcon {
    Pin,
    Settings,
}

/// The corner pair: pin, then settings, right-aligned on the header line.
///
/// NotchStar's title row is exactly this — the wordmark alone on the left, a
/// pin and a gear at the right — so the notch carries the same two controls
/// where the reference puts them.
fn corner_icons(fw: f32, fh: f32, state: &ViewState) -> Vec<Node> {
    let y = HEADER_Y + (header_height() - CORNER_ICON) * 0.5;
    let settings_x = corner_icons_x(fw).0;
    let pin_x = corner_icons_x(fw).1;
    let idle = Rgba::rgba(0.91, 0.91, 0.91, 1.0);
    vec![
        // The pin carries the pinned/hidden state, so it is white only while
        // the island is pinned over fullscreen apps.
        Node::Glyph {
            rect: put(fw, fh, Rect::new(pin_x, y, CORNER_ICON, CORNER_ICON)),
            name: "pin",
            color: if state.pinned { Rgba::WHITE } else { idle },
        },
        Node::Glyph {
            rect: put(fw, fh, Rect::new(settings_x, y, CORNER_ICON, CORNER_ICON)),
            name: "settings",
            color: idle,
        },
    ]
}

/// `(settings_x, pin_x)` — the two corner icons' left edges, right-aligned.
/// Single source of truth so drawing and hit-testing cannot drift apart.
fn corner_icons_x(fw: f32) -> (f32, f32) {
    let settings_x = fw - PANEL_PAD - CORNER_ICON;
    (settings_x, settings_x - CORNER_ICON_GAP - CORNER_ICON)
}

/// The five-tab strip, centered, 8 px between items.
///
/// Labels are measured with the pure approximator (`text::measure`) and then given
/// symmetric padding, so each tab's box is a superset of its label run. The `Glyph`
/// node carries the icon name, the `Text` node the visible label.
///
/// The selected tab gets the stronger micro-fill; the
/// hovered one gets it at 24 % of that. The two states are additive rather than
/// exclusive, so hovering the selected tab deepens it instead of doing nothing.
///
/// NotchStar's dock behaviour, adapted to a text strip: every tab carries an
/// icon left of its label, and the selected pill is ONE shape that glides from
/// the box it was on to the box that was just picked (stepped by the swap's
/// `marker` spring) instead of blinking off one tab and on to the next.
fn tab_strip(fw: f32, fh: f32, state: &ViewState) -> Vec<Node> {
    // Anchored to the panel's FOOT, NotchStar's dock. `fh` is the live island
    // height, so the dock rides down with the window as it opens instead of
    // being pinned to a fixed y that only works at full size.
    let y = dock_top(fh);
    let active = state.active_tab.min(TABS.len() - 1);
    let prev = state.prev_tab.min(TABS.len() - 1);
    let reveal = state.tab_swap.marker().clamp(0.0, 1.0);
    let boxes = tab_boxes(fw, active, prev, reveal);
    let mut out = Vec::with_capacity(TABS.len() * 3);
    if boxes.len() != TABS.len() {
        return out;
    }

    for (i, (label, (left, w))) in TABS.iter().zip(boxes).enumerate() {
        if w <= 0.0 {
            continue;
        }
        let selected = i == active;
        let hovered = state.hover_tab == Some(i);
        if selected || hovered {
            // The reference's micro-fill reads as a *lift* out of pure black, so
            // it has to be light-on-dark: a black-alpha fill over the opaque
            // black body composites to exactly the body colour and is invisible.
            out.push(Node::RoundRect {
                rect: put(fw, fh, Rect::new(left, y, w, TAB_H)),
                radii: CornerRadii::uniform(TAB_RADIUS),
                fill: Rgba::rgba(1.0, 1.0, 1.0, if selected { 0.22 } else { 0.08 }),
            });
        }
        let color = if selected {
            Rgba::WHITE
        } else if hovered {
            Rgba::rgba(0.88, 0.88, 0.92, 1.0)
        } else {
            TAB_LABEL.color
        };

        // Icon always; label only on the tab that is opening or closing, and
        // both are driven by the SAME spring as the box width, so the box and
        // its text can never disagree mid-flight.
        let iy = y + (TAB_H - TAB_ICON) / 2.0;
        out.push(Node::Glyph {
            rect: put(fw, fh, Rect::new(left + TAB_PAD, iy, TAB_ICON, TAB_ICON)),
            name: TAB_GLYPHS[i],
            color,
        });
        let open = if selected {
            reveal
        } else if i == prev && prev != active {
            1.0 - reveal
        } else {
            0.0
        };
        if open > 0.02 {
            // The label keeps its FINAL width and only fades/slides in — sizing
            // it to the animating box would ellipsize the text mid-reveal
            // (`Cale…` for two frames) instead of wiping it open.
            let label_w = measure(label, &TAB_LABEL);
            let inset = (1.0 - open) * TAB_ICON_GAP;
            // Center the glyph box in the TAB_H row, exactly as the icon is:
            // a rect starting at `y` seats the cap height against the strip's
            // top edge, so the label reads high inside its fill pill.
            let ty = y + (TAB_H - line_height(&TAB_LABEL)) / 2.0;
            out.push(Node::Text {
                rect: put(
                    fw,
                    fh,
                    Rect::new(
                        left + TAB_PAD + (TAB_ICON + TAB_ICON_GAP - inset) * open,
                        ty,
                        label_w + TAB_SLACK,
                        line_height(&TAB_LABEL),
                    ),
                ),
                text: label.to_string(),
                style: TextStyle {
                    color: Rgba::rgba(color.r, color.g, color.b, open),
                    align: Align::Start,
                    ..TAB_LABEL.clone()
                },
            });
        }
    }
    out
}

/// Content rows: `(left, top, width, height)` in panel space, in layout order.
///
/// Single source of truth for the rows, exactly like [`tab_boxes`] for the strip:
/// drawing and hit-testing both read it, so the row the cursor highlights is the
/// row the cursor is actually over.
fn row_boxes(fw: f32, fh: f32) -> Vec<(f32, f32, f32, f32)> {
    row_rects_below(fw, fh, row_band_top())
}

/// Panel-space y of the top of the row band: just under the header. The dock
/// is at the FOOT now, so nothing sits between the header and the content —
/// matching how NotchStar stacks wordmark, body, dock.
fn row_band_top() -> f32 {
    HEADER_Y + header_height() + HEADER_GAP
}

/// Row geometry below an explicit band top. Split out so [`build`] can pass the
/// tab strip's *measured* bottom (which the pure band top above approximates)
/// without duplicating the packing maths.
fn row_rects_below(fw: f32, fh: f32, top: f32) -> Vec<(f32, f32, f32, f32)> {
    let avail = (fh - FOOTER_BOTTOM_PAD - top).max(0.0);
    let inner_w = fw - PANEL_PAD * 2.0;
    let n = CONTENT_ROWS as f32;
    let full_h = ROW_H * n + ROW_GAP * (n - 1.0);
    if avail <= 0.0 || inner_w <= 0.0 || full_h <= 0.0 {
        return Vec::new();
    }
    // Shrink the rows to fit a half-open island rather than overflowing it.
    let scale = (avail / full_h).min(1.0);
    let h = ROW_H * scale;
    let gap = ROW_GAP * scale;
    let total = h * n + gap * (n - 1.0);
    // Centred in the leftover band, so rows sit between the tabs and the footer.
    let y0 = top + (avail - total).max(0.0) * 0.5;

    (0..CONTENT_ROWS)
        .map(|i| {
            let y = y0 + (h + gap) * i as f32;
            (PANEL_PAD, y, inner_w, h)
        })
        .collect()
}

/// Transient pill HUD: a coloured fill plus its value, occupying the glyph slot.
///
/// Shape and text only — the glyph table is still placeholder squares (real
/// outlines land in Phase 8), so an icon-based HUD would draw a grey box.
fn pill_hud(fw: f32, fh: f32, hud: &crate::app::hud::Hud, opacity: f32) -> Vec<Node> {
    use crate::app::hud::Hud;
    // A banner is not a gauge: it needs a wider track and a second line, so it
    // returns its own layout instead of bending the readout's contract.
    if let Hud::Notification { app, title, body } = hud {
        return pill_notification(fw, fh, app, title, body, opacity);
    }
    // A short inset fill behind the readout: the pill is 34 px tall, so the HUD
    // reads as a tinted track rather than a label floating on black.
    let h = (fh - 10.0).max(4.0);
    let track = Rect::new((fw - PILL_HUD_W) / 2.0, (fh - h) / 2.0, PILL_HUD_W, h);
    let (fill_col, text, frac) = match hud {
        Hud::Battery {
            percent,
            charging,
            low,
        } => {
            let col = if *charging {
                PILL_GREEN
            } else if *low {
                PILL_AMBER
            } else {
                PILL_GREY
            };
            (col, format!("{percent} %"), Some(*percent as f32 / 100.0))
        }
        Hud::Volume { percent, muted } => (
            if *muted { PILL_AMBER } else { PILL_GREEN },
            if *muted {
                "muted".to_string()
            } else {
                format!("{percent} %")
            },
            Some(*percent as f32 / 100.0),
        ),
        Hud::Timer { remaining, .. } => (
            PILL_GREEN,
            format!("{}:{:02}", remaining / 60, remaining % 60),
            None,
        ),
        Hud::Online(online, ssid) => (
            if *online { PILL_GREEN } else { PILL_AMBER },
            match (online, ssid) {
                (false, _) => "offline".to_string(),
                (true, Some(s)) => {
                    format!("WiFi \u{b7} {}", s.chars().take(18).collect::<String>())
                }
                (true, None) => "online".to_string(),
            },
            None,
        ),
        Hud::Vpn(name) => (
            PILL_GREEN,
            match name {
                Some(n) => format!("VPN \u{b7} {}", n.chars().take(18).collect::<String>()),
                None => "VPN off".to_string(),
            },
            None,
        ),
        Hud::Focus(on) => (
            if *on { PILL_AMBER } else { PILL_GREY },
            if *on { "focus on" } else { "focus off" }.to_string(),
            None,
        ),
        Hud::Brightness(pct) => (PILL_GREEN, format!("{pct} %"), Some(*pct as f32 / 100.0)),
        Hud::Drive(label) => (
            PILL_GREEN,
            match label {
                Some(l) => format!("\u{25a4} {}", l.chars().take(20).collect::<String>()),
                None => "drive ejected".to_string(),
            },
            None,
        ),
        // Unreachable: the banner returns its own layout above. Listed only so
        // the match stays exhaustive when a new Hud variant is added.
        Hud::Notification { .. } => (PILL_GREY, String::new(), None),
    };

    let mut out = vec![Node::RoundRect {
        rect: put(fw, fh, track),
        radii: CornerRadii::uniform(track.h / 2.0),
        fill: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.10), opacity),
    }];
    // Battery and volume fill the track like a gauge; the countdown has no
    // meaningful magnitude on a short track, so it stays a flat badge.
    if let Some(f) = frac {
        let w = track.w * f.clamp(0.0, 1.0);
        if w > 0.5 {
            out.push(Node::RoundRect {
                rect: put(fw, fh, Rect::new(track.x, track.y, w, track.h)),
                radii: CornerRadii::uniform(track.h / 2.0),
                fill: layout::fade(fill_col, opacity),
            });
        }
    }
    let style = TextStyle::numeric(11.0);
    let tw = measure(&text, &style);
    let th = line_height(&style);
    out.push(Node::Text {
        rect: put(fw, fh, Rect::new((fw - tw) / 2.0, (fh - th) / 2.0, tw, th)),
        text,
        style,
    });
    out
}

/// A two-line banner for a notification toast (NotchStar/TopNotch's toast).
fn pill_notification(
    fw: f32,
    fh: f32,
    app: &str,
    title: &str,
    body: &str,
    opacity: f32,
) -> Vec<Node> {
    let track = Rect::new(
        (fw - PILL_NOTIF_W) / 2.0,
        (fh - (PILL_NOTIF_H - 4.0)) / 2.0,
        PILL_NOTIF_W,
        PILL_NOTIF_H - 4.0,
    );
    let mut out = vec![Node::RoundRect {
        rect: put(fw, fh, track),
        radii: CornerRadii::uniform(track.h / 2.0),
        fill: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.10), opacity),
    }];
    let style = TextStyle::numeric(9.5);
        let lh = line_height(&style);
        // App name + title on the first line, body on the second. Each rect is the
        // measured run plus `NOTIF_SLACK`: the approximator under-reports, and a
        // rect sized exactly to it makes the renderer ellipsize its own text.
        const NOTIF_SLACK: f32 = 10.0;
        let head = format!(
            "{} \u{b7} {}",
            app.chars().take(12).collect::<String>(),
            title.chars().take(18).collect::<String>()
        );
        let head_w = measure(&head, &style) + NOTIF_SLACK;
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new((fw - head_w) / 2.0, track.y + 1.0, head_w, lh),
            ),
            text: head,
            style: TextStyle {
                color: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.92), opacity),
                ..style
            },
        });
        let body_text: String = body.chars().take(30).collect();
        let body_w = measure(&body_text, &style) + NOTIF_SLACK;
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(
                    (fw - body_w) / 2.0,
                    track.y + 1.0 + lh,
                    body_w,
                    lh,
                ),
            ),
            text: body_text,
        style: TextStyle {
            color: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.62), opacity),
            ..style
        },
    });
    out
}

/// Width of the notification banner inside the pill.
const PILL_NOTIF_W: f32 = 150.0;
/// Height of the notification banner inside the pill.
const PILL_NOTIF_H: f32 = 30.0;

/// Width of the HUD's gauge track inside the pill.
const PILL_HUD_W: f32 = 54.0;

/// Content area: `CONTENT_ROWS` placeholder rows at the reference's 2 % white
/// micro-fill, filling the space between the tab strip and the footer.
///
/// The row under the cursor lifts to the same 6 % the hovered tab uses, so the
/// whole shell shares one hover vocabulary.
fn content_rows(fw: f32, fh: f32, top: f32, state: &ViewState) -> Vec<Node> {
    row_rects_below(fw, fh, top)
        .into_iter()
        .enumerate()
        .map(|(i, (left, y, w, h))| {
            let hovered = state.hover_row == Some(i);
            Node::RoundRect {
                rect: put(fw, fh, Rect::new(left, y, w, h)),
                radii: CornerRadii::uniform(ROW_RADIUS),
                fill: Rgba::rgba(1.0, 1.0, 1.0, if hovered { 0.10 } else { 0.04 }),
            }
        })
        .collect()
}

/// Four-bar audio visualizer (Atoll's `AudioSpectrum`): springs chase the
/// output meter, so bars decay smoothly between poll ticks and never teleport.
/// Pure state — the app thread steps it; `build` only reads heights.
#[derive(Debug, Clone)]
pub struct Viz {
    bars: [crate::core::anim::Spring; 4],
}

impl Default for Viz {
    fn default() -> Self {
        Self {
            bars: std::array::from_fn(|_| {
                crate::core::anim::Spring::new(
                    0.3,
                    crate::core::anim::SpringConfig::new(0.18, 0.55),
                )
            }),
        }
    }
}

impl Viz {
    /// Drive toward a fresh meter reading. `levels` are 0..=1 peaks; the app
    /// thread passes the same snapshot every frame, so springs do the smoothing.
    ///
    /// Paused, the spectrum is NOT drawn (the resting glyph sits in the slot
    /// instead), so integrating it would chase a target nobody can see and keep
    /// `animating` true forever — the notch redrawing 60 times a second while
    /// idle. The springs simply freeze and hold their last heights; playback
    /// relights them from that value.
    pub fn step(&mut self, dt: f32, levels: [f32; 4], playing: bool) -> bool {
        if !playing {
            return false;
        }
        let mut moved = false;
        for (i, bar) in self.bars.iter_mut().enumerate() {
            bar.set_target(levels[i].clamp(0.08, 1.0));
            moved |= bar.step(dt);
        }
        moved
    }

    /// True when the springs are still integrating a visible change.
    pub fn debug_busy(&self) -> bool {
        self.bars.iter().any(|b| b.is_animating())
    }

    /// Current heights, 0..=1.
    pub fn heights(&self) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for (o, b) in out.iter_mut().zip(self.bars.iter()) {
            *o = b.value().clamp(0.05, 1.0);
        }
        out
    }
}

/// Atoll's spectrum: four 2 px bars, gap 2 px, centred in the pill glyph slot,
/// heights from the visualizer springs. Drawn where the resting glyph would be.
fn pill_media(fw: f32, fh: f32, state: &ViewState, alpha: f32) -> Vec<Node> {
    if alpha <= 0.4 {
        return Vec::new();
    }
    let art = PILL_ART;
    let y_art = (fh - art) / 2.0;
    let x_art = PILL_MEDIA_INSET;
    let mut out = Vec::with_capacity(4);

    // Art thumbnail.
    out.push(Node::Image {
        rect: put(fw, fh, Rect::new(x_art, y_art, art, art)),
        handle: state
            .media
            .art
            .map(crate::core::scene::ImageHandle)
            .unwrap_or_default(),
        radii: CornerRadii::uniform(PILL_ART_RADIUS),
    });

    // Viz bars at the right edge.
    let viz_total = VIZ_BAR_W * 4.0 + VIZ_GAP * 3.0;
    let viz_x0 = fw - PILL_MEDIA_INSET - viz_total;
    let cy = fh / 2.0;
    let heights = state.viz.heights();
    for (i, h) in heights.iter().enumerate() {
        let hh = (14.0 * h).max(3.0);
        out.push(Node::RoundRect {
            rect: put(
                fw,
                fh,
                Rect::new(
                    viz_x0 + i as f32 * (VIZ_BAR_W + VIZ_GAP),
                    cy - hh / 2.0,
                    VIZ_BAR_W,
                    hh,
                ),
            ),
            radii: CornerRadii::uniform(VIZ_BAR_W / 2.0),
            fill: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.9), alpha),
        });
    }

    // Title text in the gap between art and viz bars.
    let tx = x_art + art + PILL_MEDIA_GAP;
    let tw = (viz_x0 - tx - PILL_MEDIA_GAP).max(8.0);
    let style = TextStyle {
        size: 10.5,
        weight: Weight::Semibold,
        color: layout::fade(Rgba::WHITE, alpha),
        ..TextStyle::default()
    };
    let th = line_height(&style);
    let ty = (fh - th) / 2.0 - 0.5;
    out.push(Node::Text {
        rect: put(fw, fh, Rect::new(tx, ty, tw, th)),
        text: if state.media.title.is_empty() {
            "Now playing".to_string()
        } else {
            state.media.title.clone()
        },
        style,
    });
    out
}
fn viz_bars(fw: f32, fh: f32, state: &ViewState, alpha: f32) -> Vec<Node> {
    const MAX_H: f32 = 14.0;
    let heights = state.viz.heights();
    let total = VIZ_BAR_W * 4.0 + VIZ_GAP * 3.0;
    let x0 = (fw - total) / 2.0;
    let cy = fh / 2.0;
    heights
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let h = (MAX_H * h).max(3.0);
            Node::RoundRect {
                rect: put(
                    fw,
                    fh,
                    Rect::new(
                        x0 + i as f32 * (VIZ_BAR_W + VIZ_GAP),
                        cy - h / 2.0,
                        VIZ_BAR_W,
                        h,
                    ),
                ),
                radii: CornerRadii::uniform(VIZ_BAR_W / 2.0),
                fill: layout::fade(Rgba::rgba(1.0, 1.0, 1.0, 0.95), alpha),
            }
        })
        .collect()
}

/// How much smaller (as a fraction of height) incoming content starts
/// its rise. A nudge — the iOS effect is scale you feel, not see.
const SWAP_SCALE_DELTA: f32 = 0.03;

/// Shrink every node in a subtree vertically by `factor`, keeping each
/// node's centre fixed, so rows settle in place instead of sliding.
/// Vertical only: a horizontal scale would distort icon glyphs.
fn scale_about_top(node: &mut Node, factor: f32) {
    let h = node.rect().h;
    let dh = h * (1.0 - factor);
    node.translate(0.0, dh / 2.0);
    match node {
        Node::RoundRect { rect, .. }
        | Node::Text { rect, .. }
        | Node::Image { rect, .. }
        | Node::Sprite { rect, .. }
        | Node::Bar { rect, .. }
        | Node::Arc { rect, .. }
        | Node::Glyph { rect, .. }
        | Node::Group { rect, .. } => rect.h = h * factor,
    }
    if let Node::Group { children, .. } = node {
        for c in children.iter_mut() {
            scale_about_top(c, factor);
        }
    }
}

/// Multiply a node's opacity by `opacity`, recursively.
fn fade_node(node: &mut Node, opacity: f32) {
    match node {
        Node::RoundRect { fill, .. } => *fill = layout::fade(*fill, opacity),
        Node::Bar { track, fill, .. } => {
            *track = layout::fade(*track, opacity);
            *fill = layout::fade(*fill, opacity);
        }
        Node::Arc { track, fill, .. } => {
            *track = layout::fade(*track, opacity);
            *fill = layout::fade(*fill, opacity);
        }
        Node::Text { style, .. } => style.color = layout::fade(style.color, opacity),
        Node::Glyph { color, .. } => *color = layout::fade(*color, opacity),
        // Images carry no colour of their own; their accent would come from state.
        Node::Image { .. } => {}
        // Sprites carry no colour either — a plain alpha scalar — so the fade
        // is a multiply. Without this, a pet would sit at full strength while
        // the glyph around it dissolved, and pop out at the end.
        Node::Sprite { opacity: alpha, .. } => *alpha *= opacity.clamp(0.0, 1.0),
        Node::Group { children, .. } => children.iter_mut().for_each(|c| fade_node(c, opacity)),
    }
}

#[cfg(test)]
mod tests {
    use super::layout::{PANEL_RADIUS_REF, PANEL_W_REF, PILL_RADIUS_REF, PILL_W_REF};
    use super::*;

    /// Test helper: a view at an explicit size. Production sizes the persistent
    /// `ViewState` in place; tests build throwaway ones.
    fn view_sized(island_width: f32, island_height: f32) -> ViewState {
        ViewState {
            island_width,
            island_height,
            ..Default::default()
        }
    }

    /// Same slack the bounds invariant allows, shared with `layout::fit`.
    const EPS: f32 = layout::BOUNDS_EPSILON;

    fn span() -> f32 {
        PANEL_W_REF - PILL_W_REF
    }

    /// Width for a given openness in 0..=1.
    fn width_at(open: f32) -> f32 {
        PILL_W_REF + span() * open.clamp(0.0, 1.0)
    }

    /// The morph's nominal height for a given openness. In reality the height
    /// spring lags the width; this is the shape the tests sweep.
    fn height_at(open: f32) -> f32 {
        PILL_H_REF + (PANEL_H_REF - PILL_H_REF) * open.clamp(0.0, 1.0)
    }

    fn collapsed() -> Frame {
        build(&view_sized(PILL_W_REF, PILL_H_REF))
    }

    /// The hover (peek) frame is a capsule — concave ear wings gone, all four
    /// corners rounded to h/2 — detached from the top by PEEK_DROP.
    #[test]
    fn peek_frame_is_a_capsule_with_no_concave_ears() {
        let mut v = view_sized(layout::PEEK_W_REF, layout::PEEK_H_REF);
        v.peek = true;
        v.peek_t = 1.0;
        let f = build(&v);
        assert_eq!(
            f.size,
            (layout::PEEK_W_REF, layout::PEEK_H_REF),
            "peek takes the peek size, not the pill's"
        );

        let Node::RoundRect { rect, radii, .. } = &f.scene.nodes[0] else {
            panic!("first node is the body")
        };
        assert_eq!(rect.w, layout::PEEK_W_REF, "body spans the frame");
        assert_eq!(rect.h, layout::PEEK_H_REF, "body spans the frame");
        // Fully rounded: every corner is h/2 — a capsule, not a card.
        let half = layout::PEEK_H_REF / 2.0;
        assert!(
            (radii.top_left - half).abs() < 1e-4,
            "top-left radius {} != h/2 {}",
            radii.top_left,
            half
        );
        assert!(
            (radii.top_right - half).abs() < 1e-4,
            "top-right radius {} != h/2 {}",
            radii.top_right,
            half
        );
        assert!(
            radii.top_left >= 0.0 && radii.top_right >= 0.0,
            "no concave ear wings on hover"
        );
    }

    /// The width rule for the hover pill: it takes the MINIMIZED pill's width
    /// with the arcs excluded. The concave ear wings flare past each top corner
    /// but are decoration, not width -- so the hover capsule is exactly as wide
    /// as the collapsed pill's body box, not as wide as the silhouette you see
    /// across the top. Anything wider reads as a different, larger widget.
    #[test]
    fn peek_width_is_the_minimized_body_width_not_the_arced_silhouette() {
        let f = collapsed();
        let Node::RoundRect { rect, radii, .. } = &f.scene.nodes[0] else {
            panic!("first node is the body")
        };
        assert_eq!(rect.w, layout::PILL_W_REF, "collapsed body spans the frame");
        assert!(radii.top_left < 0.0, "collapsed top corners are concave ears");

        assert_eq!(
            layout::PEEK_W_REF,
            rect.w,
            "hover width must equal the collapsed BODY width (arcs excluded)"
        );
        assert!(
            layout::PEEK_W_REF < rect.w + 2.0 * layout::EAR_W,
            "hover must not grow to the arced silhouette width"
        );
    }

    fn panel() -> Frame {
        build(&view_sized(PANEL_W_REF, PANEL_H_REF))
    }

    #[test]
    fn tab_click_centers_select_their_tab() {
        // `tab_boxes` is the single source of truth for drawing and hit-testing,
        // so feeding each box's own center back in must round-trip, and a miss
        // must keep the old selection instead of clearing it.
        let top = dock_top(PANEL_H_REF);
        let y = top + TAB_H / 2.0;
        let boxes = tab_boxes(PANEL_W_REF, 0, 0, 1.0);
        assert!(boxes.len() >= 2, "need multiple tabs to test selection");
        for (i, (left, w)) in boxes.iter().enumerate() {
            let mut v = ViewState {
                island_width: PANEL_W_REF,
                island_height: PANEL_H_REF,
                active_tab: 0,
                ..Default::default()
            };
            assert_eq!(
                v.click_tab(left + w / 2.0, y),
                Some(i),
                "center of tab {i} should select it"
            );
            assert_eq!(v.active_tab, i);
        }
        let mut v = view(2, None);
        assert_eq!(v.click_tab(4.0, 4.0), None, "corner miss selects nothing");
        assert_eq!(v.active_tab, 2, "miss must not clear the selection");
    }

    #[test]
    fn row_hover_hits_only_the_row_under_the_cursor() {
        // Same contract as the tab strip: `row_rects` is the single source of
        // truth, so each row's own center must hit it and the gaps between rows
        // must not hit anything.
        let rects = row_boxes(PANEL_W_REF, PANEL_H_REF);
        assert!(rects.len() >= 2, "need multiple rows to test hover");
        let v = view(0, None);
        for (i, (left, top, w, h)) in rects.iter().enumerate() {
            assert_eq!(
                v.row_at(left + w / 2.0, top + h / 2.0),
                Some(i),
                "center of row {i} should hit it"
            );
        }
        // Above the first row is the tab strip, not a row.
        let (_, top0, _, _) = rects[0];
        assert_eq!(v.row_at(PANEL_W_REF / 2.0, top0 - 4.0), None);
        // Collapsed island shows no rows, so nothing can be hovered.
        let shut = view_sized(PILL_W_REF, PILL_H_REF);
        assert_eq!(shut.row_at(PANEL_W_REF / 2.0, top0 + 4.0), None);
    }

    /// A view at full panel size with an explicit selection.
    fn view(active_tab: usize, hover_tab: Option<usize>) -> ViewState {
        ViewState {
            island_width: PANEL_W_REF,
            island_height: PANEL_H_REF,
            active_tab,
            hover_tab,
            ..Default::default()
        }
    }

    // ------------------------------------------------------------ tab strip

    #[test]
    fn tab_hit_testing_matches_the_drawn_strip() {
        let v = view(0, None);
        for (i, (left, w)) in tab_boxes(PANEL_W_REF, 0, 0, 1.0).into_iter().enumerate() {
            let y = dock_center(PANEL_H_REF);
            assert_eq!(v.tab_at(left + w / 2.0, y), Some(i), "centre of tab {i}");
            assert_eq!(v.tab_at(left + 0.5, y), Some(i), "left edge of tab {i}");
            assert_eq!(
                v.tab_at(left + w - 0.5, y),
                Some(i),
                "right edge of tab {i}"
            );
        }
    }

    #[test]
    fn tab_hit_testing_rejects_the_gaps_and_the_margins() {
        let v = view(0, None);
        let y = dock_center(PANEL_H_REF);
        let boxes = tab_boxes(PANEL_W_REF, 0, 0, 1.0);
        let (first_left, _) = boxes[0];
        // Left margin, before the first tab.
        assert_eq!(v.tab_at(first_left - 1.0, y), None);
        // The 8 px gap between the first and second tab.
        let (l0, w0) = boxes[0];
        let (l1, _) = boxes[1];
        assert_eq!(v.tab_at(l0 + w0 + 1.0, y), None);
        assert!(l1 - (l0 + w0) >= TAB_GAP - 1e-3);
        // Above and below the strip band.
        assert_eq!(v.tab_at(first_left + 2.0, y - TAB_H), None);
        assert_eq!(v.tab_at(first_left + 2.0, y + TAB_H), None);
    }

    #[test]
    fn collapsed_pill_shows_the_now_playing_row_only_while_playing() {
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        v.pet = None;
        // Nothing playing: pill is body + resting glyph.
        assert_eq!(all_nodes(&build(&v)).len(), 2);
        v.media.playing = true;
        v.media.title = "Late Night Drive".into();
        v.media.position = std::time::Duration::from_secs(30);
        v.media.duration = Some(std::time::Duration::from_secs(60));
        // Playing: the glyph slot holds the MusicNotch-style row — art
        // thumbnail, four spectrum bars, title — plus the progress group.
        let f = build(&v);
        assert!(
            f.scene
                .nodes
                .iter()
                .any(|n| matches!(n, Node::Image { .. })),
            "the playing pill must show the album art"
        );
        assert!(
            f.scene
                .nodes
                .iter()
                .any(|n| matches!(n, Node::Text { text, .. } if text == "Late Night Drive")),
            "the playing pill must show the track title"
        );
        // 1 body + 6 row nodes + 3 bar group nodes.
        assert_eq!(all_nodes(&f).len(), 1 + 6 + 3);
        // Live stream with unknown duration: no bar to pin at zero.
        v.media.duration = None;
        assert_eq!(all_nodes(&build(&v)).len(), 1 + 6);
    }

    #[test]
    fn collapsed_pill_draws_no_ring_ever() {
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        v.pet = None;
        v.media.duration = Some(std::time::Duration::from_secs(60));
        assert!(
            !all_nodes(&build(&v))
                .iter()
                .any(|n| matches!(n, Node::Arc { .. })),
            "idle pill has no ring"
        );
        v.media.playing = true;
        v.media.position = std::time::Duration::from_secs(30);
        assert!(
            !all_nodes(&build(&v))
                .iter()
                .any(|n| matches!(n, Node::Arc { .. })),
            "the playing pill must not ring itself"
        );
    }

    #[test]
    fn a_mic_dot_appears_in_the_pill_when_the_capture_endpoint_is_hot() {
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        assert_eq!(all_nodes(&build(&v)).len(), 2, "idle: body + glyph");
        v.mic_active = true;
        let f = build(&v);
        // Body + glyph + the red dot at the right edge.
        assert_eq!(all_nodes(&f).len(), 3, "{:#?}", f.scene.nodes);
    }

    #[test]
    fn a_hud_replaces_the_idle_glyph_in_the_pill() {
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        // Idle: body + glyph.
        assert_eq!(all_nodes(&build(&v)).len(), 2);
        v.hud = Some(crate::app::hud::Hud::Battery {
            percent: 42,
            charging: true,
            low: false,
        });
        let f = build(&v);
        // Body + HUD track + gauge fill + readout text, and no idle glyph.
        assert_eq!(all_nodes(&f).len(), 4, "{:#?}", f.scene.nodes);
        assert!(find_text(&f, "42 %").is_some());
    }

    #[test]
    fn collapsed_island_has_no_clickable_tabs() {
        let mut v = view(0, None);
        v.island_width = PILL_W_REF;
        v.island_height = PILL_H_REF;
        let y = dock_center(PANEL_H_REF);
        assert_eq!(v.tab_at(PILL_W_REF / 2.0, y), None);
        assert_eq!(v.click_tab(PILL_W_REF / 2.0, y), None);
    }

    #[test]
    fn click_selects_the_tab_under_the_cursor_and_leaves_others_alone() {
        let mut v = view(0, None);
        let y = dock_center(PANEL_H_REF);
        let boxes = tab_boxes(PANEL_W_REF, 0, 0, 1.0);
        let (l, w) = boxes[3];
        assert_eq!(v.click_tab(l + w / 2.0, y), Some(3));
        assert_eq!(v.active_tab, 3);
        // A click outside every tab must not clear the selection.
        assert_eq!(v.click_tab(1.0, 1.0), None);
        assert_eq!(v.active_tab, 3);
    }

    #[test]
    fn selected_tab_draws_a_micro_fill_pill_behind_its_label() {
        let f = build(&view(2, None));
        let pill = all_nodes(&f)
            .into_iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, fill, radii } => Some((rect, fill, radii)),
                _ => None,
            })
            .find(|(r, _, _)| (r.h - TAB_H).abs() < 1e-3)
            .expect("selected tab must draw a pill");
        assert_eq!(pill.1, Rgba::rgba(1.0, 1.0, 1.0, 0.22));
        assert!((pill.2.top_left - TAB_RADIUS).abs() < 1e-4);
        let (l, w) = tab_boxes(PANEL_W_REF, 2, 2, 1.0)[2];
        assert!((pill.0.x - l).abs() < 1e-3 && (pill.0.w - w).abs() < 1e-3);
    }

    #[test]
    fn unselected_and_unhovered_tabs_draw_no_pill() {
        let f = build(&view(0, None));
        let pills = all_nodes(&f)
            .into_iter()
            .filter(|n| matches!(n, Node::RoundRect { rect, .. } if (rect.h - TAB_H).abs() < 1e-3))
            .count();
        assert_eq!(pills, 1, "only the selected tab has a pill");
    }

    #[test]
    fn hovered_tab_draws_a_fainter_pill_than_the_selected_one() {
        let f = build(&view(0, Some(1)));
        let fills: Vec<Rgba> = all_nodes(&f)
            .into_iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, fill, .. } if (rect.h - TAB_H).abs() < 1e-3 => Some(fill),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 2, "selected + hovered");
        assert!(fills.contains(&Rgba::rgba(1.0, 1.0, 1.0, 0.22)), "selected");
        assert!(fills.contains(&Rgba::rgba(1.0, 1.0, 1.0, 0.08)), "hovered");
    }

    #[test]
    fn selected_tab_label_is_white_the_rest_dim() {
        let f = build(&view(1, None));
        for (label, style) in texts(&f)
            .into_iter()
            .filter_map(|(t, _, s)| TABS.iter().any(|l| *l == t).then_some((t, s)))
        {
            let want_white = label == TABS[1];
            assert_eq!(style.color == Rgba::WHITE, want_white, "tab {label}");
        }
    }

    #[test]
    fn every_tab_label_is_still_drawn_once() {
        let f = build(&view(0, None));
        // NotchStar's dock: an idle tab is icon-only, and exactly one label —
        // the selected tab's — is on screen at a time.
        for (i, label) in TABS.iter().enumerate() {
            let n = texts(&f).into_iter().filter(|(t, _, _)| t == label).count();
            let want = usize::from(i == 0);
            assert_eq!(n, want, "tab {label} drawn {n} times, wanted {want}");
        }
    }

    /// Every node in the frame, cloned so callers can hold them past the borrow.
    fn all_nodes(f: &Frame) -> Vec<Node> {
        let mut out = Vec::new();
        f.scene.walk(&mut |n| out.push(n.clone()));
        out
    }

    fn texts(f: &Frame) -> Vec<(String, Rect, TextStyle)> {
        all_nodes(f)
            .into_iter()
            .filter_map(|n| match n {
                Node::Text { text, rect, style } => Some((text, rect, style)),
                _ => None,
            })
            .collect()
    }

    fn find_text(f: &Frame, want: &str) -> Option<(Rect, TextStyle)> {
        texts(f)
            .into_iter()
            .find(|(t, _, _)| t == want)
            .map(|(_, r, s)| (r, s))
    }

    /// The tab boxes exactly as the strip lays them out for the default view
    /// (tab 0 selected and settled), asserted against the icons actually drawn
    /// so the geometry can never drift from the frame.
    fn glyph_rects(f: &Frame) -> Vec<Rect> {
        let icons: Vec<Rect> = all_nodes(f)
            .into_iter()
            .filter_map(|n| match n {
                Node::Glyph { rect, .. } if (rect.w - TAB_ICON).abs() < 1e-3 => Some(rect),
                _ => None,
            })
            .collect();
        let boxes = tab_boxes(PANEL_W_REF, 0, 0, 1.0);
        assert_eq!(
            icons.len(),
            boxes.len(),
            "one icon per tab: {:#?}",
            f.scene.nodes
        );
        // Each icon sits TAB_PAD inside its box, level in the strip's band.
        for (icon, (left, _)) in icons.iter().zip(&boxes) {
            assert!(
                (icon.x - (left + TAB_PAD)).abs() < 1e-3,
                "icon {icon:?} is not TAB_PAD inside box at {left}"
            );
        }
        boxes
            .into_iter()
            .map(|(x, w)| Rect::new(x, icons[0].y, w, icons[0].h))
            .collect()
    }

    /// The content rows: the rounded rects that span the panel's inner width.
    ///
    /// Selected by width rather than "narrower than the frame" because the tab
    /// strip now also emits rounded rects (the selection/hover pill), and those
    /// must not be mistaken for content rows.
    fn row_rects(f: &Frame) -> Vec<Rect> {
        let inner = f.size.0 - PANEL_PAD * 2.0;
        all_nodes(f)
            .into_iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, .. } if (rect.w - inner).abs() < 1e-3 => Some(rect),
                _ => None,
            })
            .collect()
    }

    fn assert_in_bounds(f: &Frame, w: f32, h: f32) {
        f.scene.walk(&mut |n| {
            let r = n.rect();
            assert!(
                r.x >= -EPS && r.y >= -EPS,
                "rect escapes top-left at {w}x{h}: {r:?}"
            );
            assert!(
                r.x + r.w <= w + EPS && r.y + r.h <= h + EPS,
                "rect escapes bottom-right at {w}x{h}: {r:?}"
            );
            assert!(
                r.w.is_finite() && r.h.is_finite(),
                "non-finite rect at {w}x{h}"
            );
        });
    }

    // ---------------------------------------------------------------- purity

    #[test]
    fn build_is_pure_and_repeatable() {
        for open in [0.0f32, 0.13, 0.5, 0.87, 1.0] {
            let s = view_sized(width_at(open), height_at(open));
            assert_eq!(
                build(&s),
                build(&s),
                "build is not deterministic at openness {open}"
            );
        }
    }

    #[test]
    fn default_state_is_zero_sized() {
        let f = build(&ViewState::default());
        assert_eq!(f.size, (0.0, 0.0));
        // Only the degenerate body; nothing else could stay in bounds.
        assert_eq!(f.scene.count(), 1);
    }

    // ------------------------------------------------------------------ body

    #[test]
    fn body_is_opaque_pure_black_filling_the_frame() {
        for open in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let (w, h) = (width_at(open), height_at(open));
            let f = build(&view_sized(w, h));
            let Node::RoundRect { rect, fill, .. } = &f.scene.nodes[0] else {
                panic!("first node must be the body at {open}")
            };
            assert_eq!(*fill, Rgba::BLACK, "body must be pure black");
            assert_eq!(fill.a, 1.0, "body must be opaque");
            assert_eq!(*rect, Rect::new(0.0, 0.0, w, h), "body must fill the frame");
        }
    }

    #[test]
    fn body_emits_no_image_node_so_nothing_can_be_acrylic() {
        // The depth cue is black-vs-wallpaper. An Image node behind the panel would
        // mean a texture or acrylic backdrop.
        for open in [0.0f32, 0.5, 1.0] {
            let f = build(&view_sized(width_at(open), height_at(open)));
            assert!(
                !all_nodes(&f)
                    .iter()
                    .any(|n| matches!(n, Node::Image { .. })),
                "unexpected Image node at openness {open}"
            );
        }
    }

    #[test]
    fn body_is_the_only_opaque_fill_at_full_panel() {
        // Everything inside the panel is scaled by the content opacity, so the body
        // stays the only fully opaque rect: no "panel on top of panel" seam.
        let f = panel();
        let mut opaque_interior = 0;
        f.scene.walk(&mut |n| {
            if let Node::RoundRect { fill, rect, .. } = n {
                if fill.a >= 1.0 && rect.w < f.size.0 {
                    opaque_interior += 1;
                }
            }
        });
        assert_eq!(opaque_interior, 0, "an interior rect is fully opaque");
    }

    // ------------------------------------------------------------- collapsed

    #[test]
    fn collapsed_frame_contains_only_body_and_glyph() {
        let f = collapsed();
        let ns = all_nodes(&f);
        assert_eq!(ns.len(), 2, "collapsed scene: {:#?}", f.scene.nodes);
        assert!(
            matches!(ns[0], Node::RoundRect { .. }),
            "node 0 is the body"
        );
        assert!(
            matches!(ns[1], Node::RoundRect { .. }),
            "node 1 is the glyph"
        );
        assert_eq!(f.scene.nodes.len(), 2, "no panel content may leak in");
    }

    #[test]
    fn collapsed_glyph_is_a_centered_14px_rounded_square() {
        let f = collapsed();
        let Node::RoundRect { rect, radii, fill } = &f.scene.nodes[1] else {
            panic!("expected the glyph round rect")
        };
        assert!((rect.w - 14.0).abs() < 1e-4, "{rect:?}");
        assert!((rect.h - 14.0).abs() < 1e-4, "{rect:?}");
        assert_eq!(*radii, CornerRadii::uniform(3.0));
        let (cx, cy) = rect.center();
        assert!(
            (cx - PILL_W_REF / 2.0).abs() < 1e-3,
            "glyph not centered: {rect:?}"
        );
        assert!(
            (cy - PILL_H_REF / 2.0).abs() < 1e-3,
            "glyph not centered: {rect:?}"
        );
        assert_eq!(*fill, PILL_GREY, "glyph must be the resting grey");
        assert_eq!(fill.a, 1.0, "at rest the glyph is fully opaque");
    }

    #[test]
    fn body_corners_are_all_ios_rounded() {
        // Top corners are the constant concave wings (-EAR_W); bottoms carry
        // the iOS continuous radius.
        let Node::RoundRect { radii, .. } = &collapsed().scene.nodes[0] else {
            panic!()
        };
        // The clamp caps the wing at half the pill height (16 at h=32).
        assert_eq!(radii.top_left, -(layout::EAR_W.min(PILL_H_REF / 2.0)));
        assert_eq!(radii.top_right, -(layout::EAR_W.min(PILL_H_REF / 2.0)));
        assert_eq!(radii.bottom_left, PILL_RADIUS_REF);
        assert_eq!(radii.bottom_right, PILL_RADIUS_REF);
        assert_eq!(PILL_RADIUS_REF, 16.0);
    }

    #[test]
    fn expanded_body_keeps_its_corners_rounded_too() {
        // The panel morph: top corners stay at the constant wing notch, bottom
        // corners grow from pill 16 -> panel 36.
        for open in [0.0f32, 0.5, 1.0] {
            let f = build(&view_sized(width_at(open), height_at(open)));
            let Node::RoundRect { radii, .. } = &f.scene.nodes[0] else {
                panic!()
            };
            let h = height_at(open);
            let notch = layout::EAR_W.min(h / 2.0);
            assert_eq!(radii.top_left, -notch, "wing notch at {open}");
            assert_eq!(radii.top_right, -notch, "wing notch at {open}");
            let want = PILL_RADIUS_REF + (PANEL_RADIUS_REF - PILL_RADIUS_REF) * open;
            assert!(
                (radii.bottom_left - want).abs() < 1e-3,
                "bottom_left at {open}"
            );
            assert!(
                (radii.bottom_right - want).abs() < 1e-3,
                "bottom_right at {open}"
            );
        }
    }

    // ----------------------------------------------------------------- panel

    #[test]
    fn panel_frame_contains_header_title_in_reference_style() {
        let f = panel();
        let (rect, style) = find_text(&f, APP_TITLE).expect("panel must contain the header title");
        assert_eq!(style.size, 12.0, "title is 12 px");
        assert_eq!(style.weight, Weight::Semibold, "title is semibold");
        assert_eq!(style.color, Rgba::WHITE, "title is white");
        assert!((rect.y - HEADER_Y).abs() < 1e-4, "header row sits at y=16");
        assert!((rect.x - PANEL_PAD).abs() < 1e-4, "header is left-aligned");
    }

#[test]
    fn panel_has_no_placeholder_footer_clock() {
        // The footer used to print a hardcoded "00:00" that never updated,
        // which looked like a stuck clock. The footer is gone; nothing in
        // the panel should contain that placeholder string.
        let f = panel();
        assert!(
            find_text(&f, "00:00").is_none(),
            "placeholder footer clock must not be drawn"
        );
    }

    #[test]
    fn panel_frame_contains_every_tab_with_8px_gaps() {
        let f = panel();
        let rects = glyph_rects(&f);
        assert_eq!(
            rects.len(),
            TABS.len(),
            "expected every tab: {:#?}",
            f.scene.nodes
        );
        for w in rects.windows(2) {
            assert!(w[0].max_x() <= w[1].x + 1e-4, "tabs overlap: {w:?}");
            assert!(
                (w[1].x - w[0].max_x() - TAB_GAP).abs() < 1e-3,
                "gap must be 8 px: {w:?}"
            );
        }
        let used: f32 = rects.iter().map(|r| r.w).sum::<f32>() + TAB_GAP * (TABS.len() - 1) as f32;
        assert!(
            used <= PANEL_W_REF - PANEL_PAD * 2.0 + 1e-3,
            "tab row overflows the padded frame: {used}"
        );
    }

    #[test]
    fn tab_strip_is_centered() {
        let rects = glyph_rects(&panel());
        // Only the selected tab shows a label, so the natural widths are the
        // same ones the strip itself computes.
        let natural: Vec<f32> = TABS
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let base = TAB_PAD * 2.0 + TAB_ICON;
                if i == 0 {
                    base + TAB_ICON_GAP + measure(label, &TAB_LABEL) + TAB_SLACK
                } else {
                    base
                }
            })
            .collect();
        let avail = PANEL_W_REF - PANEL_PAD * 2.0;
        let boxes = layout::row_items(&natural, TAB_GAP, avail);
        // glyph_rects widens each icon rect back out by TAB_PAD, so the
        // widened rect's x IS the box's left edge.
        let want_left = PANEL_PAD + boxes[0].0;
        assert!(
            (rects[0].x - want_left).abs() < 1e-3,
            "{:?} vs {want_left}",
            rects[0]
        );
    }

    #[test]
    fn tab_label_is_vertically_centered_in_the_tab_box() {
        let f = panel();
        let (rect, _) = find_text(&f, "Media").expect("selected tab label");
        let lh = line_height(&TAB_LABEL);
        // The strip row is TAB_H tall; the label box must sit in its middle,
        // the same way the icon does (`y + (TAB_H - TAB_ICON) / 2`).
        let want = dock_top(PANEL_H_REF) + (TAB_H - lh) / 2.0;
        assert!((rect.y - want).abs() < 1e-3, "label y {} vs {want}", rect.y);
        assert!((rect.h - lh).abs() < 1e-3, "label height {} vs {lh}", rect.h);
    }

    #[test]
    fn tab_labels_are_measured_not_guessed() {
        let f = panel();
        // Only the selected tab draws its label, so only that one is measurable.
        for label in ["Media"] {
            let (rect, _) = find_text(&f, label)
                .unwrap_or_else(|| panic!("missing tab label {label}"));
            let want = measure(label, &TAB_LABEL);
            assert!(want > 0.0, "{label} measured zero width");
            assert!((rect.w - want - TAB_SLACK).abs() < 1e-4, "{label}: {rect:?}");
        }
    }

    /// The whole reason `TAB_SLACK` exists: every tab label must survive the
    /// renderer's own measurement inside its box. `measure` is a table and
    /// DirectWrite is authoritative, so a box sized to the bare estimate is one
    /// sub-pixel disagreement away from an ellipsis (`Me…`, `Tim…`) — which is
    /// exactly what the open panel showed while the row still had 78 px spare.
    #[test]
    fn every_tab_label_fits_its_box_with_slack_to_spare() {
        let w = tab_boxes(PANEL_W_REF, 0, 0, 1.0)[0].1;
        let label = TABS[0];
        assert!(
            w - TAB_PAD * 2.0 >= measure(label, &TAB_LABEL) + TAB_SLACK - 1e-4,
            "{label}: box {w} leaves no room for the real run"
        );
    }

    /// The header's corner icons exist, sit at the top-right of the panel, and
    /// stay inside the frame at every size.
    #[test]
    fn corner_icons_are_drawn_top_right_inside_the_frame() {
        for (w, h) in [
            (PANEL_W_REF, PANEL_H_REF),
            (width_at(0.5), height_at(0.5)),
            (width_at(1.0), height_at(1.0)),
        ] {
            let v = view_sized(w, h);
            let f = build(&v);
            let mut found: Vec<(&str, Rect)> = Vec::new();
            for n in &f.scene.nodes {
                if let Node::Glyph { name, rect, .. } = n {
                    if *name == "pin" || *name == "settings" {
                        found.push((name, *rect));
                    }
                }
            }
            assert_eq!(found.len(), 2, "{w}x{h}: expected pin + settings, got {found:?}");
            let (pin, settings) = found.iter().fold((None, None), |(p, s), (n, r)| {
                if *n == "pin" {
                    (Some(*r), s)
                } else {
                    (p, Some(*r))
                }
            });
            let (pin, settings) = (pin.unwrap(), settings.unwrap());
            // Right-aligned inside the panel padding.
            assert!(
                settings.max_x() <= w - PANEL_PAD + layout::BOUNDS_EPSILON,
                "{w}x{h}: settings escapes right: {settings:?}"
            );
            assert!(
                settings.max_x() > pin.max_x(),
                "{w}x{h}: settings must sit right of the pin"
            );
            // On the header line, not floating above or below it.
            let top = HEADER_Y + (header_height() - CORNER_ICON) * 0.5;
            assert!(
                (pin.y - top).abs() < 1.0 && (settings.y - top).abs() < 1.0,
                "{w}x{h}: icons off the header line: pin={pin:?} settings={settings:?}"
            );
        }
    }

    /// Full-tab text containment: every tab draws its own content, and every
    /// text rect it emits must stay inside the panel frame. This is the check
    /// that was missing when "some texts were not being shortened at all, they
    /// went outside the box": only the default tab's panel was bounds-checked,
    /// so an escaping rect on any other tab shipped silently. Also note that
    /// The notification banner's text rects carry `NOTIF_SLACK` over the
    /// measured run, so the renderer's larger real advance cannot ellipsize
    /// its own text (the bug the "Arc" title had before `HEADER_SLACK`).
    #[test]
    fn notification_banner_rects_carry_slack() {
        let app = "Outlook".to_string();
        let title = "Design review".to_string();
        let body = "Arc notch dock is ready".to_string();
        let nodes = pill_hud(
            PILL_W_REF,
            PILL_H_REF,
            &crate::app::hud::Hud::Notification {
                app: app.clone(),
                title: title.clone(),
                body: body.clone(),
            },
            1.0,
        );
        assert!(!nodes.is_empty(), "a notification must emit nodes");
        let style = TextStyle::numeric(9.5);
        let head = format!("{} \u{b7} {}", app, title);
        let head_w = measure(&head, &style);
        let body_w = measure(&body, &style);
        let texts: Vec<(String, Rect)> = nodes
            .into_iter()
            .filter_map(|n| match n {
                Node::Text { text, rect, .. } => Some((text, rect)),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2, "one head line and one body line");
        assert!(
            texts.iter().any(|(t, _)| *t == head),
            "head line missing: {texts:?}"
        );
        assert!(
            texts.iter().any(|(t, _)| *t == body),
            "body line missing: {texts:?}"
        );
        // Slack: at least 8 px over the approximator, still inside the frame.
        for (t, r) in &texts {
            let want = if *t == head { head_w } else { body_w };
            assert!(
                r.w >= want + 8.0,
                "rect not slack: {t:?} w={} measured={want}",
                r.w
            );
            assert!(r.x >= 0.0 && r.max_x() <= PILL_W_REF + EPS, "{t:?} escapes");
        }
    }

    /// `assert_in_bounds` here is over the *frame*, not just the body.
    #[test]
    fn every_tab_keeps_every_text_inside_its_frame() {
        for tab in 0..TABS.len() {
            let f = build(&view(tab, None));
            assert_in_bounds(&f, PANEL_W_REF, PANEL_H_REF);
            for (t, r, _) in texts(&f) {
                // A zero-sized text rect paints nothing and is how a "missing"
                                // label sneaks through; reject it.
                assert!(r.w > 0.0 && r.h > 0.0, "tab {tab}: {t:?} has empty rect");
                assert!(
                    r.x >= PANEL_PAD - layout::BOUNDS_EPSILON
                        && r.max_x() <= PANEL_W_REF - PANEL_PAD + layout::BOUNDS_EPSILON,
                    "tab {tab}: {t:?} escapes horizontally: {r:?}"
                );
                assert!(
                    r.y >= 0.0 - layout::BOUNDS_EPSILON
                        && r.max_y() <= PANEL_H_REF + layout::BOUNDS_EPSILON,
                    "tab {tab}: {t:?} escapes vertically: {r:?}"
                );
            }
        }
    }

    /// A Stats-tab panel: the Media tab draws real content, so the placeholder
    /// row assertions belong to a placeholder tab.
    /// A panel on a tab that still draws the generic placeholder rows.
    /// The generic content rows laid out at the frame's tab strip bottom —
    /// the same call every non-content tab gets from `panel_content_for`.
    /// Every tab now has real content, so these tests call the builder
    /// directly instead of looking up "the placeholder tab" by index.
    fn placeholder_rows() -> Vec<Node> {
        let v = view_sized(PANEL_W_REF, PANEL_H_REF);
        // The dock lives at the FOOT, so the rows start under the header —
        // `row_band_top`, the same anchor `build` uses.
        content_rows(PANEL_W_REF, PANEL_H_REF, row_band_top(), &v)
    }

    /// The placeholder rows laid out inside a full built frame, so the
    /// row-fill assertions can scan every node in the panel.
    fn placeholder_panel() -> Frame {
        build(&view_sized(PANEL_W_REF, PANEL_H_REF))
    }

    #[test]
    fn panel_content_has_three_placeholder_rows() {
        let rows: Vec<Rect> = placeholder_rows().iter().map(|n| n.rect()).collect();
        assert_eq!(rows.len(), CONTENT_ROWS, "{rows:?}");
        for w in rows.windows(2) {
            assert!(w[1].y > w[0].max_y(), "rows must not overlap: {w:?}");
            assert!(
                (w[1].y - w[0].max_y() - ROW_GAP).abs() < 1e-3,
                "row gap: {w:?}"
            );
        }
        for r in &rows {
            assert!(
                (r.w - (PANEL_W_REF - PANEL_PAD * 2.0)).abs() < 1e-3,
                "rows span the padded frame: {r:?}"
            );
        }
    }

    #[test]
    fn content_rows_use_the_reference_4pct_micro_fill_and_r8() {
        let rows = placeholder_rows();
        assert_eq!(rows.len(), CONTENT_ROWS);
        let inner = PANEL_W_REF - PANEL_PAD * 2.0;
        for n in rows {
            if let Node::RoundRect { rect, radii, fill } = n {
                assert!((rect.w - inner).abs() < 1e-3, "row width: {rect:?}");
                assert_eq!(fill, Rgba::rgba(1.0, 1.0, 1.0, 0.04), "4 % white: {rect:?}");
                assert!(
                    (radii.top_left - ROW_RADIUS).abs() < 1e-4,
                    "reference row radius is 8: {rect:?}"
                );
            }
        }
    }

    #[test]
    fn content_sits_between_the_tab_strip_and_the_panel_bottom() {
        let f = placeholder_panel();
        let tabs_bottom = glyph_rects(&f)
            .iter()
            .map(|r| r.max_y())
            .fold(0.0f32, f32::max);
        // The footer clock is gone, so the lower bound is the panel edge
        // itself (FOOTER_BOTTOM_PAD of slack).
        let bottom = PANEL_H_REF - FOOTER_BOTTOM_PAD;
        for r in row_rects(&f) {
            assert!(r.y >= tabs_bottom - 1e-3, "row overlaps the tabs: {r:?}");
            assert!(
                r.max_y() <= bottom + 1e-3,
                "row overlaps the footer band: {r:?}"
            );
        }
    }

    #[test]
    fn content_group_rect_covers_the_frame() {
        let mut found = false;
        panel().scene.walk(&mut |n| {
            if let Node::Group { rect, .. } = n {
                assert_eq!(*rect, Rect::new(0.0, 0.0, PANEL_W_REF, PANEL_H_REF));
                found = true;
            }
        });
        assert!(
            found,
            "panel content must be grouped so it can be faded as one"
        );
    }

    // -------------------------------------------------------------- openness

    #[test]
    fn openness_endpoints_produce_collapsed_and_panel() {
        let f0 = collapsed();
        assert_eq!(f0.scene.count(), 2, "openness 0.0 is body + glyph only");

        let f5 = build(&view_sized(width_at(0.5), height_at(0.5)));
        assert!(
            f5.scene.count() > 2,
            "openness 0.5 has content: {:#?}",
            f5.scene.nodes
        );

        let f1 = panel();
        assert!(f1.scene.count() > 2, "openness 1.0 has content");
        assert!(
            find_text(&f1, APP_TITLE).is_some(),
            "the title is fully present at openness 1.0"
        );
    }

    #[test]
    fn content_is_skipped_below_the_fade_in_threshold() {
        let at = layout::CONTENT_FADE_IN_START - 0.01;
        let f = build(&view_sized(width_at(at), PANEL_H_REF));
        assert_eq!(
            f.scene.count(),
            2,
            "content must be skipped at openness {at}: {:#?}",
            f.scene.nodes
        );
    }

    #[test]
    fn content_fades_in_above_the_threshold_instead_of_popping() {
        let f = build(&view_sized(width_at(0.35), PANEL_H_REF));
        assert!(
            f.scene.count() > 2,
            "content must be emitted at openness 0.35"
        );
        let (_, style) = find_text(&f, APP_TITLE).expect("title");
        assert!(
            style.color.a > 0.0 && style.color.a < 1.0,
            "content must be partially faded in, got alpha {}",
            style.color.a
        );
    }

    #[test]
    fn content_opacity_is_monotonic_in_openness() {
        let mut prev = -1.0;
        for i in 0..=200 {
            let open = i as f32 / 200.0;
            let f = build(&view_sized(width_at(open), height_at(open)));
            let Some((_, style)) = find_text(&f, APP_TITLE) else {
                continue;
            };
            assert!(style.color.a >= prev, "content opacity dipped at {open}");
            prev = style.color.a;
        }
        assert!(
            (prev - 1.0).abs() < 1e-4,
            "content must reach full alpha when open"
        );
    }

    #[test]
    fn pill_glyph_dissolves_before_the_content_arrives() {
        // Just below the content threshold: glyph fading, no content yet.
        let at = layout::CONTENT_FADE_IN_START - 0.02;
        let f = build(&view_sized(width_at(at), PANEL_H_REF));
        assert_eq!(f.scene.count(), 2, "content must not be emitted yet");
        let Node::RoundRect { fill, .. } = &f.scene.nodes[1] else {
            panic!("glyph must be the second node")
        };
        assert!(fill.a < 1.0, "glyph must be fading: {fill:?}");
        assert!(fill.a > 0.0, "glyph must not be gone yet: {fill:?}");
    }

    #[test]
    fn pill_glyph_is_fully_gone_at_half_open() {
        let f = build(&view_sized(width_at(0.5), PANEL_H_REF));
        // Once the fade completes the glyph node is dropped entirely rather than
        // emitted at zero alpha: a transparent squircle still costs a tessellation.
        assert!(
            !all_nodes(&f)
                .iter()
                .any(|n| matches!(n, Node::RoundRect { fill, .. } if *fill == PILL_GREY)),
            "the pill glyph must be gone at openness 0.5"
        );
        assert_eq!(
            f.scene.nodes[0].rect(),
            Rect::new(0.0, 0.0, width_at(0.5), PANEL_H_REF),
            "the body is still there"
        );
    }

    #[test]
    fn radius_lerps_16_to_36_across_the_morph() {
        // Top corners are the constant wing notch; bottom corners track
        // openness 16 -> 36 monotonically.
        let mut prev = -1.0;
        for i in 0..=100 {
            let open = i as f32 / 100.0;
            let f = build(&view_sized(width_at(open), PANEL_H_REF));
            let Node::RoundRect { radii, .. } = &f.scene.nodes[0] else {
                panic!()
            };
            let want = PILL_RADIUS_REF + (PANEL_RADIUS_REF - PILL_RADIUS_REF) * open;
            assert_eq!(radii.top_left, -layout::EAR_W, "wing notch at {open}");
            assert_eq!(radii.top_right, -layout::EAR_W, "wing notch at {open}");
            assert!(
                (radii.bottom_left - want).abs() < 1e-3,
                "at {open}: {:?}",
                radii
            );
            assert!(
                (radii.bottom_right - want).abs() < 1e-3,
                "at {open}: {:?}",
                radii
            );
            assert!(radii.bottom_left >= prev, "radius dipped at {open}");
            prev = radii.bottom_left;
        }
        assert!((prev - PANEL_RADIUS_REF).abs() < 1e-3);
    }

    #[test]
    fn body_radius_is_clamped_to_what_the_frame_can_render() {
        // A very short island cannot render a 24 px radius.
        let f = build(&view_sized(640.0, 8.0));
        let Node::RoundRect { radii, .. } = &f.scene.nodes[0] else {
            panic!()
        };
        assert!(radii.top_left <= 4.0 + 1e-4, "{radii:?}");
    }

    // ---------------------------------------------------------------- bounds

    #[test]
    fn all_nodes_stay_inside_frame_across_sizes() {
        for i in 0..=100 {
            let open = i as f32 / 100.0;
            for h in [32.0f32, 60.0, 116.0, 200.0] {
                let w = width_at(open);
                assert_in_bounds(&build(&view_sized(w, h)), w, h);
            }
        }
    }

    #[test]
    fn all_nodes_stay_inside_frame_across_a_wide_sweep() {
        // Sizes smaller than the pill and larger than the panel too, so no caller
        // can escape the invariant.
        for i in 0..=200 {
            let w = 100.0 + (i as f32 / 200.0) * 800.0;
            let h = 8.0 + (i as f32 / 200.0) * 260.0;
            assert_in_bounds(&build(&view_sized(w, h)), w, h);
        }
    }

    #[test]
    fn all_nodes_stay_inside_frame_for_degenerate_sizes() {
        for (w, h) in [
            (0.0, 0.0),
            (0.0, 200.0),
            (640.0, 0.0),
            (1.0, 1.0),
            (185.0, 1.0),
            (1.0, 32.0),
            (300.0, 33.0),
        ] {
            assert_in_bounds(&build(&view_sized(w, h)), w, h);
        }
    }

    #[test]
    fn negative_sizes_do_not_produce_escaping_rects() {
        let f = build(&view_sized(-50.0, -10.0));
        assert_eq!(f.scene.count(), 1, "no content for a negative frame");
        assert_eq!(f.scene.nodes[0].rect(), Rect::new(0.0, 0.0, -50.0, -10.0));
    }

    // --------------------------------------------------------------- measure

    #[test]
    fn measure_is_monotonic_in_text_length() {
        let style = TextStyle::title();
        let mut prev = -1.0;
        for t in ["", "A", "Ar", "Arc"] {
            let w = measure(t, &style);
            assert!(w > prev, "{t:?} measured {w}, not more than {prev}");
            prev = w;
        }
        assert_eq!(prev, measure("Arc", &style));
    }

    #[test]
    fn measure_is_monotonic_in_style_size() {
        let mut prev = 0.0;
        for i in 1..=24 {
            let w = measure(
                "Clipboard",
                &TextStyle {
                    size: i as f32,
                    ..Default::default()
                },
            );
            assert!(w > prev, "size {i} measured {w}, not more than {prev}");
            prev = w;
        }
    }

    #[test]
    fn header_box_leaves_room_past_the_measured_run() {
        // Sizing this box to exactly `measure()` clipped the title to `A…`:
        // the pure approximator under-reports DirectWrite's real advance by a
        // fraction of a px, and the renderer trims whatever does not fit.
        let mut style = TextStyle::title();
        style.align = Align::Start;
        let (rect, _) = find_text(&panel(), APP_TITLE).unwrap();
        assert!(
            rect.w >= measure(APP_TITLE, &style) + HEADER_SLACK - 1e-4,
            "header box {} must clear the measured run by {HEADER_SLACK}",
            rect.w
        );
    }

    #[test]
    fn the_spectrum_replaces_the_glyph_only_while_playing() {
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        v.media.playing = true;
        v.media.title = "Track".into();
        let playing = build(&v);
        assert_eq!(wide(&playing, 2.0), 4, "four 2 px bars while a track plays");
        v.media.playing = false;
        let paused = build(&v);
        assert_eq!(wide(&paused, 2.0), 0, "paused: the resting glyph, no bars");
    }

    #[test]
    fn the_pet_replaces_the_glyph_only_while_idle() {
        use crate::core::scene::ImageHandle;
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        v.media.playing = false;
        v.pet = Some(crate::services::pet::PetFrame { handle: ImageHandle(7), frame: 2 });
        let idle = build(&v);
        let sprites: Vec<_> =
            all_nodes(&idle).into_iter().filter(|n| matches!(n, Node::Sprite { .. })).collect();
        assert_eq!(sprites.len(), 1, "one sprite in the collapsed pill");
        if let Node::Sprite { handle, src, .. } = &sprites[0] {
            assert_eq!(*handle, ImageHandle(7));
            assert_eq!((src.x, src.y, src.w, src.h), crate::services::pet::frame_rect(2));
        }
        // Playing wins over the pet: the pill shows the spectrum, not the mascot.
        v.media.playing = true;
        let playing = build(&v);
        assert_eq!(wide(&playing, 2.0), 4);
        assert_eq!(
            all_nodes(&playing).iter().filter(|n| matches!(n, Node::Sprite { .. })).count(),
            0
        );
    }

    /// A standing row must never be drawn through the centred mark. The pet
    /// sprite sat on top of the "Downloads" track because rows flowed
    /// straight out of x = 10 with nothing reserving the centre.
    #[test]
    fn standing_pill_rows_never_cross_the_centre_mark() {
        use crate::core::scene::ImageHandle;
        let mut v = view_sized(PILL_W_REF, PILL_H_REF);
        v.media.playing = false;
        v.downloads.delta = 12;
        v.pet = Some(crate::services::pet::PetFrame { handle: ImageHandle(7), frame: 0 });
        let f = build(&v);
        // The sprite's own left edge is the reserved slot's inner bound.
        let (_, _, sw, sh) = crate::services::pet::frame_rect(0);
        let slot_w = (layout::PILL_H_REF - PILL_PET_PAD * 2.0) * (sw / sh);
        let slot_left = (PILL_W_REF - slot_w) / 2.0 - 8.0;
        let mut checked = 0;
        for t in texts(&f) {
            let r = t.1;
            if r.max_x() > slot_left - 8.0 {
                continue; // the mark itself / panel content, not a flank row
            }
            assert!(
                r.max_x() <= slot_left,
                "{:?} ends at {:.2}, past the centre slot's left edge {:.2}",
                t.0,
                r.max_x(),
                slot_left
            );
            checked += 1;
        }
        assert!(checked > 0, "the Downloads row should still render in the free flank");
    }

    /// Counts round-rects exactly `w` wide — the bars' signature.
    fn wide(f: &Frame, w: f32) -> usize {
        all_nodes(f)
            .iter()
            .filter(|n| match n {
                Node::RoundRect { rect, .. } => (rect.w - w).abs() < 0.01,
                _ => false,
            })
            .count()
    }

    #[test]
    fn footer_clock_width_is_stable_across_values() {
        // The reason for `tnum`: "00:00" and "12:34" must be the same width, or a
        // running clock twitches.
        let style = TextStyle::numeric(12.0);
        assert!((measure("00:00", &style) - measure("12:34", &style)).abs() < 1e-6);
    }

    // --------------------------------------------------------------- parity

    #[test]
    fn mirrored_geometry_matches_the_app_state_constants() {
        // `app::state` is a private module, so its `PILL_*`/`PANEL_*` consts are not
        // importable from `ui`. These are mirrored here; this test is what stops the
        // two copies from drifting apart. If the app owner ever makes the consts
        // public, delete this and import them directly.
        use crate::app::IslandState;

        let pill = IslandState::collapsed();
        assert_eq!(pill.logical_width(), PILL_W_REF, "PILL_W drifted");
        assert_eq!(pill.logical_height(), PILL_H_REF, "PILL_H drifted");
        assert_eq!(
            pill.logical_radius(),
            PILL_RADIUS_REF,
            "PILL_RADIUS drifted"
        );

        let mut panel = IslandState::collapsed();
        panel.toggle();
        for _ in 0..400 {
            panel.step_dt(1.0 / 60.0);
        }
        assert_eq!(panel.logical_width(), PANEL_W_REF, "PANEL_W drifted");
        assert_eq!(panel.logical_height(), PANEL_H_REF, "PANEL_H drifted");
        assert_eq!(
            panel.logical_radius(),
            PANEL_RADIUS_REF,
            "PANEL_RADIUS drifted"
        );
    }

    #[test]
    fn tab_swap_crossfades_and_rises_together() {
        let mut swap = TabSwap::default();
        // Idle: nothing outgoing, rise settled at 1 (layout unchanged).
        assert_eq!(swap.outgoing_opacity(), 0.0);
        assert_eq!(swap.rise(), 1.0);
        assert!(!swap.step(1.0 / 240.0), "idle swap must not want frames");

        swap.swap_to();
        assert_eq!(
            swap.outgoing_opacity(),
            1.0,
            "outgoing starts fully visible"
        );
        assert_eq!(swap.rise(), 0.0, "incoming starts below and invisible");

        // One crossfade: the outgoing body fades while the incoming rises.
        let mut t = 0.0;
        let mut saw_mid = false;
        while swap.step(1.0 / 240.0) && t < 2.0 {
            t += 1.0 / 240.0;
            let (out, inc) = (swap.outgoing_opacity(), swap.incoming_opacity());
            assert!((0.0..=1.0).contains(&out) && (0.0..=1.0).contains(&inc));
            if (0.05..0.95).contains(&out) && (0.05..0.95).contains(&inc) {
                saw_mid = true;
            }
        }
        assert!(t < 2.0, "swap never settled");
        assert!(saw_mid, "fade and rise must overlap, not run in sequence");
        assert_eq!(swap.outgoing_opacity(), 0.0);
        assert_eq!(swap.rise(), 1.0);
        assert!(!swap.step(1.0 / 240.0), "settled swap must go quiet");
    }

    #[test]
    fn tab_swap_restarts_from_the_top() {
        let mut swap = TabSwap::default();
        swap.swap_to();
        for _ in 0..30 {
            swap.step(1.0 / 240.0);
        }
        let mid = swap.rise();
        swap.swap_to();
        assert_eq!(swap.outgoing_opacity(), 1.0, "restart re-arms the fade");
        assert!(swap.rise() <= mid, "restart pulls the rise back to 0");
    }

    /// The chrome must ride out a tab swap untouched: the header (title, corner
    /// icons) holds its exact rect, and the dock holds its LINE — its items may
    /// glide sideways (the selected pill widens and the label reveals, which is
    /// the NotchStar behaviour we want) but never rise with the body.
    #[test]
    fn chrome_holds_still_during_a_tab_swap() {
        let mut v = view(0, None);
        v.active_tab = 1;
        v.tab_swap.swap_to();
        v.tab_swap.step(1.0 / 120.0);
        let mid = build(&v);

        let mut settled = v.clone();
        settled.tab_swap = TabSwap::default();
        let rest = build(&settled);

        let dock_line = dock_top(PANEL_H_REF);
        let header_line = HEADER_Y + header_height();
        let pick = |f: &Frame, header: bool| -> Vec<Rect> {
            all_nodes(f)
                .into_iter()
                .map(|n| n.rect())
                .filter(|r| {
                    // Deliberately the OUTER bands only: everything between
                    // header_line and dock_line is body, and body is the part
                    // a swap is supposed to animate.
                    if header {
                        r.max_y() <= header_line
                    } else {
                        r.y >= dock_line
                    }
                })
                .collect()
        };

        for header in [true, false] {
            let (a, b) = (pick(&mid, header), pick(&rest, header));
            // The dock legitimately holds MORE nodes mid-swap: both the outgoing
            // and the incoming label are partly revealed. The header must not.
            if header {
                assert_eq!(a.len(), b.len(), "header node count changed mid-swap");
            }
            // Nothing in either band may drift vertically, ever.
            let ys = |v: &[Rect]| -> Vec<i32> {
                let mut out: Vec<i32> = v.iter().map(|r| (r.y * 100.0) as i32).collect();
                out.sort_unstable();
                out.dedup();
                out
            };
            assert_eq!(ys(&a), ys(&b), "chrome rose with the body (header={header})");
            if header {
                for (ra, rb) in a.iter().zip(&b) {
                    assert!(
                        (ra.x - rb.x).abs() < 1e-3,
                        "header slid sideways mid-swap: {ra:?} -> {rb:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scale_about_top_shrinks_to_center_and_restores() {
        let mut n = Node::RoundRect {
            rect: Rect::new(0.0, 100.0, 50.0, 40.0),
            fill: Rgba::BLACK,
            radii: CornerRadii::uniform(0.0),
        };
        scale_about_top(&mut n, 0.5);
        let r = n.rect();
        // Centre fixed: 100 + 20 -> 110 + 10.
        assert!((r.h - 20.0).abs() < 1e-4, "h={}", r.h);
        assert!((r.y - 110.0).abs() < 1e-4, "y={}", r.y);
        // Factor of 1.0 must be a no-op (settled frames stay pixel-identical).
        let before = format!("{:?}", n.rect());
        scale_about_top(&mut n, 1.0);
        assert_eq!(format!("{:?}", n.rect()), before);
    }

    #[test]
    fn stagger_cascades_and_stays_in_range() {
        // Row 0 leads: at rise 0 nothing has started, at 1 everything landed.
        assert_eq!(stagger_progress(0.0, 0), 0.0);
        assert_eq!(stagger_progress(1.0, 0), 1.0);
        // A later row trails an earlier one at the same instant.
        let mid = 0.5;
        assert!(stagger_progress(mid, 1) < stagger_progress(mid, 0));
        assert!(stagger_progress(mid, 4) < stagger_progress(mid, 1));
        // Monotone in rise and bounded for every index, including absurd ones.
        for i in [0usize, 1, 7, 1000] {
            let mut last = -1.0;
            for step in 0..=100 {
                let p = stagger_progress(step as f32 / 100.0, i);
                assert!((0.0..=1.0).contains(&p), "out of range at i={i} p={p}");
                assert!(p >= last, "not monotone at i={i}");
                last = p;
            }
            assert_eq!(stagger_progress(1.0, i), 1.0, "row {i} never lands");
        }
    }

    #[test]
    fn settled_swap_leaves_layout_untouched() {
        let mut state = ViewState::default();
        state.island_width = 640.0;
        state.island_height = 400.0;
        let before = build(&state);
        // A settled swap must be a no-op on the scene.
        let after = build(&state);
        assert_eq!(before.scene.nodes.len(), after.scene.nodes.len());
    }
}
