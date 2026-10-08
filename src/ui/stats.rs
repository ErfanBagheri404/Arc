//! The Stats tab: four metric rows, each a label, a value, and a sparkline.
//!
//! Pure builder, same contract as `ui::media`: `content(fw, fh, top, stats)`
//! returns scene nodes from a `services::metrics::Stats` snapshot. The sampler
//! owns every Win32 reader; this module only arranges what it published.
//!
//! A metric that is `None` draws as `n/a` with a flat baseline rather than
//! hiding the row: the island keeps its height, so switching tabs never makes
//! the layout jump.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::metrics::Stats;
use crate::ui::layout;
use crate::ui::line_height;

/// Sparkline box height (reference: `18`).
const SPARK_H: f32 = 18.0;
/// Row padding inside the row card.
const ROW_PAD: f32 = 8.0;
/// Gap between the label column and the sparkline.
const COL_GAP: f32 = 10.0;
/// Width of the right-aligned value column.
const VALUE_W: f32 = 62.0;
/// Radius of the sparkline baseline.
const SPARK_RADIUS: f32 = 1.5;
/// Baseline colour under the sparkline fill.
const BASELINE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.12,
};

/// One labelled metric: a caption, a formatted value, and its history.
struct Row<'a> {
    label: &'static str,
    value: String,
    history: &'a [f32],
    /// 1.0 when the sparkline's peak maps to full height.
    scale: f32,
}

/// The Stats content block, laid out under `top` (the tab strip's bottom).
pub fn content(fw: f32, fh: f32, top: f32, stats: &Stats) -> Vec<Node> {
    let rows = rows(stats);
    let n = rows.len().max(1) as f32;
    // Four rows at the reference's 44 px plus gaps, scaled down to fit a
    // half-open island rather than overflowing it.
    let avail = (fh - crate::ui::FOOTER_BOTTOM_PAD - top).max(0.0);
    let full_h = n * ROW_H + (n - 1.0) * ROW_GAP;
    if avail <= 0.0 || full_h <= 0.0 || fw <= crate::ui::PANEL_PAD * 2.0 {
        return Vec::new();
    }
    let scale = (avail / full_h).min(1.0);
    let h = ROW_H * scale;
    let gap = ROW_GAP * scale;
    let y0 = top + (avail - full_h * scale).max(0.0) * 0.5;

    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let y = y0 + (h + gap) * i as f32;
        let card = Rect::new(crate::ui::PANEL_PAD, y, fw - crate::ui::PANEL_PAD * 2.0, h);
        out.push(Node::RoundRect {
            rect: put(fw, fh, card),
            radii: CornerRadii::uniform(crate::ui::ROW_RADIUS),
            fill: Rgba::rgba(1.0, 1.0, 1.0, 0.04),
        });
        let spark_w = (card.w - ROW_PAD * 2.0 - COL_GAP - VALUE_W).max(1.0);
        let value_x = card.max_x() - ROW_PAD - VALUE_W;
        let text_y = card.y + (h - line_height(&TextStyle::numeric(12.0))) * 0.5;

        let label_style = TextStyle::subtitle(Rgba::WHITE);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(card.x + ROW_PAD, text_y, spark_w, line_height(&label_style)),
            ),
            text: row.label.to_string(),
            style: label_style,
        });

        let value_style = TextStyle::numeric(12.0);
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(value_x, text_y, VALUE_W, line_height(&value_style))),
            text: row.value.clone(),
            style: value_style,
        });

        out.extend(sparkline(
            fw,
            fh,
            Rect::new(
                card.x + ROW_PAD + spark_w + COL_GAP,
                card.y + (h - SPARK_H) * 0.5,
                spark_w,
                SPARK_H,
            ),
            row.history,
            row.scale,
        ));
    }
    out
}

/// Height of one metric row before any shrink-to-fit scaling (reference: `44`).
const ROW_H: f32 = 44.0;
/// Gap between metric rows.
const ROW_GAP: f32 = 8.0;

/// The four metric rows, in layout order.
fn rows(stats: &Stats) -> Vec<Row<'_>> {
    let m = &stats.current;
    vec![
        Row {
            label: "CPU",
            value: pct(m.cpu_percent),
            history: stats.cpu_history.samples(),
            // CPU is already a percentage: full height is 100 %.
            scale: 100.0,
        },
        Row {
            label: "Memory",
            value: m.memory_fraction().map_or("n/a".to_string(), |_| {
                format!("{} / {}", gib(m.memory_used.unwrap_or(0)), gib(m.memory_total.unwrap_or(0)))
            }),
            history: stats.memory_history.samples(),
            // Stored as a fraction, so a full-height bar is 1.0.
            scale: 1.0,
        },
        Row {
            label: "Network",
            value: match (m.net_down_bps, m.net_up_bps) {
                (Some(d), Some(u)) => format!("{} {}", rate(d), rate(u)),
                _ => "n/a".to_string(),
            },
            history: stats.net_history.samples(),
            // Auto-scaled: a NIC's absolute rate means nothing across machines,
            // so the sparkline fills to its own window peak.
            scale: 0.0,
        },
        Row {
            label: "Disk",
            value: match (m.disk_free, m.disk_total) {
                (Some(f), Some(_)) => format!("{} free", gib(f)),
                _ => "n/a".to_string(),
            },
            // Free space is a scalar, not a rate: a flat line, no ring history.
            history: &[],
            scale: 1.0,
        },
    ]
}

/// Render a history as a bar sparkline: one vertical bar per sample.
///
/// Bars, not a polyline: the scene has no path node, and a polyline would mean
/// adding one to the renderer for a 60-sample decoration.
fn sparkline(fw: f32, fh: f32, rect: Rect, samples: &[f32], scale: f32) -> Vec<Node> {
    let n = samples.len();
    if n == 0 {
        return vec![Node::RoundRect {
            rect: put(fw, fh, rect),
            radii: CornerRadii::uniform(SPARK_RADIUS),
            fill: BASELINE,
        }];
    }
    let top = match scale {
        s if s > 0.0 => s,
        // Auto-scale to the window peak; a flat history peaks at whatever the
        // single sample was, which draws a full-height block rather than nothing.
        _ => samples
            .iter()
            .copied()
            .fold(0.0_f32, f32::max)
            .max(f32::EPSILON),
    };
    let slot = rect.w / n as f32;
    let bar_w = (slot * 0.7).max(1.0);
    let mut out = Vec::with_capacity(n);
    for (i, v) in samples.iter().enumerate() {
        let frac = (v / top).clamp(0.0, 1.0);
        let bh = (frac * rect.h).max(1.0);
        out.push(Node::RoundRect {
            rect: put(
                fw,
                fh,
                Rect::new(rect.x + slot * i as f32, rect.max_y() - bh, bar_w, bh),
            ),
            radii: CornerRadii::uniform(SPARK_RADIUS),
            fill: Rgba::rgba(1.0, 1.0, 1.0, 0.28),
        });
    }
    out
}

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// A percentage, or `n/a`.
fn pct(v: Option<f32>) -> String {
    v.map_or("n/a".to_string(), |v| format!("{v:.0} %"))
}

/// Bytes as GiB with one decimal.
fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// Bytes/sec as a short rate: `1.2 MB/s`, `900 kB/s`, or `0 B/s`.
fn rate(bps: f64) -> String {
    const KB: f64 = 1024.0;
    if bps >= KB * KB {
        format!("{:.1} MB/s", bps / (KB * KB))
    } else if bps >= KB {
        format!("{:.0} kB/s", bps / KB)
    } else {
        format!("{bps:.0} B/s")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_switches_units_at_binary_prefixes() {
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(999.0), "999 B/s");
        assert_eq!(rate(1024.0), "1 kB/s");
        assert_eq!(rate(1.5 * 1024.0 * 1024.0), "1.5 MB/s");
    }

    #[test]
    fn gib_shows_one_decimal() {
        assert_eq!(gib(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(gib(8 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "8.5 GiB");
    }

    #[test]
    fn pct_rounds_and_handles_unknown() {
        assert_eq!(pct(None), "n/a");
        assert_eq!(pct(Some(41.6)), "42 %");
    }

    #[test]
    fn empty_history_draws_only_the_flat_baseline() {
        let nodes = sparkline(400.0, 120.0, Rect::new(0.0, 0.0, 100.0, 18.0), &[], 1.0);
        assert_eq!(nodes.len(), 1, "no history → the baseline card only");
    }

    #[test]
    fn sparkline_bars_scale_to_the_declared_peak() {
        // A sample at the peak gets the full height; half the peak gets half.
        let nodes = sparkline(400.0, 120.0, Rect::new(0.0, 0.0, 100.0, 18.0), &[100.0, 50.0], 100.0);
        let heights: Vec<f32> = nodes
            .iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, .. } => Some(rect.h),
                _ => None,
            })
            .collect();
        assert_eq!(heights.len(), 2);
        assert!((heights[0] - 18.0).abs() < 1e-4, "/Peak bar full height");
        assert!((heights[1] - 9.0).abs() < 1e-4, "Half peak, half height");
    }

    #[test]
    fn auto_scaled_sparkline_peaks_at_the_window_maximum() {
        // scale 0.0 means "fill to my own peak": the 40 sample must top out.
        let nodes = sparkline(400.0, 120.0, Rect::new(0.0, 0.0, 100.0, 18.0), &[10.0, 40.0], 0.0);
        let h: Vec<f32> = nodes
            .iter()
            .filter_map(|n| match n {
                Node::RoundRect { rect, .. } => Some(rect.h),
                _ => None,
            })
            .collect();
        assert!((h[0] - 4.5).abs() < 0.01, "Quarter of the window peak");
        assert!((h[1] - 18.0).abs() < 0.01, "The window peak fills");
    }

    #[test]
    fn unknown_metrics_render_as_na_not_garbage() {
        // All-None snapshot: every value reads "n/a".
        let stats = Stats::default();
        let rows = rows(&stats);
        assert_eq!(rows.len(), 4);
        for r in &rows {
            assert_eq!(r.value, "n/a", "{}", r.label);
        }
    }
}
