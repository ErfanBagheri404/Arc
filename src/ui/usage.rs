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
use crate::core::scene::{Align, Node, TextStyle, Weight};
use crate::services::usage::{fmt_age, fmt_countdown, Runner, Severity, Snapshot};
use crate::ui::{line_height, put};

/// Height of one runner row before shrink-to-fit scaling.
const ROW_H: f32 = 66.0;
/// Gap between rows.
const ROW_GAP: f32 = 8.0;
/// Padding inside the row card.
const ROW_PAD: f32 = 14.0;
/// Width of the right-aligned token count.
const VALUE_W: f32 = 72.0;
/// Ring diameter and band thickness, from the reference (d38 / stroke 4).
const RING_D: f32 = 38.0;
const RING_T: f32 = 4.0;
/// Height of the weekly meter bar under the text block.
const METER_H: f32 = 4.0;

/// Per-provider tint. Fixed so a row reads the same across refreshes.
fn tint(name: &str) -> Rgba {
    if name.starts_with("Claude") {
        Rgba::rgb(0.851, 0.529, 0.349) // #d98759
    } else {
        Rgba::rgb(0.439, 0.722, 0.980) // #70b8fa
    }
}

/// Muted label colour for secondary text.
fn muted() -> Rgba {
    Rgba::rgba(1.0, 1.0, 1.0, 0.55)
}

/// Bar/ring colour for a severity band.
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

/// The line under the provider name: window span, reset countdown, or the
/// reason there is no number.
fn sub_line(r: &Runner, now: i64) -> String {
    if !r.installed {
        return "not installed".into();
    }
    let Some(w) = r.reported else {
        return if r.tokens == 0 {
            "no usage yet".into()
        } else {
            "no quota data".into()
        };
    };
    let span = match w.window_minutes {
        300 => "5h".to_string(),
        10_080 => "7d".to_string(),
        m if m > 0 && m % 1440 == 0 => format!("{}d", m / 1440),
        m if m > 0 => format!("{}h", m / 60),
        _ => "window".to_string(),
    };
    match w.secs_until_reset(now) {
        Some(s) => format!("{span} · resets {}", fmt_countdown(s)),
        None => format!(
            "{span} · {}",
            fmt_age(now - r.last_activity.unwrap_or(now))
        ),
    }
}

/// The Usage content block, laid out under `top` (the tab strip's bottom).
///
/// `now` is UNIX seconds, passed in so the countdowns and the tests agree.
pub fn content(fw: f32, fh: f32, top: f32, snap: &Snapshot, now: i64) -> Vec<Node> {
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
    let pad = ROW_PAD * scale;
    let d = (RING_D * scale).min(h - pad);

    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let y = y0 + (h + gap) * i as f32;
        let card = Rect::new(crate::ui::PANEL_PAD, y, fw - crate::ui::PANEL_PAD * 2.0, h);
        out.push(Node::RoundRect {
            rect: put(fw, fh, card),
            radii: CornerRadii::uniform(crate::ui::ROW_RADIUS),
            fill: Rgba::rgba(1.0, 1.0, 1.0, 0.04),
        });

        let accent = tint(r.name);
        // Ring: the arc drains with the window, so remaining time is what
        // you see at a glance — the number inside is the used percentage.
        let frac = r.reported.map(|w| w.fraction()).unwrap_or(0.0);
        let ring_y = card.y + (h - d) * 0.5;
        let ring = Rect::new(card.x + pad, ring_y, d, d);
        out.push(Node::Arc {
            rect: put(fw, fh, ring),
            progress: if r.installed { 1.0 - frac } else { 0.0 },
            track: Rgba::rgba(1.0, 1.0, 1.0, 0.10),
            fill: if r.installed {
                bar_color(r.reported.map(|w| w.tone()).unwrap_or(Severity::Ok))
            } else {
                accent
            },
            thickness: (RING_T * scale).max(1.0),
        });
        if r.reported.is_some() && r.installed {
            let pct = format!("{}", (frac * 100.0).round() as i32);
            let st = TextStyle {
                size: (d * 0.30).max(8.0),
                weight: Weight::Semibold,
                tabular: true,
                align: Align::Center,
                ..Default::default()
            };
            out.push(Node::Text {
                rect: put(fw, fh, Rect::new(ring.x, ring.y, d, line_height(&st))),
                text: pct,
                style: st,
            });
        }

        // Text block sits right of the ring; the value column is reserved.
        let tx = ring.max_x() + pad * 0.9;
        let tw = (card.max_x() - pad - VALUE_W) - tx;
        let name_style = TextStyle {
            size: 12.0 * scale.max(0.75),
            weight: Weight::Semibold,
            ..Default::default()
        };
        let name_lh = line_height(&name_style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(tx, card.y + h * 0.18, tw.max(0.0), name_lh),
            ),
            text: r.name.to_string(),
            style: name_style,
        });

        let sub_style = TextStyle {
            size: 10.0 * scale.max(0.75),
            color: muted(),
            ..Default::default()
        };
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(
                    tx,
                    card.y + h * 0.18 + name_lh + 1.0,
                    tw.max(0.0),
                    line_height(&sub_style),
                ),
            ),
            text: sub_line(r, now),
            style: sub_style,
        });

        // Weekly window: the thin capsule under the text block.
        if let Some(w) = r.weekly.filter(|_| r.installed) {
            let bar_y = card.max_y() - pad * 0.6 - METER_H;
            let bar_w = tw.max(0.0);
            out.push(Node::RoundRect {
                rect: put(fw, fh, Rect::new(tx, bar_y, bar_w, METER_H)),
                radii: CornerRadii::uniform(METER_H / 2.0),
                fill: Rgba::rgba(1.0, 1.0, 1.0, 0.10),
            });
            let fw_w = bar_w * w.fraction();
            if fw_w > 0.5 {
                out.push(Node::RoundRect {
                    rect: put(fw, fh, Rect::new(tx, bar_y, fw_w, METER_H)),
                    radii: CornerRadii::uniform(METER_H / 2.0),
                    fill: bar_color(w.tone()),
                });
            }
        }

        let value_style = TextStyle {
            size: 12.0 * scale.max(0.75),
            weight: Weight::Semibold,
            tabular: true,
            align: Align::End,
            color: if r.installed {
                Rgba::WHITE
            } else {
                muted()
            },
            ..Default::default()
        };
        let value_lh = line_height(&value_style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(
                    card.max_x() - pad - VALUE_W,
                    card.y + h * 0.18,
                    VALUE_W,
                    value_lh,
                ),
            ),
            text: fmt_tokens(r.tokens),
            style: value_style,
        });

        // The `est` badge: this number is our own block estimate, not a
        // quota the runner reported, and it must not read as authoritative.
        if r.estimated {
            let bw = 24.0;
            let bh = 14.0;
            let by = card.y + h * 0.18 + value_lh + 1.0;
            out.push(Node::RoundRect {
                rect: put(
                    fw,
                    fh,
                    Rect::new(card.max_x() - pad - bw, by, bw, bh),
                ),
                radii: CornerRadii::uniform(4.0),
                fill: Rgba::rgba(1.0, 1.0, 1.0, 0.08),
            });
            let bs = TextStyle {
                size: 8.0,
                weight: Weight::Regular,
                align: Align::Center,
                color: muted(),
                ..Default::default()
            };
            out.push(Node::Text {
                rect: put(fw, fh, Rect::new(card.max_x() - pad - bw, by, bw, line_height(&bs))),
                text: "est".into(),
                style: bs,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::usage::{Window, DANGER, WARN};

    fn runner(name: &'static str, tokens: u64, limit: u64) -> Runner {
        Runner {
            name,
            tokens,
            limit: Some(limit),
            installed: true,
            files: 1,
            reported: None,
            weekly: None,
            estimated: false,
            last_activity: None,
        }
    }

    fn window(used: f32, reset_in: i64, now: i64) -> Window {
        Window {
            used_percent: used,
            window_minutes: 300,
            resets_at: Some(now + reset_in),
        }
    }

    fn texts(nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .filter_map(|n| match n {
                Node::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rows_are_stacked_and_do_not_overlap() {
        let snap = Snapshot {
            claude: runner("Claude Code", 500_000, 1_000_000),
            codex: runner("Codex", 950_000, 1_000_000),
        };
        let nodes = content(400.0, 600.0, 100.0, &snap, 0);
        let cards: Vec<Rect> = nodes
            .iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, radii, .. } if radii.top_left == crate::ui::ROW_RADIUS => {
                    Some(*rect)
                }
                _ => None,
            })
            .collect();
        assert_eq!(cards.len(), 2, "one card per runner");
        assert!(cards[1].y > cards[0].max_y(), "rows must not overlap");
    }

    #[test]
    fn ring_drains_with_the_window_and_shows_used_percent() {
        let now = 1_700_000_000;
        let mut r = runner("Codex", 1_000, 1_000);
        r.reported = Some(window(75.0, 3600, now));
        let snap = Snapshot {
            claude: Runner::default(),
            codex: r,
        };
        let nodes = content(400.0, 600.0, 100.0, &snap, now);
        let arcs: Vec<f32> = nodes
            .iter()
            .filter_map(|n| match n {
                Node::Arc { progress, .. } => Some(*progress),
                _ => None,
            })
            .collect();
        assert_eq!(arcs.len(), 1);
        assert!((arcs[0] - 0.25).abs() < 1e-5, "ring shows remaining 25%");
        let t = texts(&nodes);
        assert!(t.iter().any(|s| s == "75"), "number is used percent: {t:?}");
        assert!(
            t.iter().any(|s| s.contains("resets in 1h")),
            "countdown present: {t:?}"
        );
    }

    #[test]
    fn missing_quota_still_draws_the_row() {
        let snap = Snapshot {
            claude: runner("Claude Code", 10, 100),
            codex: Runner::default(),
        };
        // rows() hides the absent Codex runner; Claude still draws its row.
        let t = texts(&content(400.0, 600.0, 100.0, &snap, 0));
        assert!(t.iter().any(|s| s == "Claude Code"));
        assert!(t.iter().any(|s| s.contains("no quota data")));
    }

    #[test]
    fn installed_runner_without_data_draws_a_full_ring() {
        let snap = Snapshot {
            claude: runner("Claude Code", 10, 100),
            codex: Runner::default(),
        };
        let arcs: Vec<f32> = content(400.0, 600.0, 100.0, &snap, 0)
            .iter()
            .filter_map(|n| match n {
                Node::Arc { progress, .. } => Some(*progress),
                _ => None,
            })
            .collect();
        // No reported window means 100% of the window remains.
        assert_eq!(arcs, vec![1.0], "no data = full ring, not empty");
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
        assert!(content(400.0, 0.0, 0.0, &snap, 0).is_empty());
    }
}
