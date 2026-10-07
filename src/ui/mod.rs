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

pub mod layout;
pub mod media;
pub mod text;

pub use text::{line_height, measure};

/// The island's brand name, shown in the panel header.
pub const APP_TITLE: &str = "Arc";

/// Visible tab labels, in layout order.
pub const TABS: [&str; 5] = ["Media", "Stats", "Timer", "Clipboard", "Usage"];

/// Placeholder content rows. Structure only — no data behind them yet.
pub const CONTENT_ROWS: usize = 3;

/// Gap between tab items, from the reference's tab strip spacing.
pub const TAB_GAP: f32 = 8.0;
/// Horizontal padding inside the panel, applied on both sides.
pub const PANEL_PAD: f32 = 16.0;
/// Top offset of the header row.
pub const HEADER_Y: f32 = 16.0;
/// Gap between the header's line box and the tab strip.
pub const HEADER_GAP: f32 = 6.0;
/// Height of the tab strip band.
pub const TAB_H: f32 = 20.0;
/// Horizontal padding inside a tab, so its box is a superset of its label.
pub const TAB_PAD: f32 = 10.0;
/// Corner radius of the hovered/active tab's background pill.
pub const TAB_RADIUS: f32 = 8.0;
/// Height of one content row.
pub const ROW_H: f32 = 24.0;
/// Gap between content rows.
pub const ROW_GAP: f32 = 8.0;
/// Corner radius of the content rows (reference: `RoundedRectangle(r: 8)`).
pub const ROW_RADIUS: f32 = 8.0;
/// Gap between the footer clock and the bottom edge.
pub const FOOTER_BOTTOM_PAD: f32 = 12.0;
/// Height of the collapsed pill's center glyph.
pub const PILL_GLYPH: f32 = 14.0;
/// Corner radius of the collapsed pill's center glyph.
pub const PILL_GLYPH_RADIUS: f32 = 3.0;
/// Placeholder clock. A real tab replaces this with elapsed/remaining time.
pub const FOOTER_CLOCK: &str = "00:00";

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

/// Tab label style: 12 px regular, dimmed grey (the reference rests on
/// white/grey, with a single album-art accent appearing elsewhere in the shell).
const TAB_LABEL: TextStyle = TextStyle {
    size: 12.0,
    weight: Weight::Regular,
    color: Rgba {
        r: 0.72,
        g: 0.72,
        b: 0.76,
        a: 1.0,
    },
    tabular: false,
    align: Align::Center,
};

/// Tab strip and content state. Grows per phase.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    pub island_width: f32,
    pub island_height: f32,
    /// Index into [`TABS`] of the selected tab.
    pub active_tab: usize,
    /// Index into [`TABS`] under the cursor, if any.
    pub hover_tab: Option<usize>,
    /// Content row under the cursor, if any.
    pub hover_row: Option<usize>,
    /// What the Media tab draws. The app thread refreshes it from
    /// `services::media` each frame; `build` stays pure.
    pub media: crate::services::media::MediaState,
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
        self.active_tab = self.tab_at(x, y)?;
        Some(self.active_tab)
    }

    /// Which tab contains panel-space `(x, y)`, if any.
    pub fn tab_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.content_visible() {
            return None;
        }
        let top = HEADER_Y + header_height() + HEADER_GAP;
        if !(top..top + TAB_H).contains(&y) {
            return None;
        }
        tab_boxes(self.island_width)
            .into_iter()
            .position(|(left, w)| w > 0.0 && x >= left && x <= left + w)
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

    /// Whether the panel content (tabs, rows, footer) is on screen at all.
    ///
    /// Same threshold as content emission: whatever the user cannot see cannot
    /// be hovered or clicked.
    fn content_visible(&self) -> bool {
        openness(self.island_width, self.island_height) > layout::CONTENT_FADE_IN_START
    }
}

/// Each tab's `(left, width)` in panel space, padding included, in layout order.
///
/// Single source of truth for the strip: drawing and hit-testing both read it, so
/// a click can never land on a different tab than the one that was drawn.
fn tab_boxes(fw: f32) -> Vec<(f32, f32)> {
    let natural: Vec<f32> = TABS
        .iter()
        .map(|label| measure(label, &TAB_LABEL) + TAB_PAD * 2.0)
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

    let mut scene = Scene::new();

    // 1. The body. Always emitted, always the whole frame, always pure black and
    //    opaque. This is the whole look. All four corners carry the same radius
    //    (iOS style): the reference pill reads as a rounded rect, not a capsule
    //    and not a square-topped tab. The radius is clamped to what the rect can
    //    actually render.
    scene.push(Node::RoundRect {
        rect: frame,
        radii: CornerRadii::uniform(radius).clamped(fw, fh),
        fill: Rgba::BLACK,
    });

    // A zero-sized island emits nothing else: there is nothing to lay out inside,
    // and any rect could only escape the frame bounds.
    if fw <= 0.0 || fh <= 0.0 {
        return Frame {
            size: (fw, fh),
            scene,
        };
    }

    // 2. Collapsed pill glyph. Fades out across the first half of the morph, so it
    //    dissolves instead of vanishing under the arriving panel content.
    let glyph_opacity = 1.0 - layout::ease(open / layout::PILL_GLYPH_FADE_OUT_END);
    if glyph_opacity > 0.0 {
        let g = PILL_GLYPH;
        scene.push(Node::RoundRect {
            rect: put(fw, fh, Rect::new((fw - g) / 2.0, (fh - g) / 2.0, g, g)),
            radii: CornerRadii::uniform(PILL_GLYPH_RADIUS),
            fill: layout::fade(PILL_GREY, glyph_opacity),
        });
    }

    // 3. Panel content: skipped below the fade-in threshold, alpha-scaled above
    //    it, so it materializes instead of popping in at a fixed size.
    let content_opacity = layout::ease(
        (open - layout::CONTENT_FADE_IN_START) / (1.0 - layout::CONTENT_FADE_IN_START),
    );
    if content_opacity > 0.0 {
        let mut group = Node::Group {
            rect: frame,
            children: panel_content(fw, fh, state),
        };
        if let Node::Group { children, .. } = &mut group {
            children
                .iter_mut()
                .for_each(|c| fade_node(c, content_opacity));
        }
        scene.push(group);
    }

    Frame {
        size: (fw, fh),
        scene,
    }
}

/// Clamp a rect into the frame. Every emitted rect goes through here, which is
/// what guarantees the bounds invariant asserted by the sweep tests.
///
/// This matters because the island is mid-spring: the window can be far smaller
/// than the panel it is morphing out of, and content laid out in panel space must
/// be clipped into the live frame rather than trusted to fit.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// The expanded panel's contents, in draw order: header, tabs, rows, footer.
fn panel_content(fw: f32, fh: f32, state: &ViewState) -> Vec<Node> {
    let mut out = header_block(fw, fh);
    let tabs = tab_strip(fw, fh, header_height(), state);
    let tab_bottom = tabs.iter().map(|n| n.rect().max_y()).fold(0.0f32, f32::max);
    out.extend(tabs);
    if state.active_tab == MEDIA_TAB {
        out.extend(media::content(fw, fh, tab_bottom, &state.media));
    } else {
        out.extend(content_rows(fw, fh, tab_bottom, state));
    }
    out.extend(footer_block(fw, fh));
    out
}

/// Index in [`TABS`] of the Media tab — the one with a real content surface.
pub const MEDIA_TAB: usize = 0;

/// Height of the header row's line box (20 px title × 1.3 line height).
fn header_height() -> f32 {
    line_height(&TextStyle::title())
}

/// Header row: the title, at `TextStyle::title()` (20 px semibold white).
fn header_block(fw: f32, fh: f32) -> Vec<Node> {
    let mut style = TextStyle::title();
    style.align = Align::Start;
    // Reserve exactly the measured run; the renderer re-measures at draw time.
    vec![Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(
                PANEL_PAD,
                HEADER_Y,
                measure(APP_TITLE, &style),
                line_height(&style),
            ),
        ),
        text: APP_TITLE.to_string(),
        style,
    }]
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
fn tab_strip(fw: f32, fh: f32, header_h: f32, state: &ViewState) -> Vec<Node> {
    let y = HEADER_Y + header_h + HEADER_GAP;
    let mut out = Vec::with_capacity(TABS.len() * 3);

    for (i, (label, (left, w))) in TABS.iter().zip(tab_boxes(fw)).enumerate() {
        if w <= 0.0 {
            continue;
        }
        let selected = i == state.active_tab;
        let hovered = state.hover_tab == Some(i);
        if selected || hovered {
            // The reference's micro-fill reads as a *lift* out of pure black, so
            // it has to be light-on-dark: a black-alpha fill over the opaque
            // black body composites to exactly the body colour and is invisible.
            out.push(Node::RoundRect {
                rect: put(fw, fh, Rect::new(left, y, w, TAB_H)),
                radii: CornerRadii::uniform(TAB_RADIUS),
                fill: Rgba::rgba(1.0, 1.0, 1.0, if selected { 0.16 } else { 0.06 }),
            });
        }
        let color = if selected {
            Rgba::WHITE
        } else {
            TAB_LABEL.color
        };
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(
                    left + TAB_PAD,
                    y,
                    w - TAB_PAD * 2.0,
                    line_height(&TAB_LABEL),
                ),
            ),
            text: label.to_string(),
            style: TextStyle {
                color,
                ..TAB_LABEL.clone()
            },
        });
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

/// Panel-space y of the top of the row band: just under the tab strip. Matches
/// the measured `tab_bottom` [`build`] passes, so hit-testing and drawing agree.
fn row_band_top() -> f32 {
    HEADER_Y + header_height() + HEADER_GAP + TAB_H
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

/// Footer clock, bottom-left, tabular figures at 12 px.
fn footer_block(fw: f32, fh: f32) -> Vec<Node> {
    let style = TextStyle::numeric(12.0);
    let w = measure(FOOTER_CLOCK, &style);
    let h = line_height(&style);
    let y = (fh - FOOTER_BOTTOM_PAD - h).max(0.0);
    vec![Node::Text {
        rect: put(fw, fh, Rect::new(PANEL_PAD, y, w, h)),
        text: FOOTER_CLOCK.to_string(),
        style,
    }]
}

/// Multiply a node's opacity by `opacity`, recursively.
fn fade_node(node: &mut Node, opacity: f32) {
    match node {
        Node::RoundRect { fill, .. } => *fill = layout::fade(*fill, opacity),
        Node::Bar { track, fill, .. } => {
            *track = layout::fade(*track, opacity);
            *fill = layout::fade(*fill, opacity);
        }
        Node::Text { style, .. } => style.color = layout::fade(style.color, opacity),
        Node::Glyph { color, .. } => *color = layout::fade(*color, opacity),
        // Images carry no colour of their own; their accent would come from state.
        Node::Image { .. } => {}
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

    fn panel() -> Frame {
        build(&view_sized(PANEL_W_REF, PANEL_H_REF))
    }

    #[test]
    fn tab_click_centers_select_their_tab() {
        // `tab_boxes` is the single source of truth for drawing and hit-testing,
        // so feeding each box's own center back in must round-trip, and a miss
        // must keep the old selection instead of clearing it.
        let top = HEADER_Y + header_height() + HEADER_GAP;
        let y = top + TAB_H / 2.0;
        let boxes = tab_boxes(PANEL_W_REF);
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
        for (i, (left, w)) in tab_boxes(PANEL_W_REF).into_iter().enumerate() {
            let y = HEADER_Y + header_height() + HEADER_GAP + TAB_H / 2.0;
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
        let y = HEADER_Y + header_height() + HEADER_GAP + TAB_H / 2.0;
        let boxes = tab_boxes(PANEL_W_REF);
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
    fn collapsed_island_has_no_clickable_tabs() {
        let mut v = view(0, None);
        v.island_width = PILL_W_REF;
        v.island_height = PILL_H_REF;
        let y = HEADER_Y + header_height() + HEADER_GAP + TAB_H / 2.0;
        assert_eq!(v.tab_at(PILL_W_REF / 2.0, y), None);
        assert_eq!(v.click_tab(PILL_W_REF / 2.0, y), None);
    }

    #[test]
    fn click_selects_the_tab_under_the_cursor_and_leaves_others_alone() {
        let mut v = view(0, None);
        let y = HEADER_Y + header_height() + HEADER_GAP + TAB_H / 2.0;
        let boxes = tab_boxes(PANEL_W_REF);
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
        assert_eq!(pill.1, Rgba::rgba(1.0, 1.0, 1.0, 0.16));
        assert!((pill.2.top_left - TAB_RADIUS).abs() < 1e-4);
        let (l, w) = tab_boxes(PANEL_W_REF)[2];
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
        assert!(fills.contains(&Rgba::rgba(1.0, 1.0, 1.0, 0.16)), "selected");
        assert!(fills.contains(&Rgba::rgba(1.0, 1.0, 1.0, 0.06)), "hovered");
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
        for label in TABS {
            let n = texts(&f).into_iter().filter(|(t, _, _)| t == label).count();
            assert_eq!(n, 1, "tab {label} drawn exactly once");
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

    fn glyph_rects(f: &Frame) -> Vec<Rect> {
        // Tab boxes are the measured label rects (inset by TAB_PAD) widened back
        // to the full pill rect, in x order.
        let mut rects: Vec<Rect> = TABS
            .iter()
            .filter_map(|label| find_text(f, label).map(|(r, _)| r))
            .collect();
        rects.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());
        rects.iter_mut().for_each(|r| {
            r.x -= TAB_PAD;
            r.w += TAB_PAD * 2.0;
        });
        rects
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
    fn body_corners_are_uniform_ios_style() {
        // The reference pill carries the same continuous-corner radius on all
        // four corners: measured on the reference, the top rows taper inward
        // exactly like the bottom rows. Not a capsule (r = h/2), not a
        // square-topped tab.
        let Node::RoundRect { radii, .. } = &collapsed().scene.nodes[0] else {
            panic!()
        };
        assert_eq!(*radii, CornerRadii::uniform(PILL_RADIUS_REF));
        assert_eq!(radii.top_left, PILL_RADIUS_REF, "top-left must be rounded");
        assert_eq!(
            radii.top_right, PILL_RADIUS_REF,
            "top-right must be rounded"
        );
        assert_eq!(radii.bottom_left, PILL_RADIUS_REF);
        assert_eq!(radii.bottom_right, PILL_RADIUS_REF);
        assert_eq!(PILL_RADIUS_REF, 10.0);
    }

    #[test]
    fn expanded_body_keeps_its_corners_rounded_too() {
        // The panel morph carries the same uniform rounding: no corner ever
        // goes square at any point of the morph.
        for open in [0.0f32, 0.5, 1.0] {
            let f = build(&view_sized(width_at(open), height_at(open)));
            let Node::RoundRect { radii, .. } = &f.scene.nodes[0] else {
                panic!()
            };
            assert_eq!(radii.top_left, radii.bottom_left, "at {open}");
            assert_eq!(radii.top_right, radii.bottom_right, "at {open}");
            assert!(radii.top_left > 0.0, "top-left rounded at {open}");
            assert!(radii.bottom_right > 0.0);
        }
    }

    // ----------------------------------------------------------------- panel

    #[test]
    fn panel_frame_contains_header_title_in_reference_style() {
        let f = panel();
        let (rect, style) = find_text(&f, APP_TITLE).expect("panel must contain the header title");
        assert_eq!(style.size, 20.0, "title is 20 px");
        assert_eq!(style.weight, Weight::Semibold, "title is semibold");
        assert_eq!(style.color, Rgba::WHITE, "title is white");
        assert!((rect.y - HEADER_Y).abs() < 1e-4, "header row sits at y=16");
        assert!((rect.x - PANEL_PAD).abs() < 1e-4, "header is left-aligned");
    }

    #[test]
    fn panel_frame_contains_footer_clock_with_tabular_numerals() {
        let f = panel();
        let (rect, style) =
            find_text(&f, FOOTER_CLOCK).expect("panel must contain the footer clock");
        assert!(style.tabular, "the clock must use tabular figures");
        assert_eq!(style.size, 12.0);
        assert!((rect.x - PANEL_PAD).abs() < 1e-4, "footer is bottom-left");
        assert!(
            (rect.max_y() - (PANEL_H_REF - FOOTER_BOTTOM_PAD)).abs() < 1e-4,
            "footer is {FOOTER_BOTTOM_PAD} px off the bottom edge: {rect:?}"
        );
        assert!((rect.w - measure(FOOTER_CLOCK, &style)).abs() < 1e-4);
    }

    #[test]
    fn panel_frame_contains_five_tabs_with_8px_gaps() {
        let f = panel();
        let rects = glyph_rects(&f);
        assert_eq!(rects.len(), 5, "expected five tabs: {:#?}", f.scene.nodes);
        for w in rects.windows(2) {
            assert!(w[0].max_x() <= w[1].x + 1e-4, "tabs overlap: {w:?}");
            assert!(
                (w[1].x - w[0].max_x() - TAB_GAP).abs() < 1e-3,
                "gap must be 8 px: {w:?}"
            );
        }
        let used: f32 = rects.iter().map(|r| r.w).sum::<f32>() + TAB_GAP * 4.0;
        assert!(
            used <= PANEL_W_REF - PANEL_PAD * 2.0 + 1e-3,
            "tab row overflows the padded frame: {used}"
        );
    }

    #[test]
    fn tab_strip_is_centered() {
        let rects = glyph_rects(&panel());
        let used: f32 = rects.iter().map(|r| r.w).sum::<f32>() + TAB_GAP * 4.0;
        let avail = PANEL_W_REF - PANEL_PAD * 2.0;
        let want_left = PANEL_PAD + (avail - used) / 2.0;
        assert!(
            (rects[0].x - want_left).abs() < 1e-3,
            "{:?} vs {want_left}",
            rects[0]
        );
    }

    #[test]
    fn tab_labels_are_measured_not_guessed() {
        let f = panel();
        for label in TABS {
            let (rect, _) =
                find_text(&f, label).unwrap_or_else(|| panic!("missing tab label {label}"));
            let want = measure(label, &TAB_LABEL);
            assert!(want > 0.0, "{label} measured zero width");
            assert!((rect.w - want).abs() < 1e-4, "{label}: {rect:?}");
        }
    }

    /// A Stats-tab panel: the Media tab draws real content, so the placeholder
    /// row assertions belong to a placeholder tab.
    fn stats_panel() -> Frame {
        let mut v = view_sized(PANEL_W_REF, PANEL_H_REF);
        v.active_tab = 1;
        build(&v)
    }

    #[test]
    fn panel_content_has_three_placeholder_rows() {
        let f = stats_panel();
        let rows = row_rects(&f);
        assert_eq!(rows.len(), CONTENT_ROWS, "{:#?}", f.scene.nodes);
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
        let f = stats_panel();
        let rows = row_rects(&f);
        assert_eq!(rows.len(), CONTENT_ROWS);
        let inner = PANEL_W_REF - PANEL_PAD * 2.0;
        for n in all_nodes(&f) {
            if let Node::RoundRect { rect, radii, fill } = n {
                // Content rows only: the tab pill is also a rounded rect and
                // carries the selection fill instead.
                if (rect.w - inner).abs() < 1e-3 {
                    assert_eq!(fill, Rgba::rgba(1.0, 1.0, 1.0, 0.04), "4 % white: {rect:?}");
                    assert!(
                        (radii.top_left - ROW_RADIUS).abs() < 1e-4,
                        "reference row radius is 8: {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn content_sits_between_the_tab_strip_and_the_footer() {
        let f = panel();
        let tabs_bottom = glyph_rects(&f)
            .iter()
            .map(|r| r.max_y())
            .fold(0.0f32, f32::max);
        let (footer, _) = find_text(&f, FOOTER_CLOCK).expect("footer");
        for r in row_rects(&f) {
            assert!(r.y >= tabs_bottom - 1e-3, "row overlaps the tabs: {r:?}");
            assert!(
                r.max_y() <= footer.y + 1e-3,
                "row overlaps the footer: {r:?}"
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
    fn radius_lerps_10_to_24_across_the_morph() {
        // All four corners carry the same radius (iOS style) and track
        // openness together: the corners hold the same value as each other at
        // every point of the morph.
        let mut prev = -1.0;
        for i in 0..=100 {
            let open = i as f32 / 100.0;
            let f = build(&view_sized(width_at(open), PANEL_H_REF));
            let Node::RoundRect { radii, .. } = &f.scene.nodes[0] else {
                panic!()
            };
            let want = PILL_RADIUS_REF + (PANEL_RADIUS_REF - PILL_RADIUS_REF) * open;
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
            assert_eq!(radii.top_left, radii.bottom_left, "corners split at {open}");
            assert_eq!(
                radii.top_right, radii.bottom_right,
                "corners split at {open}"
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
    fn header_box_is_exactly_the_measured_run() {
        let mut style = TextStyle::title();
        style.align = Align::Start;
        let (rect, _) = find_text(&panel(), APP_TITLE).unwrap();
        assert!((rect.w - measure(APP_TITLE, &style)).abs() < 1e-4);
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
}
