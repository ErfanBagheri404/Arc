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

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const BLACK: Self = Self::rgba(0.0, 0.0, 0.0, 1.0);
    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);

    /// The reference design's hover micro-fill: 2% at rest, 24% on hover.
    pub fn black_hover(is_hover: bool) -> Self {
        Self::rgba(0.0, 0.0, 0.0, if is_hover { 0.24 } else { 0.02 })
    }

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
    pub fn clamped(self, w: f32, h: f32) -> Self {
        let m = (w.min(h) / 2.0).max(0.0);
        Self {
            top_left: self.top_left.clamp(0.0, m),
            top_right: self.top_right.clamp(0.0, m),
            bottom_right: self.bottom_right.clamp(0.0, m),
            bottom_left: self.bottom_left.clamp(0.0, m),
        }
    }
}

/// Squircle (continuous corner) coefficient. 1.0 = circular arcs, ~0.55 ≈ Apple's
/// continuous curvature look, which is what the reference design renders.
pub const SQUIRCLE_K: f32 = 0.55;

/// One cubic Bézier segment of a rounded-rect outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathSegment {
    pub c1: (f32, f32),
    pub c2: (f32, f32),
    pub to: (f32, f32),
}

/// Build the outline of `rect` with continuous (squircle) corners as cubic Béziers.
/// Four segments, clockwise from the top-left corner.
pub fn squircle_segments(rect: &Rect, radii: CornerRadii) -> Vec<PathSegment> {
    let r = radii.clamped(rect.w, rect.h);
    let (x, y) = (rect.x, rect.y);
    let (max_x, max_y) = (rect.max_x(), rect.max_y());
    let k = SQUIRCLE_K;

    // Circle approximation constant scaled by the squircle factor.
    let c = 0.552_284_749_83 * k;

    let mut segs = Vec::with_capacity(4);

    // Top-left: (x + r.tl, y) → (x, y + r.tl)
    segs.push(PathSegment {
        c1: (x + r.top_left * (1.0 - c), y),
        c2: (x, y + r.top_left * (1.0 - c)),
        to: (x, y + r.top_left),
    });
    // Bottom-left: (x, max_y - r.bl) → (x + r.bl, max_y)
    segs.push(PathSegment {
        c1: (x, max_y - r.bottom_left * (1.0 - c)),
        c2: (x + r.bottom_left * (1.0 - c), max_y),
        to: (x + r.bottom_left, max_y),
    });
    // Bottom-right: (max_x - r.br, max_y) → (max_x, max_y - r.br)
    segs.push(PathSegment {
        c1: (max_x - r.bottom_right * (1.0 - c), max_y),
        c2: (max_x, max_y - r.bottom_right * (1.0 - c)),
        to: (max_x, max_y - r.bottom_right),
    });
    // Top-right: (max_x, y + r.tr) → (max_x - r.tr, y)
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
        let r2 = CornerRadii::uniform(-5.0).clamped(40.0, 32.0);
        assert_eq!(r2.bottom_right, 0.0);
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
    fn luminance_and_hover_fill() {
        assert!(Rgba::WHITE.luminance() > 0.9);
        assert!(Rgba::BLACK.luminance().abs() < 1e-6);
        assert!((Rgba::black_hover(false).a - 0.02).abs() < 1e-6);
        assert!((Rgba::black_hover(true).a - 0.24).abs() < 1e-6);
    }

    #[test]
    fn clamped_bounds_channels() {
        let c = Rgba::rgba(1.5, -0.2, 0.5, 2.0).clamped();
        assert_eq!((c.r, c.g, c.b, c.a), (1.0, 0.0, 0.5, 1.0));
    }
}
