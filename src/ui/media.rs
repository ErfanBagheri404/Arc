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
        handle: state.art.map(crate::core::scene::ImageHandle).unwrap_or_default(),
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
    let sy = ty + title_h + 2.0;
    out.push(Node::Text {
        rect: put(fw, fh, Rect::new(tx, sy, tw, line_height(&sub_style))),
        text: state.artist.clone(),
        style: sub_style,
    });

    // 3. Progress: `0:14 / −3:43` tabular, then the accent-filled bar.
    let bar_y = art.y + ART - 6.0;
    let progress = state.progress().unwrap_or(0.0);
    let pos = fmt(state.position);
    let total = state.duration.map(|d| d.as_secs() as f32).unwrap_or(0.0);
    let clock = format!("{pos} / {}", fmt_total(total, state.duration));
    let clock_style = TextStyle::numeric(12.0);
    let cw = measure(&clock, &clock_style);
    let ch = line_height(&clock_style);
    out.push(Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(tx, bar_y - ch - 2.0, cw, ch),
        ),
        text: clock,
        style: clock_style,
    });

    let bar = bar_rect(fw, fh, top);
    out.push(Node::Bar {
        rect: put(fw, fh, bar),
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
                if state.playing { "pause" } else { "play" }
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
    let body_h = ART + 30.0; // art row + transport row
    top + (band - body_h).max(0.0) * 0.5
}

/// Panel-space rect of the progress bar, the one clickable seek target.
pub fn bar_rect(fw: f32, fh: f32, top: f32) -> Rect {
    let y0 = body_top(fh, top);
    let tx = crate::ui::PANEL_PAD + ART + ART_GAP;
    let tw = (fw - tx - crate::ui::PANEL_PAD).max(0.0);
    Rect::new(tx, y0 + ART - 6.0, tw, 4.0)
}

/// Panel-space rectangles of the transport buttons, in layout order.
///
/// Shared by drawing and hit-testing so a click never lands on a different
/// button than the one that was drawn — the same rule `tab_boxes` follows.
pub fn transport(fw: f32, fh: f32, top: f32) -> Vec<(Rect, crate::services::media::Command)> {
    use crate::services::media::Command;
    let controls_y = body_top(fh, top) + ART + 10.0;
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
        rect: put(
            fw,
            fh,
            Rect::new((fw - w) / 2.0, (fh - h) / 2.0, w, h),
        ),
        text: msg.to_string(),
        style,
    }]
}

/// Clamp a rect into the frame, exactly as `ui::build` does.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}