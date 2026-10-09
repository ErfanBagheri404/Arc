//! Calendar tab: upcoming events from the subscribed `.ics` sources, soonest
//! first.
//!
//! Pure builder, same contract as `ui::clipboard`: `content(fw, fh, top,
//! events)` returns scene nodes in panel space and reads nothing else.
//!
//! The subscriptions are managed in settings for now — there is no add-box in
//! the panel because the island has no keyboard input path yet (same reason
//! clipboard search waits for Phase 7). A URL lands via the settings file, the
//! worker fetches it, and the events show here.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::calendar::Event;
use crate::ui::{layout, line_height, measure, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// Height of one event row.
pub const ROW_H: f32 = 34.0;
/// Gap between event rows.
pub const ROW_GAP: f32 = 8.0;
/// Gap above the first row.
pub const TOP_PAD: f32 = 8.0;
/// How many characters of a summary fit one row.
pub const PREVIEW_CHARS: usize = 44;

/// The grey track every row sits on.
const ROW_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);

/// Each event row's rect in panel space, soonest first. Drawing and
/// hit-testing both read this, so a click can never land on a different event
/// than the one drawn.
pub fn rows(fw: f32, top: f32, count: usize) -> Vec<Rect> {
    let y0 = top + TOP_PAD;
    (0..count)
        .map(|i| Rect::new(PANEL_PAD, y0 + (ROW_H + ROW_GAP) * i as f32, fw - PANEL_PAD * 2.0, ROW_H))
        .collect()
}

/// The Calendar tab's content, laid out under `top`.
pub fn content(fw: f32, fh: f32, top: f32, events: &[Event]) -> Vec<Node> {
    let mut out = Vec::new();
    let rows = rows(fw, top, events.len());
    if events.is_empty() {
        let style = TextStyle::numeric(12.0);
        let text = "No upcoming events — add an .ics URL in settings".to_string();
        let tw = measure(&text, &style);
        let th = line_height(&style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(PANEL_PAD, top + TOP_PAD, fw - PANEL_PAD * 2.0, th.max(ROW_H)),
            ),
            text,
            style: TextStyle {
                color: Rgba::rgba(1.0, 1.0, 1.0, 0.6),
                ..style
            },
        });
        let _ = tw;
        return out;
    }
    for (e, r) in events.iter().zip(rows.iter()) {
        out.push(Node::RoundRect {
            rect: put(fw, fh, *r),
            radii: CornerRadii::uniform(8.0),
            fill: layout::fade(ROW_TRACK, 1.0),
        });
        let mut label = e.label();
        if label.chars().count() > PREVIEW_CHARS {
            label = format!("{}…", label.chars().take(PREVIEW_CHARS).collect::<String>());
        }
        let style = TextStyle::numeric(12.0);
        let th = line_height(&style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(r.x + 10.0, r.y + (r.h - th) / 2.0, r.w - 20.0, th),
            ),
            text: label,
            style,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(summary: &str, start: i64) -> Event {
        Event {
            summary: summary.into(),
            start,
            end: start + 3600,
            all_day: false,
        }
    }

    #[test]
    fn empty_events_show_an_empty_state() {
        let nodes = content(400.0, 300.0, 0.0, &[]);
        assert_eq!(nodes.len(), 1, "one empty-state text, no rows");
    }

    #[test]
    fn one_row_per_event() {
        let evs = vec![ev("A", 100), ev("B", 200)];
        let nodes = content(400.0, 300.0, 0.0, &evs);
        // Row track + label per event.
        assert_eq!(nodes.len(), 4);
    }

    #[test]
    fn long_summaries_are_truncated() {
        let long = "x".repeat(PREVIEW_CHARS + 10);
        let evs = vec![ev(&long, 100)];
        let nodes = content(400.0, 300.0, 0.0, &evs);
        if let Node::Text { text, .. } = &nodes[1] {
            assert!(text.ends_with('…'));
            assert_eq!(text.chars().count(), PREVIEW_CHARS + 1);
        } else {
            panic!("expected text node");
        }
    }
}
