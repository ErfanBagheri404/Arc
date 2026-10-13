//! Settings tab: the toggles the tray menu's "Settings…" opens.
//!
//! Pure builder like every other tab: `content` emits nodes, the row rects are
//! exported so hit-testing reads exactly what was drawn. Two rows today —
//! the idle pet on/off and the pet picker — so no scrolling logic is worth
//! writing yet.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::ui::{layout, line_height, measure, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// Height of one settings row.
pub const ROW_H: f32 = 34.0;
/// Gap above the first row.
pub const TOP_PAD: f32 = 8.0;
/// Gap between rows.
pub const ROW_GAP: f32 = 8.0;

/// The grey track every row sits on.
const ROW_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);
/// The iOS-style green an enabled toggle wears.
const ON: Rgba = Rgba::rgb(0.30, 0.75, 0.40);
/// The dim track a disabled toggle wears.
const OFF: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.14);

/// Row 0: the pet on/off row.
pub fn toggle_rect(fw: f32, top: f32) -> Rect {
    Rect::new(PANEL_PAD, top + TOP_PAD, fw - PANEL_PAD * 2.0, ROW_H)
}

/// Row 1: the pet picker row (no-op visual when the pet is off).
pub fn picker_rect(fw: f32, top: f32) -> Rect {
    Rect::new(
        PANEL_PAD,
        top + TOP_PAD + ROW_H + ROW_GAP,
        fw - PANEL_PAD * 2.0,
        ROW_H,
    )
}

/// Which row `(x, y)` lands on, or `None`. Mirrors the two rects above.
#[must_use]
pub fn row_hit(fw: f32, top: f32, x: f32, y: f32) -> Option<usize> {
    if toggle_rect(fw, top).contains(x, y) {
        Some(0)
    } else if picker_rect(fw, top).contains(x, y) {
        Some(1)
    } else {
        None
    }
}

/// The Settings tab's content, laid out under `top`.
pub fn content(
    fw: f32,
    fh: f32,
    top: f32,
    pet_on: bool,
    pet_name: Option<&str>,
    pet_count: usize,
) -> Vec<Node> {
    let mut out = Vec::new();
    out.extend(row(fw, fh, toggle_rect(fw, top), "Pet when idle"));
    out.push(toggle(fw, fh, toggle_rect(fw, top), pet_on));
    if pet_on {
        let label = match pet_name {
            Some(name) => format!("Pet · {name}"),
            None if pet_count == 0 => "Pet · none installed".to_string(),
            None => "Pet".to_string(),
        };
        out.extend(row(fw, fh, picker_rect(fw, top), &label));
        if pet_count > 1 {
            out.push(chevrons(fw, fh, picker_rect(fw, top)));
        }
    }
    out
}

/// A settings row: a full-width track plus its left-aligned label.
fn row(fw: f32, fh: f32, r: Rect, label: &str) -> Vec<Node> {
    let style = TextStyle {
        size: 12.0,
        ..Default::default()
    };
    let h = line_height(&style);
    vec![
        Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(9.0),
            fill: ROW_TRACK,
        },
        Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(r.x + 10.0, r.y + (r.h - h) * 0.5, (r.w - 20.0).max(0.0), h),
            ),
            text: label.to_string(),
            style,
        },
    ]
}

/// The iOS-style switch on a row's right edge: track plus knob, knob slid when
/// on. Purely geometric — the renderer already draws both parts as round rects.
fn toggle(fw: f32, fh: f32, r: Rect, on: bool) -> Node {
    let (w, h) = (36.0, 20.0);
    let track = Rect::new(r.max_x() - w - 10.0, r.y + (r.h - h) * 0.5, w, h);
    let knob = 16.0;
    let kx = if on {
        track.max_x() - knob - 2.0
    } else {
        track.x + 2.0
    };
    let group = Node::Group {
        rect: put(fw, fh, track),
        children: vec![
            Node::RoundRect {
                rect: put(fw, fh, track),
                radii: CornerRadii::uniform(h / 2.0),
                fill: if on { ON } else { OFF },
            },
            Node::RoundRect {
                rect: put(fw, fh, Rect::new(kx, track.y + 2.0, knob, knob)),
                radii: CornerRadii::uniform(knob / 2.0),
                fill: Rgba::WHITE,
            },
        ],
    };
    group
}

/// The picker's affordance: a thin "‹ ›" hint on the row's right edge, drawn
/// as text so it inherits the tab's font and needs no glyph art.
fn chevrons(fw: f32, fh: f32, r: Rect) -> Node {
    let style = TextStyle {
        size: 13.0,
        ..Default::default()
    };
    let h = line_height(&style);
    let w = measure("‹ ›", &style).max(1.0);
    Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(r.max_x() - w - 12.0, r.y + (r.h - h) * 0.5, w, h),
        ),
        text: "‹ ›".to_string(),
        style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_stack_without_touching() {
        let a = toggle_rect(640.0, 60.0);
        let b = picker_rect(640.0, 60.0);
        assert!(a.max_y() <= b.y + 0.01);
        assert!(b.max_x() <= 640.0 - PANEL_PAD + 0.01);
    }

    #[test]
    fn hit_testing_matches_the_drawn_rows() {
        assert_eq!(row_hit(640.0, 60.0, 300.0, 70.0), Some(0));
        assert_eq!(row_hit(640.0, 60.0, 300.0, 110.0), Some(1));
        assert_eq!(row_hit(640.0, 60.0, 300.0, 400.0), None);
    }

    #[test]
    fn pet_off_shows_only_the_toggle() {
        let nodes = content(640.0, 400.0, 60.0, false, None, 0);
        // One row track + its label + the toggle group.
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn pet_on_names_the_pet_and_offers_a_picker() {
        let nodes = content(640.0, 400.0, 60.0, true, Some("Nukey"), 2);
        assert!(nodes
            .iter()
            .any(|n| matches!(n, Node::Text { text, .. } if text == "Pet · Nukey")));
        // With a second pet installed the chevrons show.
        assert!(nodes
            .iter()
            .any(|n| matches!(n, Node::Text { text, .. } if text == "‹ ›")));
    }

    #[test]
    fn a_single_pet_hides_the_picker_hint() {
        let nodes = content(640.0, 400.0, 60.0, true, Some("Solo"), 1);
        assert!(!nodes
            .iter()
            .any(|n| matches!(n, Node::Text { text, .. } if text == "‹ ›")));
    }
}
