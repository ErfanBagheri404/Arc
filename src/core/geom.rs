//! Geometry primitives: rects, colors, and an Apple-style squircle path.

/// Axis-aligned rect in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn max_x(&self) -> f32 {
        self.x + self.w
    }

    pub fn max_y(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.max_x() && py >= self.y && py < self.max_y()
    }

    /// Linear interpolation, used by the spring-driven layout so a morphing panel
    /// carries its children along instead of snapping.
    pub fn lerp(&self, other: &Rect, t: f32) -> Rect {
        Rect {
            x: self.x + (other.x - self.x) * t,
            y: self.y + (other.y - self.y) * t,
            w: self.w + (other.w - self.w) * t,
            h: self.h + (other.h - self.h) * t,
        }
    }
}

/// Non-premultiplied sRGB color with alpha in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    // Named for the CSS-style rgba() it mirrors; `Rgba::from_rgba` would read
    // as `from_…` without taking a tuple. Naming kept for call-site clarity.
    #[allow(clippy::self_named_constructors)]
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const BLACK: Self = Self::rgba(0.0, 0.0, 0.0, 1.0);
    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);

    /// Clamp channels to 0..=1.
    pub fn clamped(self) -> Self {
        let c = |v: f32| v.clamp(0.0, 1.0);
        Self {
            r: c(self.r),
            g: c(self.g),
            b: c(self.b),
            a: c(self.a),
        }
    }

    /// Relative luminance (Rec. 709).
    pub fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// HSV, for the album-art accent extractor.
    pub fn to_hsv(self) -> (f32, f32, f32) {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let d = max - min;
        let v = max;
        let s = if max <= f32::EPSILON { 0.0 } else { d / max };
        let h = if d <= f32::EPSILON {
            0.0
        } else if max == self.r {
            ((self.g - self.b) / d).rem_euclid(6.0)
        } else if max == self.g {
            (self.b - self.r) / d + 2.0
        } else {
            (self.r - self.g) / d + 4.0
        };
        (h * 60.0, s, v)
    }

    /// Parse `#rrggbb` (the on-disk form Settings persists colors as). `None`
    /// on any malformed input — a hand-edited file must fall back to a default,
    /// not turn the notch a surprising color.
    #[must_use]
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#')?.trim();
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self::rgb(
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
        ))
    }

    /// Lower-case `#rrggbb`, what [`settings::set_string`] writes. Round-trips
    /// through [`Self::parse_hex`].
    #[must_use]
    pub fn to_hex(self) -> String {
        let c = |f: f32| ((f.clamp(0.0, 1.0) * 255.0).round() as u32).min(255);
        format!("#{:02x}{:02x}{:02x}", c(self.r), c(self.g), c(self.b))
    }

    /// Inverse of `to_hsv`.
    pub fn from_hsv(h: f32, s: f32, v: f32) -> Self {
        let h = h.rem_euclid(360.0);
        let c = v * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match h as u32 / 60 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Self::rgb(r + m, g + m, b + m)
    }
}

/// A corner-radius pair, mirroring the reference design's asymmetric notch shape
/// (top radius small, bottom radius larger).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerRadii {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

/// Default is opaque black — the natural fill for notch/disabled surfaces.
impl Default for Rgba {
    fn default() -> Self {
        Self::BLACK
    }
}

impl CornerRadii {
    pub const fn uniform(r: f32) -> Self {
        Self {
            top_left: r,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    }

    /// Clamp every radius to what the rect can actually render (half the short side).
    /// Fit every radius to what the rect can actually render.
    ///
    /// A negative radius is not an error: it marks a *concave* corner — the
    /// wing/ear curve that sweeps from the screen edge into the body's side,
    /// as in DynamicNotch's `NotchShape`. The sign is preserved and only the
    /// magnitude is clamped, because "round outward" and "swoop inward" are
    /// different shapes sharing one field.
    pub fn clamped(self, w: f32, h: f32) -> Self {
        let m = (w.min(h) / 2.0).max(0.0);
        fn fit(v: f32, m: f32) -> f32 {
            match v.partial_cmp(&0.0) {
                Some(std::cmp::Ordering::Less) => -v.abs().min(m),
                _ => v.clamp(0.0, m),
            }
        }
        Self {
            top_left: fit(self.top_left, m),
            top_right: fit(self.top_right, m),
            bottom_right: fit(self.bottom_right, m),
            bottom_left: fit(self.bottom_left, m),
        }
    }
}

/// Cubic-circle kappa: control-point distance for a *true circular* arc.
const KAPPA: f32 = 0.552_284_8;

/// Where the corner's Bézier control points sit, as a fraction of the radius.
///
/// The curve's mid-point sits at `sqrt(2) * r * (4-3c) / 8` from the rect corner.
/// c=KAPPA (0.552) → 0.414r (flatter than you'd expect from "circular arc").
/// Larger c → smaller distance → curve fills the corner more → rounder look.
/// iOS continuous corners use ~0.9, giving 0.18r ≈ superellipse exponent 4.
pub const SQUIRCLE_C: f32 = 0.9;

/// One cubic Bézier segment of a rounded-rect outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathSegment {
    pub c1: (f32, f32),
    pub c2: (f32, f32),
    pub to: (f32, f32),
}

/// Build the outline of `rect` with continuous (squircle) corners as cubic Béziers.
/// Four segments, clockwise from the top-left corner.
#[allow(clippy::vec_init_then_push)]
/// Geometry of the DynamicNotch silhouette: a body inset by concave top
/// wings that flare out to the frame's top corners. Returns
/// `(body_left, body_right, bottom_left_r, bottom_right_r, wing_l, wing_r)`.
pub fn notch_metrics(rect: &Rect, radii: CornerRadii) -> (f32, f32, f32, f32, f32, f32) {
    // A wing can't scoop deeper than the frame's height or wider than half
    // the frame, or the two wings would cross.
    let wing = |v: f32| (-v).max(0.0).min(rect.h).min(rect.w / 2.0);
    let (wl, wr) = (wing(radii.top_left), wing(radii.top_right));
    // The wings flare OUTSIDE the body, so they never narrow it: the body keeps
    // the frame's full width, which is the width every caller (content layout,
    // the hover capsule, the click hit-test) already assumes. Carving the wings
    // out of the rect painted a 20px-narrower body than the content laid out in,
    // which is what made edges look eaten and the minimized pill read narrower
    // than the hover one.
    let body_left = rect.x;
    let body_right = rect.max_x();
    let body_w = (body_right - body_left).max(0.0);
    let m = (rect.h / 2.0).min(body_w / 2.0);
    let bl = radii.bottom_left.max(0.0).min(m);
    let br = radii.bottom_right.max(0.0).min(m);
    (body_left, body_right, bl, br, wl, wr)
}

/// The notch outline: clockwise from the frame's top-right corner —
/// TR wing down to the body's right side, bottom-right, bottom, bottom-left,
/// up the body's left side, TL wing back to the frame's top-left corner.
/// The implicit lines between segments are the body's sides and the top edge
/// along the screen. Same chaining contract as `squircle_segments`.
#[allow(clippy::vec_init_then_push)]
fn notch_segments(rect: &Rect, radii: CornerRadii) -> Vec<PathSegment> {
    let (body_l, body_r, bl, br, wl, wr) = notch_metrics(rect, radii);
    let (y, max_y) = (rect.y, rect.max_y());
    let c = SQUIRCLE_C;
    let mut segs = Vec::with_capacity(4);

    // TR wing: outer top (rect.max_x() + wr, y) → body right side (rect.max_x(), y + wr).
    // Flares OUTWARD into the transparent frame margin, so the body keeps its
    // full width and the ears are decoration beside it, not carved out of it.
    segs.push(PathSegment {
        c1: (rect.max_x() + wr * 1.0 / 3.0, y),
        c2: (rect.max_x(), y + wr / 3.0),
        to: (rect.max_x(), y + wr),
    });
    // Bottom-right on the body's own extent.
    segs.push(PathSegment {
        c1: (body_r, max_y - br * (1.0 - c)),
        c2: (body_r - br * (1.0 - c), max_y),
        to: (body_r - br, max_y),
    });
    // Bottom-left on the body's own extent.
    segs.push(PathSegment {
        c1: (body_l + bl * (1.0 - c), max_y),
        c2: (body_l, max_y - bl * (1.0 - c)),
        to: (body_l, max_y - bl),
    });
    // TL wing: body left side (rect.x, y + wl) → outer top (rect.x - wl, y),
    // mirroring the TR wing. Flares OUTWARD into the transparent margin.
    segs.push(PathSegment {
        c1: (rect.x, y + wl / 3.0),
        c2: (rect.x - wl * 1.0 / 3.0, y),
        to: (rect.x - wl, y),
    });

    segs
}

/// Bézier outline of a rounded rect. A corner whose radius reaches `min(w,h)/2`
/// is a semicircular cap (a capsule) and gets the circle-.tight constant;
/// everything else stays the squircle fillet.
pub fn squircle_segments(rect: &Rect, radii: CornerRadii) -> Vec<PathSegment> {
    if radii.top_left < 0.0 || radii.top_right < 0.0 {
        return notch_segments(rect, radii);
    }
    let r = radii.clamped(rect.w, rect.h);
    let (x, y) = (rect.x, rect.y);
    let (max_x, max_y) = (rect.max_x(), rect.max_y());

    // A corner whose radius reaches h/2 is a semicircular end cap — a capsule.
    // The squircle curve hugs the corner there and reads as a rounded square,
    // so a capsule uses the circle-Bézier constant instead (c = 0.5523 puts the
    // control offsets at k·r = (4/3)·tan(22.5°)·r, the exact quarter-circle arc).
    // Partially rounded corners keep the squircle look.
    let cap = (rect.w.min(rect.h) / 2.0).max(0.0);
    let corner_c = |corner: f32| {
        if corner >= cap - 0.5 {
            0.5523
        } else {
            SQUIRCLE_C
        }
    };

    // Four pushes with an exact matching capacity, each with its own comment
    // about which edge of the squircle it builds — clearer than an array of
    // four expression tails.
    let mut segs = Vec::with_capacity(4);

    // Top-left: (x + r.tl, y) → (x, y + r.tl)
    let c = corner_c(r.top_left);
    segs.push(PathSegment {
        c1: (x + r.top_left * (1.0 - c), y),
        c2: (x, y + r.top_left * (1.0 - c)),
        to: (x, y + r.top_left),
    });
    // Bottom-left: (x, max_y - r.bl) → (x + r.bl, max_y)
    let c = corner_c(r.bottom_left);
    segs.push(PathSegment {
        c1: (x, max_y - r.bottom_left * (1.0 - c)),
        c2: (x + r.bottom_left * (1.0 - c), max_y),
        to: (x + r.bottom_left, max_y),
    });
    // Bottom-right: (max_x - r.br, max_y) → (max_x, max_y - r.br)
    let c = corner_c(r.bottom_right);
    segs.push(PathSegment {
        c1: (max_x - r.bottom_right * (1.0 - c), max_y),
        c2: (max_x, max_y - r.bottom_right * (1.0 - c)),
        to: (max_x, max_y - r.bottom_right),
    });
    // Top-right: (max_x, y + r.tr) → (max_x - r.tr, y)
    let c = corner_c(r.top_right);
    segs.push(PathSegment {
        c1: (max_x, y + r.top_right * (1.0 - c)),
        c2: (max_x - r.top_right * (1.0 - c), y),
        to: (max_x - r.top_right, y),
    });

    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_basics() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(r.max_x(), 110.0);
        assert_eq!(r.max_y(), 70.0);
        assert_eq!(r.center(), (60.0, 45.0));
        assert!(r.contains(60.0, 45.0));
        assert!(!r.contains(9.9, 45.0));
        assert!(!r.contains(60.0, 70.0));
    }

    #[test]
    fn rect_lerp_endpoints() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(100.0, 50.0, 200.0, 40.0);
        assert_eq!(a.lerp(&b, 0.0), a);
        assert_eq!(a.lerp(&b, 1.0), b);
        let mid = a.lerp(&b, 0.5);
        assert!((mid.x - 50.0).abs() < 1e-6 && (mid.w - 105.0).abs() < 1e-6);
    }

    #[test]
    fn radii_clamp_to_half_short_side() {
        let r = CornerRadii::uniform(500.0).clamped(40.0, 32.0);
        assert_eq!(r.top_left, 16.0);
        // A negative radius is the concave wing notch: the clamp bounds its
        // magnitude, never its sign, or the wing would invert into a bulge.
        let r2 = CornerRadii::uniform(-5.0).clamped(40.0, 32.0);
        assert_eq!(r2.bottom_right, -5.0);
        let r3 = CornerRadii::uniform(-40.0).clamped(40.0, 32.0);
        assert_eq!(r3.bottom_right, -16.0);
    }

    #[test]
    fn notch_wings_scoop_from_the_body_sides_outward_past_the_frame() {
        // The silhouette is one figure: each wing leaves the body's side at the
        // frame's top edge and flares OUT into the transparent margin — so the
        // body keeps the frame's full width (the width content lays out in) and
        // the ears are decoration beside it, never carved out of it.
        let rect = Rect::new(0.0, 0.0, 100.0, 40.0);
        let radii = CornerRadii {
            top_left: -20.0,
            top_right: -20.0,
            bottom_left: 8.0,
            bottom_right: 8.0,
        };
        let segs = squircle_segments(&rect, radii);
        assert_eq!(segs.len(), 4);
        // Order: TR wing, BR, BL, TL wing.
        assert_eq!(
            segs[0].to,
            (rect.max_x(), 20.0),
            "TR wing lands on the body's own right side"
        );
        assert_eq!(
            segs[3].to,
            (-20.0, 0.0),
            "TL wing ends in the margin, mirrored past the frame's left edge"
        );
        // Tangent horizontal in the margin, vertical down the body's side.
        assert_eq!(segs[0].c1.1, rect.y);
        assert_eq!(segs[0].c2.0, rect.max_x());
        // Bottom corners ride the FULL frame: the body is not inset anymore.
        assert_eq!(segs[1].to, (rect.max_x() - 8.0, 40.0));
        assert_eq!(segs[2].to, (0.0, 32.0));
    }

    #[test]
    fn notch_wings_cannot_cross_or_exceed_the_frame() {
        // A wing deeper than the height, or wider than half the frame, would
        // invert the silhouette.
        let rect = Rect::new(0.0, 0.0, 30.0, 10.0);
        let radii = CornerRadii {
            top_left: -500.0,
            top_right: -500.0,
            bottom_left: 16.0,
            bottom_right: 16.0,
        };
        let (body_l, body_r, bl, br, wl, wr) = notch_metrics(&rect, radii);
        assert_eq!(
            (body_l, body_r),
            (rect.x, rect.max_x()),
            "the wings never narrow the body"
        );
        assert_eq!(wl, wr);
        assert!(bl >= 0.0 && br >= 0.0);
        let segs = squircle_segments(&rect, radii);
        for s in &segs {
            for (px, py) in [s.c1, s.c2, s.to] {
                // The wings may reach into the transparent margin (that is what
                // they are for), but only by the clamped wing width — never
                // unbounded, and never past the body's own edges.
                assert!(
                    px >= rect.x - wl - 1e-3 && px <= rect.max_x() + wr + 1e-3,
                    "control escaped: {px}"
                );
                assert!(
                    py >= rect.y - 1e-3 && py <= rect.max_y() + 1e-3,
                    "escaped: {py}"
                );
            }
        }
    }

    #[test]
    fn squircle_has_four_segments_ending_on_corners() {
        let rect = Rect::new(0.0, 0.0, 100.0, 40.0);
        let segs = squircle_segments(&rect, CornerRadii::uniform(16.0));
        assert_eq!(segs.len(), 4);
        // Each segment's endpoint sits on a corner arc: exactly one axis-aligned.
        for s in &segs {
            let on_vertical =
                (s.to.0 - rect.x).abs() < 1e-6 || (s.to.0 - rect.max_x()).abs() < 1e-6;
            let on_horizontal =
                (s.to.1 - rect.y).abs() < 1e-6 || (s.to.1 - rect.max_y()).abs() < 1e-6;
            assert!(
                on_vertical ^ on_horizontal,
                "endpoint {:?} not on an edge",
                s.to
            );
        }
    }

    #[test]
    fn corner_curves_are_true_arcs_not_chamfers() {
        // The mid-point of the bottom-right curve is where a "cut" corner shows
        // itself: a flat chamfer sits *farther* from the rect corner than a
        // circular arc, an iOS continuous corner sits closer. Measure it.
        let rect = Rect::new(0.0, 0.0, 100.0, 40.0);
        let r = 16.0;
        let segs = squircle_segments(&rect, CornerRadii::uniform(r));
        // segs[2] is bottom-right: (max_x - r, max_y) → (max_x, max_y - r).
        let s = segs[2];
        let to = bezier_point(((rect.max_x() - r), rect.max_y()), s.c1, s.c2, s.to, 0.5);
        let corner = (rect.max_x(), rect.max_y());
        let d = ((to.0 - corner.0).powi(2) + (to.1 - corner.1).powi(2)).sqrt();
        // iOS continuous corner (c=0.9): mid-point at sqrt(2)·r·(4-3c)/8 =
        // 0.18·r ≈ 2.9 px from the corner. Farther = chamfered (cut off),
        // much closer = squarer. Pin the iOS value.
        let ios = r * std::f32::consts::SQRT_2 * (4.0 - 3.0 * SQUIRCLE_C) / 8.0;
        assert!(
            (d - ios).abs() < 0.02 * r,
            "corner {d:.3} px from the rect corner; iOS lands at {ios:.3}"
        );
    }

    #[test]
    fn capsule_caps_are_circles_not_fillets() {
        // The hover pill's corner reaches h/2, so its control offsets must be
        // the circle one (0.4477·r), not the squircle one (0.1·r).
        let rect = Rect::new(0.0, 0.0, 220.0, 38.0);
        let r = 19.0;
        let segs = squircle_segments(&rect, CornerRadii::uniform(r));
        // c1 of the top-left segment is on the top edge, at (1 - c)·r from
        // the corner: circle → 0.4477·19 = 8.5, squircle → 0.1·19 = 1.9.
        let offset = segs[0].c1.0 - segs[0].to.0;
        assert!(
            (offset - 0.4477 * r).abs() < 0.02 * r,
            "capsule control offset {offset:.3}, expected the circle cap value {:.3}",
            0.4477 * r
        );
    }

    /// Cubic Bézier at parameter `t`.
    fn bezier_point(
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
        p3: (f32, f32),
        t: f32,
    ) -> (f32, f32) {
        let u = 1.0 - t;
        let x =
            u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y =
            u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        (x, y)
    }

    #[test]
    fn squircle_segments_stay_inside_rect() {
        let rect = Rect::new(5.0, 7.0, 120.0, 60.0);
        for s in squircle_segments(&rect, CornerRadii::uniform(20.0)) {
            for p in [s.c1, s.c2, s.to] {
                assert!(
                    p.0 >= rect.x - 1e-4 && p.0 <= rect.max_x() + 1e-4,
                    "x {p:?}"
                );
                assert!(
                    p.1 >= rect.y - 1e-4 && p.1 <= rect.max_y() + 1e-4,
                    "y {p:?}"
                );
            }
        }
    }

    #[test]
    fn squircle_tangent_is_continuous_at_join() {
        // Control points must approach each shared corner along the same tangent,
        // otherwise the outline kinks. Compare incoming tangent at `to` with the
        // segment's own start tangent.
        let rect = Rect::new(0.0, 0.0, 100.0, 60.0);
        let segs = squircle_segments(&rect, CornerRadii::uniform(18.0));
        for i in 0..segs.len() {
            let cur = segs[i];
            let next = segs[(i + 1) % segs.len()];
            let out_dir = (cur.to.0 - cur.c2.0, cur.to.1 - cur.c2.1);
            let in_dir = (next.c1.0 - cur.to.0, next.c1.1 - cur.to.1);
            let cross = out_dir.0 * in_dir.1 - out_dir.1 * in_dir.0;
            let scale = (out_dir.0.hypot(out_dir.1) * in_dir.0.hypot(in_dir.1)).max(1e-9);
            assert!(cross.abs() / scale < 1e-3, "corner {i} tangent kink");
        }
    }

    #[test]
    fn hsv_roundtrip() {
        for c in [
            Rgba::rgb(0.9, 0.2, 0.1),
            Rgba::rgb(0.1, 0.7, 0.4),
            Rgba::rgb(0.5, 0.5, 0.9),
        ] {
            let (h, s, v) = c.to_hsv();
            let back = Rgba::from_hsv(h, s, v);
            assert!((back.r - c.r).abs() < 1e-3, "{c:?} -> {back:?}");
            assert!((back.g - c.g).abs() < 1e-3);
            assert!((back.b - c.b).abs() < 1e-3);
        }
    }

    #[test]
    fn luminance_bounds() {
        assert!(Rgba::WHITE.luminance() > 0.9);
        assert!(Rgba::BLACK.luminance().abs() < 1e-6);
    }

    #[test]
    fn clamped_bounds_channels() {
        let c = Rgba::rgba(1.5, -0.2, 0.5, 2.0).clamped();
        assert_eq!((c.r, c.g, c.b, c.a), (1.0, 0.0, 0.5, 1.0));
    }
}
