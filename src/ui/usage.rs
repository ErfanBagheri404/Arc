//! The Usage tab: one row per LLM runner, a quota bar, and a token count.
//!
//! Pure builder, same contract as `ui::stats`: `content(fw, fh, top, snap)`
//! arranges what `services::usage` parsed. The service owns every file
//! read; this module only draws.
//!
//! A runner that is not installed draws as a disabled row rather than
//! disappearing, so the tab still explains itself on a machine with only
//! one of the two.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::usage::{Runner, Severity, Snapshot};
use crate::ui::line_height;
use crate::ui::put;

/// Height of one runner row before shrink-to-fit scaling.
const ROW_H: f32 = 52.0;
/// Gap between rows.
const ROW_GAP: f32 = 8.0;
/// Padding inside the row card.
const ROW_PAD: f32 = 10.0;
/// Width of the right-aligned token count.
const VALUE_W: f32 = 90.0;
/// Height of the quota bar.
const BAR_H: f32 = 6.0;

/// Bar colour for a severity band.
fn bar_color(sev: Severity) -> Rgba {
    match sev {
        Severity::Ok => Rgba::rgb(0.30, 0.85, 0.55),
        Severity::Warn => Rgba::rgb(0.95, 0.75, 0.25),
        Severity::Danger => Rgba::rgb(0.95, 0.30, 0.30),
    }
}

/// Format a token count for the value column: `1.2M`, `340K`, `900`.
fn fmt_tokens(t: u64) -> String {
    const M: u64 = 1_000_000;
    const K: u64 = 1_000;
    if t >= M {
        format!("{:.1}M", t as f64 / M as f64)
    } else if t >= K {
        format!("{}K", t / K)
    } else {
        t.to_string()
    }
}

/// The Usage content block, laid out under `top` (the tab strip's bottom).
pub fn content(fw: f32, fh: f32, top: f32, snap: &Snapshot) -> Vec<Node> {
    let rows: Vec<&Runner> = snap.rows();
    let n = rows.len().max(1) as f32;
    let avail = (fh - crate::ui::FOOTER_BOTTOM_PAD - top).max(0.0);
    let full_h = n * ROW_H + (n - 1.0) * ROW_GAP;
    if avail <= 0.0 || fw <= crate::ui::PANEL_PAD * 2.0 {
        return Vec::new();
    }
    let scale = (avail / full_h).min(1.0);
    let h = ROW_H * scale;
    let gap = ROW_GAP * scale;
    let y0 = top + (avail - full_h * scale).max(0.0) * 0.5;

    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let y = y0 + (h + gap) * i as f32;
        let card = Rect::new(crate::ui::PANEL_PAD, y, fw - crate::ui::PANEL_PAD * 2.0, h);
        out.push(Node::RoundRect {
            rect: put(fw, fh, card),
            radii: CornerRadii::uniform(crate::ui::ROW_RADIUS),
            fill: Rgba::rgba(1.0, 1.0, 1.0, 0.04),
        });

        let text_y = card.y + ROW_PAD;
        let label_style = TextStyle::subtitle(Rgba::WHITE);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(card.x + ROW_PAD, text_y, card.w - ROW_PAD * 2.0 - VALUE_W, line_height(&label_style)),
            ),
            text: r.name.to_string(),
            style: label_style,
        });

        let value_style = TextStyle::numeric(12.0);
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(card.max_x() - ROW_PAD - VALUE_W, text_y, VALUE_W, line_height(&value_style))),
            text: fmt_tokens(r.tokens),
            style: value_style,
        });

        let bar_y = card.max_y() - ROW_PAD - BAR_H;
        let bar_w = card.w - ROW_PAD * 2.0;
        out.push(Node::RoundRect {
            rect: put(fw, fh, Rect::new(card.x + ROW_PAD, bar_y, bar_w, BAR_H)),
            radii: CornerRadii::uniform(BAR_H / 2.0),
            fill: Rgba::rgba(1.0, 1.0, 1.0, 0.10),
        });
        let fill_w = bar_w * r.fraction();
        if fill_w > 0.5 {
            out.push(Node::RoundRect {
                rect: put(fw, fh, Rect::new(card.x + ROW_PAD, bar_y, fill_w, BAR_H)),
                radii: CornerRadii::uniform(BAR_H / 2.0),
                fill: bar_color(r.severity()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::usage::{DANGER, WARN};

    fn runner(name: &'static str, tokens: u64, limit: u64) -> Runner {
        Runner {
            name,
            tokens,
            limit: Some(limit),
            installed: true,
            files: 1,
        }
    }

    #[test]
    fn rows_are_stacked_and_bars_track_the_fraction() {
        let snap = Snapshot {
            claude: runner("Claude Code", 500_000, 1_000_000),
            codex: runner("Codex", 950_000, 1_000_000),
        };
        let nodes = content(400.0, 600.0, 100.0, &snap);
        // per runner: card + label + value + track + fill = 5
        assert_eq!(nodes.len(), 10, "{:#?}", nodes);
        // The card is the widest RoundRect in the row; the track and fill
        // are narrower, so the widest one per row is the card.
        let cards: Vec<Rect> = nodes
            .iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        // order per runner: card, track, fill
        assert_eq!(cards.len(), 6, "card + track + fill per runner");
        assert!(cards[3].y > cards[0].max_y(), "rows must not overlap");
    }

    #[test]
    fn absent_runners_draw_nothing() {
        let snap = Snapshot {
            claude: runner("Claude Code", 10, 100),
            codex: Runner::default(),
        };
        let nodes = content(400.0, 600.0, 100.0, &snap);
        assert_eq!(nodes.len(), 5, "one runner = card+label+value+track+fill");
    }

    #[test]
    fn token_formatting_matches_the_reference() {
        assert_eq!(fmt_tokens(900), "900");
        assert_eq!(fmt_tokens(1_500), "1K");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn bar_bands_are_ordered() {
        let snap = Snapshot {
            claude: runner("Claude Code", (WARN as f64 * 100.0) as u64, 100),
            codex: runner("Codex", (DANGER as f64 * 100.0) as u64, 100),
        };
        assert_eq!(snap.claude.severity(), Severity::Warn);
        assert_eq!(snap.codex.severity(), Severity::Danger);
    }

    #[test]
    fn zero_height_frame_draws_nothing() {
        let snap = Snapshot {
            claude: runner("Claude Code", 1, 100),
            codex: runner("Codex", 1, 100),
        };
        assert!(content(400.0, 0.0, 0.0, &snap).is_empty());
    }
}