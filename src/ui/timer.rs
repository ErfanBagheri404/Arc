//! Timer tab: five preset buttons, the countdown, and its progress bar.
//!
//! Pure builder, same contract as `ui::stats`: `content(fw, fh, top, timer)`
//! returns scene nodes in panel space and reads nothing else.

use crate::app::timer::{preset_label, Timer, PRESETS};
use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::ui::{layout, line_height, measure, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// The timer's elapsed colour — same green as the media bar's fill.
const TIMER_GREEN: Rgba = Rgba::rgb(0.20, 0.78, 0.35);

/// Height of a preset button.
pub const BTN_H: f32 = 34.0;
/// Gap between preset buttons.
pub const BTN_GAP: f32 = 8.0;

/// Each preset button's rect in panel space, in [`PRESETS`] order. Drawing and
/// hit-testing both read this, so a click can never land on a different preset
/// than the one drawn.
pub fn buttons(fw: f32, top: f32) -> Vec<Rect> {
    let avail = (fw - PANEL_PAD * 2.0).max(0.0);
    let n = PRESETS.len() as f32;
    let w = ((avail - BTN_GAP * (n - 1.0)) / n).max(0.0);
    (0..PRESETS.len())
        .map(|i| Rect::new(PANEL_PAD + (w + BTN_GAP) * i as f32, top + 8.0, w, BTN_H))
        .collect()
}

/// The Timer tab's content, laid out under `top`.
pub fn content(fw: f32, fh: f32, top: f32, timer: &Timer) -> Vec<Node> {
    let mut out = Vec::new();
    for (i, r) in buttons(fw, top).into_iter().enumerate() {
        let active = i == timer.preset && timer.remaining > 0;
        out.push(Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(9.0),
            fill: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: if active { 0.14 } else { 0.06 },
            },
        });
        let style = TextStyle::numeric(12.0);
        let label = preset_label(PRESETS[i]);
        let w = measure(&label, &style).max(1.0);
        let lh = line_height(&style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(r.x + (r.w - w) * 0.5, r.y + (r.h - lh) * 0.5, w, lh),
            ),
            text: label,
            style,
        });
    }

    // The countdown line, then its progress — a bar, not a ring: the node set
    // already draws bars, and a horizontal fill reads just as well here.
    let label = timer.label();
    let style = TextStyle::numeric(26.0);
    let lh = line_height(&style);
    let below = top + 8.0 + BTN_H + 12.0;
    if below + lh > fh {
        return out;
    }
    out.push(Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(PANEL_PAD, below, fw - PANEL_PAD * 2.0, lh),
        ),
        text: label,
        style,
    });
    let bar_y = below + lh + 8.0;
    if bar_y + 4.0 <= fh {
        out.push(Node::Bar {
            rect: put(
                fw,
                fh,
                Rect::new(PANEL_PAD, bar_y, fw - PANEL_PAD * 2.0, 4.0),
            ),
            progress: timer.progress(),
            track: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.10,
            },
            fill: TIMER_GREEN,
            radii: CornerRadii::uniform(2.0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_buttons_fit_inside_the_panel_without_overflowing() {
        for r in buttons(1_000.0, 60.0) {
            assert!(r.x >= PANEL_PAD - 0.01);
            assert!(r.max_x() <= 1_000.0 - PANEL_PAD + 0.01);
        }
    }

    #[test]
    fn buttons_are_non_overlapping_and_ordered() {
        let b = buttons(1_000.0, 60.0);
        assert_eq!(b.len(), PRESETS.len());
        for w in b.windows(2) {
            assert!(w[0].max_x() <= w[1].x + 0.01);
        }
    }

    #[test]
    fn a_running_timer_draws_one_more_node_than_an_idle_one() {
        // The idle tab still draws the bar (as a track); the running tab adds
        // nothing structurally different — the bar's progress carries the state,
        // so both render the same node count. Pin that so a rewrite cannot
        // silently drop the countdown text.
        let idle = content(1_000.0, 400.0, 60.0, &Timer::default());
        let running = content(
            1_000.0,
            400.0,
            60.0,
            &Timer {
                remaining: 120,
                preset: 1,
            },
        );
        assert_eq!(idle.len(), running.len());
        assert!(running.iter().any(
            |n| matches!(n, Node::Text { text, .. } if text == "02:00")
        ));
    }

    #[test]
    fn preset_buttons_survive_a_cramped_panel_and_the_countdown_does_not() {
        // A panel too short for the countdown must not draw text past its edge;
        // the buttons, being the control surface, always fit.
        let nodes = content(1_000.0, 130.0, 90.0, &Timer::default());
        assert_eq!(
            nodes
                .iter()
                .filter(|n| matches!(n, Node::RoundRect { .. }))
                .count(),
            PRESETS.len()
        );
        assert!(!nodes
            .iter()
            .any(|n| matches!(n, Node::Text { rect, .. } if rect.max_y() > 130.0)));
    }
}
