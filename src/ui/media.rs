//! The Media tab: album art, transport, progress — the flagship surface.
//!
//! Pure builder, same contract as the rest of `ui`: `content(fw, fh, state)`
//! returns scene nodes from a `services::media::MediaState`. No Win32, no I/O.
//! The service owns the worker thread and the decoded art handle; this module
//! only arranges what that state describes.
//!
//! Layout mirrors the reference: a 64 px art square at the left, a two-line
//! title/artist block beside it, tabular progress beneath, then the transport
//! glyph row. When nothing is playing the art slot becomes a subtle placeholder.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Align, Node, TextStyle, Weight};
use crate::services::media::MediaState;
use crate::ui::layout;
use crate::ui::{line_height, measure};

/// Album art edge in logical pixels (reference: `64`).
pub const ART: f32 = 64.0;
/// Gap between the art square and the text column.
const ART_GAP: f32 = 14.0;
/// Play/pause glyph size.
const TRANSPORT: f32 = 22.0;
/// Gap between transport buttons.
const TRANSPORT_GAP: f32 = 22.0;
/// Radius used for the progress track.
const BAR_RADIUS: f32 = 3.0;
/// Leading between the media column's stacked rows (title → subtitle, and each
/// row → the next). Half a small line box, rounded: 8 px separates glyph ink
/// where the old 2 px stacked ascenders on descenders (`Dorcci…` touching its
/// subtitle).
const TEXT_LEAD: f32 = 8.0;
/// Gap between a clock label and the bar on the shared progress row.
const BAR_SIDE_GAP: f32 = 10.0;
/// Dim colour for disabled transport buttons (shuffle/repeat).
const DIM: Rgba = Rgba {
    r: 0.42,
    g: 0.42,
    b: 0.46,
    a: 1.0,
};

/// Default album-art accent, used until the cover decodes.
const FALLBACK_ACCENT: Rgba = Rgba::rgb(0.8, 0.2, 0.3);

/// Format a `Duration` as `m:ss` or `h:mm:ss` when it exceeds an hour.
fn fmt(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The media content block, laid out under `top` (the tab strip's bottom).
pub fn content(fw: f32, fh: f32, top: f32, state: &MediaState) -> Vec<Node> {
    if !state.has_session() {
        return empty(fw, fh);
    }
    let y0 = body_top(fh, top);

    let accent = state.accent.unwrap_or(FALLBACK_ACCENT);
    let mut out = Vec::new();

    // 1. Album art. When the handle is `None` the renderer draws a placeholder
    //    fill, so the square is always present and the layout never jumps.
    let art = Rect::new(crate::ui::PANEL_PAD, y0, ART, ART);
    out.push(Node::Image {
        rect: put(fw, fh, art),
        handle: state
            .art
            .map(crate::core::scene::ImageHandle)
            .unwrap_or_default(),
        radii: CornerRadii::uniform(8.0),
    });

    // 2. Title / artist column to the right of the art.
    let tx = art.x + ART + ART_GAP;
    let ty = art.y;
    let tw = (fw - tx - crate::ui::PANEL_PAD).max(0.0);

    let title_style = TextStyle::title();
    let title_h = line_height(&title_style);
    out.push(Node::Text {
        rect: put(fw, fh, Rect::new(tx, ty, tw, title_h)),
        text: state.title.clone(),
        style: title_style,
    });

    let sub_style = TextStyle::subtitle(accent);
    // Leading is half a line box, not 2 px: a title on its line box leaves glyph
    // ink a couple of px past the box bottom, so a 2 px gap stacked the
    // subtitle's ascenders straight onto the title's descenders.
    let sy = ty + title_h + TEXT_LEAD;
    let sub_h = line_height(&sub_style);
    out.push(Node::Text {
        rect: put(fw, fh, Rect::new(tx, sy, tw, sub_h)),
        text: state.artist.clone(),
        style: sub_style,
    });

    // 3. Progress on ONE row: elapsed clock, bar, remaining clock. Stacking the
    //    clocks on their own row above a full-width bar needed 124 px of
    //    column and the band between the tab strip and the footer is only 120.
    //    Flanking the bar (Apple Music) fits in 112 and keeps the type sizes.
    let (elapsed_r, bar_r, remain_slot) = progress_row(fw, y0);
    let clock_style = TextStyle::numeric(11.0);
    let progress = state.progress().unwrap_or(0.0);
    let pos = fmt(state.position);
    let total_secs = state.duration.map(|d| d.as_secs() as f32).unwrap_or(0.0);
    let remain = fmt_total(total_secs, state.duration);

    out.push(Node::Text {
        rect: put(fw, fh, elapsed_r),
        text: pos,
        style: clock_style.clone(),
    });
    // Hug the right padding: the SLOT is fixed (so the bar never shifts), and
    // the text is left-aligned inside it, so shrink the rect to the glyphs'
    // width and anchor it to the slot's right edge.
    let remain_w = measure(&remain, &clock_style);
    out.push(Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(
                remain_slot.max_x() - remain_w.min(remain_slot.w),
                remain_slot.y,
                remain_w.min(remain_slot.w),
                remain_slot.h,
            ),
        ),
        text: remain,
        style: clock_style,
    });

    out.push(Node::Bar {
        rect: put(fw, fh, bar_r),
        progress,
        track: Rgba::rgba(1.0, 1.0, 1.0, 0.12),
        fill: accent,
        radii: CornerRadii::uniform(BAR_RADIUS),
    });

    // 4. Transport glyph row, drawn from the same rects the click handler uses.
    for (rect, cmd) in transport(fw, fh, top) {
        let name = match cmd {
            crate::services::media::Command::Previous => "prev",
            crate::services::media::Command::Next => "next",
            crate::services::media::Command::PlayPause => {
                if state.playing {
                    "pause"
                } else {
                    "play"
                }
            }
            crate::services::media::Command::Seek(_) => "seek",
        };
        out.push(Node::Glyph {
            rect: put(fw, fh, rect),
            name,
            color: Rgba::WHITE,
        });
    }

    out
}

/// Top of the media block: the tab strip's bottom, with the block centred in
/// the band between it and the footer. Shared by drawing and hit-testing.
fn body_top(fh: f32, top: f32) -> f32 {
    let band = (fh - crate::ui::FOOTER_BOTTOM_PAD - top).max(0.0);
    // The real column: art row + one progress row + transport — centring on a
    // taller estimate pushed the transport past the footer.
    let clock_h = line_height(&TextStyle::numeric(11.0));
    let body_h = ART + TEXT_LEAD + clock_h.max(4.0) + TEXT_LEAD + TRANSPORT;
    top + (band - body_h).max(0.0) * 0.5
}

/// Widest clock the row can ever show (`−10:00:00`). Reserving this instead of
/// measuring the live string keeps the bar's x/width — and therefore the seek
/// hit-test — identical on every tick, and sizes the slot with U+2212 MINUS,
/// which is wider than the ASCII hyphen a guessed literal would use (that
/// clipped the label to `-4...`).
const CLOCK_SLOT_REF: &str = "−10:00:00";

/// One progress ROW: elapsed clock, bar, remaining clock. Shared by drawing
/// (`content`) and the seek hit-test (`bar_rect`), so the clickable bar is
/// the drawn bar. Geometry depends only on `fw`/`y0`, never on the track time.
fn progress_row(fw: f32, y0: f32) -> (Rect, Rect, Rect) {
    let clock_style = TextStyle::numeric(11.0);
    let clock_h = line_height(&clock_style);
    let slot = measure(CLOCK_SLOT_REF, &clock_style);
    let y = y0 + ART + TEXT_LEAD;
    let bar_x = crate::ui::PANEL_PAD + slot + BAR_SIDE_GAP;
    let bar_end = fw - crate::ui::PANEL_PAD - slot - BAR_SIDE_GAP;
    let bar_w = (bar_end - bar_x).max(0.0);
    // Bar vertically centred on the clock line box.
    let bar_y = y + (clock_h - 4.0).max(0.0) * 0.5;
    let elapsed = Rect::new(crate::ui::PANEL_PAD, y, slot, clock_h);
    let remain = Rect::new(fw - crate::ui::PANEL_PAD - slot, y, slot, clock_h);
    (elapsed, Rect::new(bar_x, bar_y, bar_w, 4.0), remain)
}

/// Panel-space rect of the progress bar, the one clickable seek target.
/// Delegates to `progress_row`, so the clickable bar IS the drawn bar: the
/// geometry never reads the live clock strings, so it cannot shift between the
/// frame the user clicked and the frame the seek is applied in.
pub fn bar_rect(fw: f32, fh: f32, top: f32) -> Rect {
    let y0 = body_top(fh, top);
    progress_row(fw, y0).1
}

/// Panel-space rectangles of the transport buttons, in layout order.
///
/// Shared by drawing and hit-testing so a click never lands on a different
/// button than the one that was drawn — the same rule `tab_boxes` follows.
pub fn transport(fw: f32, fh: f32, top: f32) -> Vec<(Rect, crate::services::media::Command)> {
    use crate::services::media::Command;
    // Below the progress row, not `body_top + ART`: the progress row moved under
    // the art, so the old anchor parked the buttons on top of the clocks.
    let controls_y = progress_row(fw, body_top(fh, top)).1.max_y() + TEXT_LEAD;
    let cmds = [Command::Previous, Command::PlayPause, Command::Next];
    let n = cmds.len() as f32;
    let total_w = n * TRANSPORT + (n - 1.0) * TRANSPORT_GAP;
    let x0 = (fw - total_w) / 2.0;
    cmds.iter()
        .enumerate()
        .map(|(i, cmd)| {
            let x = x0 + i as f32 * (TRANSPORT + TRANSPORT_GAP);
            (Rect::new(x, controls_y, TRANSPORT, TRANSPORT), *cmd)
        })
        .collect()
}

/// Helper: remaining-total label. The reference shows `−3:43` for the total.
fn fmt_total(total_secs: f32, dur: Option<std::time::Duration>) -> String {
    match dur {
        Some(d) => format!("−{}", fmt(d)),
        None => format!("{:.0}", total_secs),
    }
}

/// Empty state: a dimmed placeholder line telling the user nothing is playing.
fn empty(fw: f32, fh: f32) -> Vec<Node> {
    let style = TextStyle {
        mono: false,
        size: 14.0,
        weight: Weight::Regular,
        color: DIM,
        tabular: false,
        align: Align::Center,
    };
    let msg = "Nothing playing";
    let w = measure(msg, &style);
    let h = line_height(&style);
    vec![Node::Text {
        rect: put(fw, fh, Rect::new((fw - w) / 2.0, (fh - h) / 2.0, w, h)),
        text: msg.to_string(),
        style,
    }]
}

/// Clamp a rect into the frame, exactly as `ui::build` does.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::media::MediaState;
    use crate::ui::layout::{PANEL_H_REF, PANEL_W_REF};

    fn playing() -> MediaState {
        MediaState {
            title: "Dorcci - Ghatle Amd [Piano Version] (Slowed + Reverb)".into(),
            artist: "Slowed Vibe | Slowed + Reverb Music | Rap | Remix".into(),
            source: "Spotify.exe".into(),
            playing: true,
            position: std::time::Duration::from_secs(70),
            duration: Some(std::time::Duration::from_secs(223)),
            ..Default::default()
        }
    }

    /// The media column never overlaps and never escapes the frame: title ->
    /// subtitle, then ONE progress row (elapsed clock, bar, remaining clock),
    /// then transport. The two clocks are side-by-side on the same line, so
    /// the check names each role instead of scanning emission order.
    #[test]
    fn media_column_rows_do_not_touch() {
        let top = crate::ui::HEADER_Y
            + line_height(&TextStyle::title())
            + crate::ui::HEADER_GAP
            + crate::ui::TAB_H;
        let st = playing();
        let nodes = content(PANEL_W_REF, PANEL_H_REF, top, &st);
        let text = |want: &str| {
            nodes
                .iter()
                .find_map(|n| match n {
                    Node::Text { rect, text, .. } if text == want => Some(*rect),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no text node {want:?}"))
        };
        let title = text(&st.title);
        let sub = text(&st.artist);
        let elapsed = text("1:10");
        let remain = text("−3:43");
        // Title above subtitle with breathing room (glyph ink overflows the
        // line box, so this is gap between boxes, not between ink).
        assert!(sub.y - (title.y + title.h) >= 4.0, "subtitle crowds title");
        // Clocks on one row below the subtitle.
        assert!(
            (elapsed.y - remain.y).abs() < 1e-3,
            "clocks off-row: {} vs {}",
            elapsed.y,
            remain.y
        );
        assert!(
            elapsed.y - (sub.y + sub.h) >= 0.0,
            "clock row overlaps subtitle: {} < {}",
            elapsed.y,
            sub.y + sub.h
        );
        // Bar between the clocks, on the clock line.
        let bar = crate::ui::media::bar_rect(PANEL_W_REF, PANEL_H_REF, top);
        assert!(bar.x >= elapsed.x + elapsed.w, "bar crowds elapsed: {bar:?}");
        assert!(
            bar.x + bar.w <= remain.x + 0.5,
            "bar crowds remaining: {bar:?}"
        );
        assert!(
            (bar.y - elapsed.y).abs() <= (elapsed.h - bar.h).max(0.0) + 0.5,
            "bar off the clock line: {bar:?} vs {elapsed:?}"
        );
        // Transport below the progress row.
        for (r, _) in crate::ui::media::transport(PANEL_W_REF, PANEL_H_REF, top) {
            assert!(r.y >= bar.y + bar.h, "transport overlaps bar: {r:?}");
        }
        // Whole column inside the frame with footer clearance.
        let bottom = crate::ui::media::transport(PANEL_W_REF, PANEL_H_REF, top)
            .iter()
            .map(|(r, _)| r.y + r.h)
            .fold(0.0f32, f32::max);
        assert!(
            bottom + crate::ui::FOOTER_BOTTOM_PAD <= PANEL_H_REF + 0.5,
            "column escapes panel: {bottom}"
        );
    }

    /// Every media node must sit inside the panel's padded box.
    #[test]
    fn media_nodes_respect_panel_padding() {
        let top = crate::ui::HEADER_Y
            + line_height(&TextStyle::title())
            + crate::ui::HEADER_GAP
            + crate::ui::TAB_H;
        let pad = crate::ui::PANEL_PAD;
        for n in content(PANEL_W_REF, PANEL_H_REF, top, &playing()) {
            let r = n.rect();
            assert!(r.x >= pad - 0.5, "node escapes left pad: {r:?}");
            assert!(
                r.x + r.w <= PANEL_W_REF - pad + 0.5,
                "node escapes right pad: {r:?}"
            );
        }
    }
}
