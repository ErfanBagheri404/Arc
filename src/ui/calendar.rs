//! Calendar tab: subscriptions, then upcoming events from those `.ics`
//! sources, soonest first.
//!
//! Pure builder, same contract as `ui::clipboard`: `content(fw, fh, top,
//! snapshot)` returns scene nodes in panel space and reads nothing else.
//!
//! ## Adding a subscription without a keyboard
//!
//! The island has no text input yet, so the add control copies the `.ics` URL
//! off the clipboard: **Add calendar = click with the URL already copied.**
//! That is a one-shot paste, not clipboard *history*, so it needs no consent —
//! see [`crate::services::clipboard::paste`]. Phase 7 replaces this with a real
//! text field when keyboard input lands.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::calendars::Snapshot;
use crate::ui::{layout, line_height, PANEL_PAD};

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
/// How many events are listed before the panel says there are more.
pub const MAX_EVENTS: usize = 4;
/// How many subscriptions are listed.
pub const MAX_SUBS: usize = 3;

/// The grey track every row sits on.
const ROW_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);
/// A subscription whose last fetch failed — red-tinted, not hidden.
const FAILED_TRACK: Rgba = Rgba::rgba(0.95, 0.35, 0.35, 0.12);
/// The add-calendar control's track, brighter so it reads as the one thing
/// you can click here.
const ADD_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.10);

/// Each event row's rect in panel space, soonest first. Drawing and
/// hit-testing both read this, so a click can never land on a different event
/// than the one drawn.
pub fn rows(fw: f32, top: f32, count: usize) -> Vec<Rect> {
    let y0 = top + TOP_PAD;
    (0..count)
        .map(|i| {
            Rect::new(
                PANEL_PAD,
                y0 + (ROW_H + ROW_GAP) * i as f32,
                fw - PANEL_PAD * 2.0,
                ROW_H,
            )
        })
        .collect()
}

/// The control rows: the add row, then one row per subscription.
fn head_rects(fw: f32, top: f32, subs: usize) -> Vec<Rect> {
    let y0 = top + TOP_PAD;
    let w = fw - PANEL_PAD * 2.0;
    let mut out = vec![Rect::new(PANEL_PAD, y0, w, ROW_H)];
    for i in 0..subs {
        out.push(Rect::new(
            PANEL_PAD,
            y0 + (ROW_H + ROW_GAP) * (i + 1) as f32,
            w,
            ROW_H,
        ));
    }
    out
}

/// Top of the event list, below every control.
fn events_top(top: f32, subs: usize) -> f32 {
    top + TOP_PAD + (ROW_H + ROW_GAP) * (subs as f32 + 1.0)
}

/// The Calendar tab's content, laid out under `top`.
pub fn content(fw: f32, fh: f32, top: f32, snap: &Snapshot) -> Vec<Node> {
    let mut out = Vec::new();
    let subs = snap.subs.len().min(MAX_SUBS);
    let head = head_rects(fw, top, subs);

    // Add row. The label names the gesture, so it is not a guess.
    let add = head[0];
    out.push(Node::RoundRect {
        rect: put(fw, fh, add),
        radii: CornerRadii::uniform(8.0),
        fill: layout::fade(ADD_TRACK, 1.0),
    });
    let th = line_height(&TextStyle::numeric(12.0));
    out.push(Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(add.x + 10.0, add.y + (add.h - th) / 2.0, add.w - 20.0, th),
        ),
        text: "+ Add calendar — copy an .ics URL first".to_string(),
        style: TextStyle::numeric(12.0),
    });

    // One row per subscription; a failed fetch shows its name in red rather
    // than silently showing fewer events.
    for (i, s) in snap.subs.iter().take(MAX_SUBS).enumerate() {
        let r = head[i + 1];
        let bad = snap.failed.iter().any(|f| f == &s.name);
        out.push(Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(8.0),
            fill: layout::fade(if bad { FAILED_TRACK } else { ROW_TRACK }, 1.0),
        });
        let lh = line_height(&TextStyle::numeric(11.0));
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(r.x + 10.0, r.y + (r.h - lh) / 2.0, r.w - 20.0, lh),
            ),
            text: if bad {
                format!("{} — unreachable", s.name)
            } else {
                s.name.clone()
            },
            style: TextStyle {
                size: 11.0,
                color: Rgba::rgba(1.0, 1.0, 1.0, if bad { 0.6 } else { 0.7 }),
                ..TextStyle::default()
            },
        });
    }

    // Events, soonest first.
    let evs: Vec<_> = snap.events.iter().take(MAX_EVENTS).collect();
    if evs.is_empty() {
        let lh = line_height(&TextStyle::numeric(12.0));
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(
                    PANEL_PAD,
                    events_top(top, subs) + TOP_PAD,
                    fw - PANEL_PAD * 2.0,
                    lh,
                ),
            ),
            text: "No upcoming events".to_string(),
            style: TextStyle {
                color: Rgba::rgba(1.0, 1.0, 1.0, 0.6),
                ..TextStyle::numeric(12.0)
            },
        });
        return out;
    }
    for (e, r) in evs.iter().zip(rows(fw, events_top(top, subs), evs.len())) {
        out.push(Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(8.0),
            fill: layout::fade(ROW_TRACK, 1.0),
        });
        let mut label = e.label();
        if label.chars().count() > PREVIEW_CHARS {
            label = format!(
                "{}…",
                label.chars().take(PREVIEW_CHARS).collect::<String>()
            );
        }
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(r.x + 10.0, r.y + (r.h - th) / 2.0, r.w - 20.0, th),
            ),
            text: label,
            style: TextStyle::numeric(12.0),
        });
    }
    if snap.events.len() > MAX_EVENTS {
        let rest = TextStyle::numeric(11.0);
        let lh = line_height(&rest);
        let y = events_top(top, subs) + TOP_PAD + (ROW_H + ROW_GAP) * evs.len() as f32;
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, lh)),
            text: format!("+{} more", snap.events.len() - MAX_EVENTS),
            style: TextStyle {
                color: Rgba::rgba(1.0, 1.0, 1.0, 0.45),
                ..rest
            },
        });
    }
    out
}

/// Which control panel-space `(x, y)` hits: `0` is add, `Some(i + 1)` is
/// subscription `i`. Event rows and empty space return `None`.
pub fn head_hit(fw: f32, top: f32, subs: usize, x: f32, y: f32) -> Option<usize> {
    head_rects(fw, top, subs)
        .iter()
        .position(|r| x >= r.x && x <= r.max_x() && y >= r.y && y <= r.max_y())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::calendar::Event;
    use crate::services::calendars::Sub;

    fn ev(summary: &str, start: i64) -> Event {
        Event {
            summary: summary.into(),
            start,
            end: start + 3600,
            all_day: false,
        }
    }

    fn texts(nodes: &[Node]) -> Vec<&str> {
        nodes
            .iter()
            .filter_map(|n| match n {
                Node::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn add_row_is_present_even_with_no_subs_or_events() {
        let nodes = content(400.0, 400.0, 0.0, &Snapshot::default());
        // add track + add label + empty-state text
        assert_eq!(nodes.len(), 3);
        let a = head_rects(400.0, 0.0, 0)[0];
        assert_eq!(head_hit(400.0, 0.0, 0, a.x + 2.0, a.y + 2.0), Some(0));
    }

    #[test]
    fn a_click_on_a_sub_row_names_that_sub_not_add() {
        // The controls are positional: geometry, not the sub contents,
        // decides the hit.
        let r = head_rects(400.0, 0.0, 2)[2];
        assert_eq!(head_hit(400.0, 0.0, 2, r.x + 2.0, r.y + 2.0), Some(2));
        let a = head_rects(400.0, 0.0, 0)[0];
        assert_eq!(head_hit(400.0, 0.0, 2, a.x + 2.0, a.y + 2.0), Some(0));
    }

    #[test]
    fn a_failed_sub_is_marked_not_hidden() {
        let nodes = content(
            400.0,
            400.0,
            0.0,
            &Snapshot {
                subs: vec![Sub {
                    name: "Broken".into(),
                    url: "x".into(),
                }],
                failed: vec!["Broken".into()],
                ..Snapshot::default()
            },
        );
        assert!(
            texts(&nodes).iter().any(|t| t.contains("unreachable")),
            "a broken URL must be visible"
        );
    }

    #[test]
    fn events_start_below_every_control() {
        // One sub means one sub row; the event list starts after it.
        assert!(rows(400.0, events_top(0.0, 1), 1)[0].y > head_rects(400.0, 0.0, 1)[1].max_y());
    }

    #[test]
    fn long_summaries_are_truncated() {
        let long = "x".repeat(PREVIEW_CHARS + 10);
        let nodes = content(
            400.0,
            400.0,
            0.0,
            &Snapshot {
                events: vec![ev(&long, 100)],
                ..Snapshot::default()
            },
        );
        // The label carries a HH:MM prefix, so match the repeated body.
        let label = texts(&nodes)
            .into_iter()
            .find(|t| t.contains(&"x".repeat(8)))
            .expect("event label");
        assert!(label.ends_with('…'));
    }

    #[test]
    fn hits_never_reach_a_sub_that_is_not_drawn() {
        // More subs than drawn: hit row i only names drawn subs, so layout
        // and routing cannot disagree on which sub a click unsubscribes.
        let subs = MAX_SUBS + 3;
        for i in 0..subs {
            let r = head_rects(400.0, 0.0, subs)[i + 1];
            let hit = head_hit(400.0, 0.0, subs.min(MAX_SUBS), r.x + 2.0, r.y + 2.0);
            assert_eq!(hit, (i < MAX_SUBS).then_some(i + 1));
        }
    }

    #[test]
    fn overflow_says_how_many_more() {
        let events: Vec<_> = (0..MAX_EVENTS + 3)
            .map(|i| ev(&format!("E{i}"), 100 + i as i64 * 3600))
            .collect();
        let nodes = content(
            400.0,
            700.0,
            0.0,
            &Snapshot {
                events,
                ..Snapshot::default()
            },
        );
        assert!(
            texts(&nodes).iter().any(|t| t.contains("+3 more"))
        );
    }
}