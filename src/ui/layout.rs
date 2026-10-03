//! Pure layout helpers: openness, easing, rect fitting, and row distribution.
//!
//! Nothing here touches the renderer. Every function is a total function of its
//! arguments so `ui::build` stays reproducible frame-for-frame (docs/05 §1:
//! "ui is a pure function of state").

use crate::core::geom::{Rect, Rgba};
use crate::core::scene::Align;

/// Collapsed pill width, mirrored from `app::state::PILL_W`.
pub const PILL_W_REF: f32 = 185.0;
/// Expanded panel width, mirrored from `app::state::PANEL_W`.
pub const PANEL_W_REF: f32 = 640.0;
/// Collapsed pill corner radius, mirrored from `app::state::PILL_RADIUS`.
pub const PILL_RADIUS_REF: f32 = 16.0;
/// Expanded panel corner radius, mirrored from `app::state::PANEL_RADIUS`.
pub const PANEL_RADIUS_REF: f32 = 24.0;

/// Collapsed pill height, mirrored from `app::state::PILL_H`.
pub const PILL_H_REF: f32 = 32.0;
/// Expanded panel height, mirrored from `app::state::PANEL_H`.
pub const PANEL_H_REF: f32 = 200.0;

/// Below this `openness` the panel content is not emitted at all. Emitting a
/// zero-alpha node still costs a D2D push and a squircle tessellation, and during
/// the first ~40 ms of the open spring the content is invisible anyway.
pub const CONTENT_FADE_IN_START: f32 = 0.15;

/// How far the pill glyph survives into the open transition. It has to be gone
/// well before `openness` reaches 1 or it would sit on top of the panel rows.
pub const PILL_GLYPH_FADE_OUT_END: f32 = 0.5;

/// Slack allowed by the bounds invariant, absorbing float error in the lerps.
/// Allowed slop when asserting emitted rects stay inside the frame.
/// Test-only: production code clamps hard in [`fit`].
#[cfg(test)]
pub const BOUNDS_EPSILON: f32 = 0.5;

/// How open the island is, `0.0` at the collapsed pill and `1.0` at the panel.
///
/// Driven by **both** axes, taking the smaller of the two progress values. Width is
/// the primary driver (the width and height springs share `SpringConfig::OPEN` but
/// start from different values, so width leads), but keying off width alone would
/// let a wide-and-short frame — which the springs genuinely pass through, and which
/// a caller can trivially pass by hand — emit a full panel into a 32 px strip.
/// Requiring the height to have caught up keeps content from appearing in a frame
/// with no room for it.
///
/// Clamped, because the springs overshoot and an over-driven size must not push
/// content past the panel layout.
#[must_use]
pub fn openness(width: f32, height: f32) -> f32 {
    openness_axis(width, PILL_W_REF, PANEL_W_REF).min(openness_axis(
        height,
        PILL_H_REF,
        PANEL_H_REF,
    ))
}

/// Progress along one axis, `0` at `from` and `1` at `to`.
fn openness_axis(value: f32, from: f32, to: f32) -> f32 {
    let span = to - from;
    if span <= 0.0 {
        return 0.0;
    }
    ((value - from) / span).clamp(0.0, 1.0)
}

/// Corner radius for a given openness: 16 at rest, 24 when open, lerped between.
///
/// The reference clamps the radius to `min(r, min(w, h) / 2)`; the renderer does
/// that too, but doing it here keeps the emitted rects self-consistent in tests.
#[must_use]
pub fn island_radius(open: f32) -> f32 {
    lerp(PILL_RADIUS_REF, PANEL_RADIUS_REF, open.clamp(0.0, 1.0))
}

/// Smoothstep. Used for opacity so content accelerates in rather than popping.
#[must_use]
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation with a clamped parameter.
#[must_use]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Force `rect` inside a `frame_w` × `frame_h` island.
///
/// The layout pass computes rects in panel space, but the window is mid-spring
/// and can be smaller than the panel it is morphing out of. Clamping here (rather
/// than trusting the caller) is what guarantees the invariant asserted by
/// `all_nodes_stay_inside_frame_across_sizes`.
#[must_use]
pub fn fit(rect: Rect, frame_w: f32, frame_h: f32) -> Rect {
    let w = rect.w.max(0.0).min(frame_w.max(0.0));
    let h = rect.h.max(0.0).min(frame_h.max(0.0));
    let x = rect.x.max(0.0).min(frame_w - w).max(0.0);
    let y = rect.y.max(0.0).min(frame_h - h).max(0.0);
    Rect::new(x, y, w, h)
}

/// Scale `base`'s alpha by `opacity`, leaving its channels alone.
#[must_use]
pub fn fade(base: Rgba, opacity: f32) -> Rgba {
    Rgba::rgba(base.r, base.g, base.b, base.a * opacity.clamp(0.0, 1.0))
}

/// Lay out `widths` on one row inside `available`, centered, with `gap` between
/// items. Returns `(x, width)` per item.
///
/// When the desired row does not fit, the gaps collapse first and then the item
/// widths are scaled down proportionally — items are never allowed to overflow
/// the container, which is how the tab strip survives a half-open island.
#[must_use]
pub fn row_items(widths: &[f32], gap: f32, available: f32) -> Vec<(f32, f32)> {
    let n = widths.len();
    if n == 0 || available <= 0.0 {
        return vec![(0.0, 0.0); n];
    }
    let total: f32 = widths.iter().copied().sum();
    let last = (n - 1) as f32;
    let (eff_gap, eff_widths): (f32, Vec<f32>) = if total + gap * last <= available {
        (gap.max(0.0), widths.to_vec())
    } else if total > 0.0 {
        let scale = available / total;
        (0.0, widths.iter().map(|w| (w * scale).max(0.0)).collect())
    } else {
        (0.0, vec![0.0; n])
    };

    let used: f32 = eff_widths.iter().sum::<f32>() + eff_gap * last;
    let mut x = Align::Center.offset(used, available);
    let mut out = Vec::with_capacity(n);
    for w in eff_widths {
        out.push((x, w));
        x += w + eff_gap;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openness_endpoints_and_clamping() {
        assert_eq!(openness(PILL_W_REF, PILL_H_REF), 0.0);
        assert_eq!(openness(PANEL_W_REF, PANEL_H_REF), 1.0);
        assert_eq!(openness(0.0, 0.0), 0.0, "sub-pill must not go negative");
        assert_eq!(
            openness(10_000.0, 10_000.0),
            1.0,
            "spring overshoot must not exceed 1"
        );
        let mid = openness(
            lerp(PILL_W_REF, PANEL_W_REF, 0.5),
            lerp(PILL_H_REF, PANEL_H_REF, 0.5),
        );
        assert!((mid - 0.5).abs() < 1e-6, "{mid}");
    }

    #[test]
    fn openness_takes_the_slower_axis() {
        // Wide but still pill-height: width alone would say "fully open", but the
        // content has nowhere to go, so the height must veto it.
        assert_eq!(openness(PANEL_W_REF, PILL_H_REF), 0.0);
        // Tall but still pill-width.
        assert_eq!(openness(PILL_W_REF, PANEL_H_REF), 0.0);
        // Mid-morph, the lagging axis is what governs.
        let wide_short = openness(
            lerp(PILL_W_REF, PANEL_W_REF, 0.9),
            lerp(PILL_H_REF, PANEL_H_REF, 0.2),
        );
        assert!((wide_short - 0.2).abs() < 1e-6, "{wide_short}");
        assert!(wide_short < 0.9, "the shorter axis must win");
    }

    #[test]
    fn radius_lerps_16_to_24() {
        assert!((island_radius(0.0) - 16.0).abs() < 1e-6);
        assert!((island_radius(1.0) - 24.0).abs() < 1e-6);
        assert!((island_radius(0.5) - 20.0).abs() < 1e-6);
        // Monotonic across the whole range.
        let mut prev = -1.0;
        for i in 0..=100 {
            let r = island_radius(i as f32 / 100.0);
            assert!(r >= prev, "radius dipped at {i}");
            prev = r;
        }
    }

    #[test]
    fn ease_is_clamped_and_flat_at_endpoints() {
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn fit_pulls_out_of_bounds_rects_back_inside() {
        let f = fit(Rect::new(-30.0, 900.0, 640.0, 200.0), 640.0, 200.0);
        assert_eq!(f, Rect::new(0.0, 0.0, 640.0, 200.0));
        let g = fit(Rect::new(620.0, 10.0, 100.0, 50.0), 640.0, 200.0);
        assert_eq!(g.x + g.w, 640.0);
        let z = fit(Rect::new(5.0, 5.0, -10.0, -10.0), 640.0, 200.0);
        assert_eq!((z.w, z.h), (0.0, 0.0));
    }

    #[test]
    fn fade_only_touches_alpha() {
        let c = fade(Rgba::rgba(0.9, 0.5, 0.1, 0.5), 0.4);
        assert!((c.a - 0.2).abs() < 1e-6);
        assert!((c.r - 0.9).abs() < 1e-6 && (c.b - 0.1).abs() < 1e-6);
        assert_eq!(fade(Rgba::WHITE, 1.0), Rgba::WHITE);
        assert_eq!(fade(Rgba::WHITE, -3.0).a, 0.0);
    }

    #[test]
    fn row_centers_when_it_fits() {
        let items = row_items(&[40.0, 60.0], 8.0, 200.0);
        // total = 100 + 8 gap = 108, centered in 200 → x0 = 46
        assert!((items[0].0 - 46.0).abs() < 1e-4, "{items:?}");
        assert!((items[0].1 - 40.0).abs() < 1e-4);
        assert!((items[1].0 - 94.0).abs() < 1e-4, "{items:?}");
    }

    #[test]
    fn row_shrinks_instead_of_overflowing() {
        let widths = [80.0, 80.0, 80.0];
        let items = row_items(&widths, 8.0, 100.0);
        let used: f32 = items.iter().map(|(_, w)| *w).sum();
        assert!(used <= 100.0 + 1e-4, "{items:?}");
        for (x, w) in &items {
            assert!(*x >= -1e-4 && x + w <= 100.0 + 1e-4);
        }
    }

    #[test]
    fn row_degenerate_inputs_are_empty_not_panicking() {
        assert!(row_items(&[], 8.0, 100.0).is_empty());
        let items = row_items(&[10.0], 8.0, 0.0);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], (0.0, 0.0));
    }

    #[test]
    fn lerp_clamps_t() {
        assert_eq!(lerp(10.0, 20.0, -1.0), 10.0);
        assert_eq!(lerp(10.0, 20.0, 2.0), 20.0);
    }
}
