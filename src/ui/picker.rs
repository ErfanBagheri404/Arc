//! Color picker tab: a live hex readout, a swatch, and the sampled trail.
//!
//! Pure builder, same contract as `ui::stats`: `content(fw, fh, top, picker)`
//! returns scene nodes in panel space and reads nothing else.
//!
//! CUT: magnifier. The readout is the output; a zoomed preview of one pixel
//! needs Desktop Duplication and a GPU texture to crop and scale, which is a
//! lot of machinery for a value the hex already states exactly.
//!
//! CUT: multi-monitor. `GetPixel` on the desktop DC samples the primary
//! monitor; routing between monitors is a table nobody asked for.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::picker::{Color, Picker};
use crate::ui::{layout, line_height, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// Height of the readout row: swatch plus hex.
pub const READOUT_H: f32 = 44.0;
/// Gap between the readout and the trail.
pub const READOUT_GAP: f32 = 12.0;
/// Height of one trail swatch.
pub const SWATCH_H: f32 = 22.0;
/// Gap between trail swatches.
pub const SWATCH_GAP: f32 = 6.0;
/// How many trail swatches fit per row.
pub const SWATCHES_PER_ROW: usize = 8;

/// A sampled color as linear 0–1 channels for the scene.
fn rgba_of(c: Color) -> Rgba {
    let (r, g, b) = c.channels();
    Rgba::rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

/// The grey track the readout sits on.
const READOUT_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);

/// The readout row's rect, directly under the tab strip.
pub fn readout_rect(fw: f32, top: f32) -> Rect {
    Rect::new(PANEL_PAD, top, fw - PANEL_PAD * 2.0, READOUT_H)
}

/// Each trail swatch's rect, oldest first. Drawing and hit-testing both read
/// this, so a click can never land on a different swatch than the one drawn.
pub fn swatch_rects(fw: f32, top: f32, count: usize) -> Vec<Rect> {
    let y0 = top + READOUT_H + READOUT_GAP;
    let inner = fw - PANEL_PAD * 2.0;
    let per = inner / SWATCHES_PER_ROW as f32;
    (0..count)
        .map(|i| {
            let col = i % SWATCHES_PER_ROW;
            let row = i / SWATCHES_PER_ROW;
            Rect::new(
                PANEL_PAD + per * col as f32,
                y0 + (SWATCH_H + SWATCH_GAP) * row as f32,
                per,
                SWATCH_H,
            )
        })
        .collect()
}

/// The Color picker tab's content, laid out under `top`.
pub fn content(fw: f32, fh: f32, top: f32, picker: &Picker) -> Vec<Node> {
    let mut out = Vec::new();
    let r = readout_rect(fw, top);
    if r.y >= fh || r.max_y() <= 0.0 {
        return out;
    }
    out.push(Node::RoundRect {
        rect: put(fw, fh, r),
        radii: CornerRadii::uniform(9.0),
        fill: READOUT_TRACK,
    });

    let current = picker.current();
    let hex = current.map(Color::hex).unwrap_or_else(|| "—".to_string());
    let style = TextStyle::numeric(14.0);
    let h = line_height(&style);
    let text_x = r.x + 12.0;
    let text_w = (r.max_x() - PANEL_PAD * 0.5 - text_x).max(0.0);
    if text_w > 8.0 {
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(text_x, r.y + (r.h - h) * 0.5, text_w, h)),
            text: hex,
            style,
        });
    }

    // The swatch: a small square of the sampled color, right of the readout.
    if let Some(c) = current {
        let swatch = Rect::new(r.max_x() - 12.0 - 20.0, r.y + (r.h - 20.0) * 0.5, 20.0, 20.0);
        out.push(Node::RoundRect {
            rect: put(fw, fh, swatch),
            radii: CornerRadii::uniform(5.0),
            fill: rgba_of(c),
        });
    }

    let trail = picker.trail();
    for (i, (rect, color)) in swatch_rects(fw, top, trail.len())
        .into_iter()
        .zip(trail.iter())
        .enumerate()
    {
        if rect.y >= fh || rect.max_y() <= 0.0 {
            break;
        }
        out.push(Node::RoundRect {
            rect: put(fw, fh, rect),
            radii: CornerRadii::uniform(4.0),
            fill: rgba_of(*color),
        });
        let _ = i;
    }
    out
}

/// Which trail swatch panel-space `(x, y)` hits, if any.
pub fn swatch_hit(fw: f32, top: f32, count: usize, x: f32, y: f32) -> Option<usize> {
    swatch_rects(fw, top, count)
        .into_iter()
        .position(|r| x >= r.x && x <= r.max_x() && y >= r.y && y <= r.max_y())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::picker::TRAIL;

    fn picker_with(n: usize) -> Picker {
        let p = Picker::new();
        {
            let mut inner = p.inner.lock().unwrap();
            inner.trail = (0..n).map(|i| Color(i as u32)).collect();
            inner.current = Some(Color(0x123456));
        }
        p
    }

    #[test]
    fn readout_sits_under_the_tab_strip_and_fits_inside_the_panel() {
        let r = readout_rect(1_000.0, 60.0);
        assert!(r.y >= 60.0);
        assert!(r.x >= PANEL_PAD - 0.01);
        assert!(r.max_x() <= 1_000.0 - PANEL_PAD + 0.01);
    }

    #[test]
    fn swatches_wrap_at_eight_per_row_and_stay_inside_the_panel() {
        let s = swatch_rects(1_000.0, 60.0, 20);
        assert_eq!(s.len(), 20);
        for w in s.windows(2) {
            assert!(w[1].x >= w[0].x - 0.01 || w[1].y > w[0].y, "wraps");
            assert!(w[0].x >= PANEL_PAD - 0.01);
            assert!(w[0].max_x() <= 1_000.0 - PANEL_PAD + 0.01);
        }
    }

    #[test]
    fn a_panel_with_a_sample_draws_more_nodes_than_one_without() {
        let empty = content(1_000.0, 400.0, 60.0, &Picker::new());
        let full = content(1_000.0, 400.0, 60.0, &picker_with(3));
        assert!(full.len() > empty.len());
    }

    #[test]
    fn swatches_stop_at_the_panel_bottom_so_nothing_draws_past_the_footer() {
        let nodes = content(1_000.0, 200.0, 90.0, &picker_with(40));
        assert!(nodes.iter().all(|n| n.rect().max_y() <= 200.0 + layout::BOUNDS_EPSILON));
    }

    #[test]
    fn swatch_hit_resolves_the_drawn_rect() {
        let s = swatch_rects(1_000.0, 60.0, 3);
        let hit = swatch_hit(1_000.0, 60.0, 3, s[1].x + 1.0, s[1].y + 1.0);
        assert_eq!(hit, Some(1));
        assert_eq!(swatch_hit(1_000.0, 60.0, 3, 5.0, 5.0), None);
    }

    #[test]
    fn a_trail_longer_than_one_row_still_fits_inside_the_panel() {
        let s = swatch_rects(1_000.0, 60.0, TRAIL * 2);
        assert!(s.iter().all(|r| r.max_x() <= 1_000.0 - PANEL_PAD + 0.01));
    }
}