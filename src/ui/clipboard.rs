//! Clipboard tab: newest-first history rows, click to restore, clear-all.
//!
//! Pure builder, same contract as `ui::stats`: `content(fw, fh, top, entries)`
//! returns scene nodes in panel space and reads nothing else.
//!
//! The panel doubles as the consent surface. With capture off (the default)
//! there is nothing to list, so the tab asks for consent instead of showing an
//! empty list — the opt-in has to be answered where the user looks, not buried
//! in a settings page they have not opened. Capture + clear-all are one row each
//! above the list so consent cannot be confused for a broken empty state.
//!
//! CUT: search. Typing into the island needs a keyboard capture path that does
//! not exist yet; Phase 7's terminal work adds it and search can share the
//! hook. A list is genuinely useful without it, so this ships without.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::clipboard::Entry;
use crate::ui::{layout, line_height, measure, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// Height of one history row.
pub const ROW_H: f32 = 34.0;
/// Gap between history rows.
pub const ROW_GAP: f32 = 8.0;
/// Height of the consent / capture control row.
pub const CTL_H: f32 = 34.0;
/// Gap above the first control row.
pub const CTL_TOP_PAD: f32 = 8.0;
/// Gap between two control rows.
pub const CTL_GAP: f32 = 8.0;
/// How much preview text is held per row, in characters.
pub const PREVIEW_CHARS: usize = 44;

/// The amber accent used for the "off" state's call to action.
const CTL_AMBER: Rgba = Rgba::rgb(0.85, 0.65, 0.20);
/// The grey track every control and row sits on.
const ROW_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);

/// Each history row's rect in panel space, newest first. Drawing and
/// hit-testing both read this, so a click can never land on a different entry
/// than the one drawn.
pub fn rows(fw: f32, top: f32, count: usize) -> Vec<Rect> {
    let y0 = top + CTL_TOP_PAD + CTL_H + CTL_GAP * 2.0;
    (0..count)
        .map(|i| Rect::new(PANEL_PAD, y0 + (ROW_H + ROW_GAP) * i as f32, fw - PANEL_PAD * 2.0, ROW_H))
        .collect()
}

/// The consent control's rect: full width, directly under the tab strip.
pub fn consent_rect(fw: f32, top: f32) -> Rect {
    Rect::new(
        PANEL_PAD,
        top + CTL_TOP_PAD,
        fw - PANEL_PAD * 2.0,
        CTL_H,
    )
}

/// The capture / clear-all control's rect, directly below the consent row.
pub fn capture_rect(fw: f32, top: f32) -> Rect {
    Rect::new(
        PANEL_PAD,
        top + CTL_TOP_PAD + CTL_H + CTL_GAP,
        fw - PANEL_PAD * 2.0,
        CTL_H,
    )
}

/// The Clipboard tab's content, laid out under `top`.
pub fn content(fw: f32, fh: f32, top: f32, entries: &[Entry], enabled: bool) -> Vec<Node> {
    let mut out = Vec::new();

    if !enabled {
        out.extend(control(fw, fh, consent_rect(fw, top), OFF_MSG, CTL_AMBER));
        return out;
    }

    out.extend(control(
        fw,
        fh,
        capture_rect(fw, top),
        if entries.is_empty() {
            "capture on — nothing copied yet"
        } else {
            "capture on — clear all"
        },
        if entries.is_empty() {
            ROW_TRACK
        } else {
            Rgba::rgba(0.85, 0.30, 0.30, 0.20)
        },
    ));

    for (i, (r, entry)) in rows(fw, top, entries.len()).into_iter().zip(entries).enumerate() {
        if r.y >= fh || r.max_y() <= 0.0 {
            break;
        }
        out.push(Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(9.0),
            fill: ROW_TRACK,
        });
        // The leading cell: a small tinted square whose glyph hints at the
        // entry's shape. Glyph nodes are placeholder squares until Phase 8,
        // so this is a text glyph instead — it renders and it is honest.
        let cell = Rect::new(r.x + 8.0, r.y + (r.h - 18.0) * 0.5, 18.0, 18.0);
        out.push(Node::RoundRect {
            rect: put(fw, fh, cell),
            radii: CornerRadii::uniform(5.0),
            fill: Rgba::rgba(1.0, 1.0, 1.0, 0.10),
        });
        let style = TextStyle::numeric(10.0);
        let glyph = entry.kind.to_string();
        let gw = measure(&glyph, &style).max(1.0);
        let gh = line_height(&style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(cell.x + (cell.w - gw) * 0.5, cell.y + (cell.h - gh) * 0.5, gw, gh),
            ),
            text: glyph,
            style,
        });

        // The preview, clipped to what fits between the cell and the right pad.
        let text_x = cell.max_x() + 8.0;
        let text_w = (r.max_x() - PANEL_PAD * 0.5 - text_x).max(0.0);
        if text_w > 8.0 {
            let preview = TextStyle {
                size: 12.0,
                ..Default::default()
            };
            let ph = line_height(&preview);
            out.push(Node::Text {
                rect: put(fw, fh, Rect::new(text_x, r.y + (r.h - ph) * 0.5, text_w, ph)),
                text: entry.preview(PREVIEW_CHARS),
                style: preview,
            });
            let _ = i;
        }
    }
    out
}

/// The consent row's label. One constant, so the text the panel shows and the
/// text the tests pin cannot drift apart.
const OFF_MSG: &str = "Clipboard history is off — enable it to capture copies.";

/// A control row: a full-width track plus its left-aligned label.
fn control(fw: f32, fh: f32, r: Rect, label: &str, fill: Rgba) -> Vec<Node> {
    let style = TextStyle {
        size: 12.0,
        ..Default::default()
    };
    let h = line_height(&style);
    vec![
        Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(9.0),
            fill,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: usize) -> Vec<Entry> {
        (0..n)
            .map(|i| Entry {
                text: format!("entry number {i}"),
                at: i as u64,
                kind: 'e',
            })
            .collect()
    }

    #[test]
    fn rows_are_ordered_newest_first_and_fit_inside_the_panel() {
        let r = rows(1_000.0, 60.0, 5);
        assert_eq!(r.len(), 5);
        for w in r.windows(2) {
            assert!(w[0].max_y() <= w[1].y + 0.01, "rows must stack downward");
            assert!(w[0].x >= PANEL_PAD - 0.01);
            assert!(w[0].max_x() <= 1_000.0 - PANEL_PAD + 0.01);
        }
    }

    #[test]
    fn consent_and_capture_controls_do_not_overlap() {
        let c = consent_rect(1_000.0, 60.0);
        let k = capture_rect(1_000.0, 60.0);
        assert!(c.max_y() <= k.y + 0.01);
        assert!(c.max_x() <= 1_000.0 - PANEL_PAD + 0.01);
    }

    #[test]
    fn an_off_panel_draws_consent_and_no_history() {
        // One control: a track plus its label, and no history rows.
        let nodes = content(1_000.0, 400.0, 60.0, &entries(3), false);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes.iter().filter(|n| matches!(n, Node::Text { .. })).count(),
            1
        );
        assert!(nodes.iter().any(
            |n| matches!(n, Node::Text { text, .. } if text == OFF_MSG)
        ));
    }

    #[test]
    fn an_on_panel_with_entries_draws_more_rows_than_an_empty_one() {
        let empty = content(1_000.0, 400.0, 60.0, &entries(0), true);
        let full = content(1_000.0, 400.0, 60.0, &entries(3), true);
        assert!(full.len() > empty.len());
    }

    #[test]
    fn rows_stop_at_the_panel_bottom_so_nothing_draws_past_the_footer() {
        // A panel too short for every entry must not emit rows past its edge.
        let nodes = content(1_000.0, 200.0, 90.0, &entries(10), true);
        assert!(nodes.iter().all(|n| match n {
            Node::Group { children, .. } => children
                .iter()
                .all(|c| c.rect().max_y() <= 200.0 + layout::BOUNDS_EPSILON),
            n => n.rect().max_y() <= 200.0 + layout::BOUNDS_EPSILON,
        }));
    }
}