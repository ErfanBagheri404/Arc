//! Spring animation integrator.
//!
//! Parameters are named after the reference design so behavior parity is checkable
//! line-by-line: `response` (seconds) and `dampingFraction` (1.0 = critically damped).
//!
//! Physics: mass 1, angular frequency `w0 = 2*PI / response`, damping ratio `z = dampingFraction`.
//! Integrated semi-implicitly (stable at large dt), which matches how UI toolkits
//! approximate springs in real time.

/// Spring configuration. Mirrors the reference design's parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringConfig {
    /// Period-ish response time in seconds. Larger = slower.
    pub response: f32,
    /// Damping ratio. <1 overshoots, 1.0 settles without overshoot, >1 sluggish.
    pub damping_fraction: f32,
}

impl SpringConfig {
    pub const fn new(response: f32, damping_fraction: f32) -> Self {
        Self {
            response,
            damping_fraction,
        }
    }

    /// Reference values, verbatim from the reference design's open/close/generic transitions.
    pub const OPEN: Self = Self::new(0.24, 0.9);
    pub const CLOSE: Self = Self::new(0.45, 1.0);
    pub const GENERIC: Self = Self::new(0.34, 0.88);

    /// Overdamped swap used for content crossfades.
    pub const SWAP: Self = Self::new(0.34, 1.2);

    /// The island's pop: a loose spring that rings once and settles. Measured
    /// off the reference morph (vnotch-dynamic.gif frames 114-127, the pill's
    /// top edge pinned at row 3 the whole time so only the bottom moves):
    /// rest 134x24, peak 149x27 at +100 ms, trough 129x23 at +300 ms, back to
    /// rest by +440 ms. That is scale 1.112 / 0.963. Solving the ring for a
    /// velocity kick -- peak-to-trough ratio fixes zeta, the half-period fixes
    /// the damped frequency -- gives response 0.38 s, damping 0.33.
    pub const POP: Self = Self::new(0.38, 0.33);

    fn w0(self) -> f32 {
        std::f32::consts::TAU / self.response.max(f32::MIN_POSITIVE)
    }
}

/// A scalar value easing toward a target.
#[derive(Debug, Clone)]
pub struct Spring {
    value: f32,
    target: f32,
    velocity: f32,
    config: SpringConfig,
}

impl Spring {
    pub fn new(initial: f32, config: SpringConfig) -> Self {
        Self {
            value: initial,
            target: initial,
            velocity: 0.0,
            config,
        }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn config(&self) -> SpringConfig {
        self.config
    }

    pub fn set_config(&mut self, config: SpringConfig) {
        self.config = config;
    }

    /// Retarget without discontinuity in velocity.
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Kick the spring with an initial velocity, leaving value and target
    /// alone. A spring at rest that is given a nudge rings: it overshoots once
    /// and settles back. That is the island's pop (the reference notch
    /// inflates ~11% and dips ~4% before it morphs).
    pub fn set_velocity(&mut self, velocity: f32) {
        self.velocity = velocity;
    }

    /// Jump instantly, killing velocity (used on layout/size changes).
    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// True while still moving: needs another frame.
    ///
    /// The thresholds are fractions of the spring's own travel, not pixels: a
    /// scale spring (`pop`, resting at 1.0) would otherwise be born "settled"
    /// and lose the kick that makes it ring. One percent of travel is below
    /// what any spring here is visible at.
    pub fn is_animating(&self) -> bool {
        let magnitude = self.target.abs().max(self.value.abs()).max(1e-6);
        let pos_eps = (SETTLE_FRACTION * magnitude).clamp(SETTLE_FLOOR, SETTLE_CEIL);
        let vel_eps = pos_eps * self.config.w0() * VELOCITY_EPS_FACTOR;
        (self.value - self.target).abs() > pos_eps || self.velocity.abs() > vel_eps
    }

    /// Advance by `dt` seconds. Returns true if the spring is still animating.
    pub fn step(&mut self, dt: f32) -> bool {
        // Substepped so a big `dt` cannot exceed the spring's own period in a
        // single step: semi-implicit Euler is stable only while dt < ~1/w0.
        // The open spring's period is ~0.7 s, so one step is capped at a
        // twentieth of that — a backgrounded window can hand us a dt of
        // seconds, and without this it gains energy and explodes.
        let dt = dt.clamp(0.0, MAX_DT);
        // A single big step is safe only when the spring is already at rest.
        // Otherwise substep so no step exceeds ~0.3 rad of the spring's own
        // period: semi-implicit Euler is stable only below that, and the open
        // spring's period is ~0.7 s. A backgrounded window hands us a dt of
        // seconds — without this it gains energy and explodes.
        let at_rest = !self.is_animating();
        let w0 = self.config.w0();
        let steps = if at_rest {
            1
        } else {
            ((dt * w0) / 0.3).ceil().max(1.0) as usize
        };
        let h = dt / steps as f32;
        for _ in 0..steps {
            self.step_once(h);
            if !self.is_animating() {
                break;
            }
        }
        if self.is_animating() {
            true
        } else {
            self.value = self.target;
            self.velocity = 0.0;
            false
        }
    }

    /// One integration step. Does not settle on its own; see [`Self::step`].
    fn step_once(&mut self, dt: f32) -> bool {
        if !self.is_animating() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let w0 = self.config.w0();
        let k = w0 * w0;
        let c = 2.0 * self.config.damping_fraction * w0;
        // Semi-implicit Euler: velocity first, then position from new velocity.
        let accel = -k * (self.value - self.target) - c * self.velocity;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
        if !self.is_animating() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

/// How close to target counts as settled, as a fraction of the spring's own
/// travel. Scale springs (`pop`, resting at 1.0) and pixel springs (width,
/// height) both get a threshold that means the same thing on screen.
const SETTLE_FRACTION: f32 = 0.01;
/// Ceiling on the fraction, in the spring's own units: 0.05 px is where a
/// spring stops being visible on screen, and a wide pixel spring (the 185 px
/// pill) must not settle 1.85 px short of its target.
const SETTLE_CEIL: f32 = 0.05;
/// Floor on the fraction, in the spring's own units. A spring collapsing
/// *toward zero* shrinks its travel with it, so a pure fraction would drive
/// the threshold to zero and the spring could never report settled.
const SETTLE_FLOOR: f32 = 1e-4;
/// Velocity settle threshold = position epsilon × natural frequency × this factor.
/// Deriving it from ω₀ is what stops a slow spring from being chopped off
/// mid-flight while still letting a fast one exit early.
const VELOCITY_EPS_FACTOR: f32 = 4.0;
/// Clamp dt so a stalled frame (debugger, sleep) cannot explode the integrator.
const MAX_DT: f32 = 1.0 / 15.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_spring_does_not_animate() {
        let mut s = Spring::new(10.0, SpringConfig::OPEN);
        assert!(!s.is_animating());
        assert!(!s.step(0.016));
    }

    #[test]
    fn open_spring_reaches_target_within_300ms() {
        let mut s = Spring::new(0.0, SpringConfig::OPEN);
        s.set_target(100.0);
        let mut t = 0.0;
        while s.is_animating() && t < 2.0 {
            s.step(1.0 / 240.0);
            t += 1.0 / 240.0;
        }
        assert!(t < 0.3, "took {t}s");
        assert!((s.value() - 100.0).abs() < 0.06);
    }

    #[test]
    fn critically_damped_close_never_overshoots() {
        let mut s = Spring::new(640.0, SpringConfig::CLOSE);
        s.set_target(150.0);
        for _ in 0..600 {
            s.step(1.0 / 240.0);
            assert!(s.value() <= 640.0 + 1e-3, "overshot to {}", s.value());
            assert!(s.value() >= 150.0 - 0.05);
        }
        assert!((s.value() - 150.0).abs() < 0.06);
    }

    #[test]
    fn underdamped_spring_overshoots_then_settles() {
        // Its own config, not `OPEN`: the assertion is about underdamped
        // behaviour, and `OPEN` is deliberately only barely underdamped.
        let mut s = Spring::new(0.0, SpringConfig::new(0.34, 0.55));
        s.set_target(100.0);
        let mut peak = 0.0f32;
        for _ in 0..600 {
            s.step(1.0 / 240.0);
            peak = peak.max(s.value());
        }
        assert!(peak > 100.5, "expected overshoot, peak {peak}");
        assert!((s.value() - 100.0).abs() < 0.06);
    }

    #[test]
    fn snap_kills_velocity() {
        let mut s = Spring::new(0.0, SpringConfig::OPEN);
        s.set_target(100.0);
        s.step(0.016);
        s.snap(42.0);
        assert_eq!(s.value(), 42.0);
        assert_eq!(s.velocity(), 0.0);
        assert!(!s.is_animating());
    }

    #[test]
    fn large_dt_is_clamped_not_exploded() {
        let mut s = Spring::new(0.0, SpringConfig::GENERIC);
        s.set_target(100.0);
        s.step(1000.0);
        assert!(s.value().is_finite());
        assert!(s.value() <= 100.5);
    }

    #[test]
    fn response_zero_does_not_divide_by_zero() {
        let mut s = Spring::new(0.0, SpringConfig::new(0.0, 0.8));
        s.set_target(10.0);
        s.step(0.016);
        assert!(s.value().is_finite());
    }

    /// The pop's whole point: a kick at rest must ring, not be swallowed.
    ///
    /// Regression: the settle threshold used to be a pixel constant, so a
    /// scale spring resting at 1.0 read as "already settled" and `step_once`
    /// zeroed the kick on the first frame. The island never popped at all.
    #[test]
    fn a_kicked_scale_spring_rings_instead_of_snapping() {
        let mut s = Spring::new(1.0, SpringConfig::POP);
        s.set_velocity(3.0);
        assert!(s.is_animating(), "the kick must survive to the first step");

        let (mut peak, mut trough) = (1.0f32, 1.0f32);
        let mut t = 0.0;
        while s.is_animating() && t < 2.0 {
            s.step(1.0 / 240.0);
            t += 1.0 / 240.0;
            peak = peak.max(s.value());
            trough = trough.min(s.value());
        }
        // vnotch-dynamic.gif f117/f123: 149/134 and 129/134.
        assert!((peak - 1.112).abs() < 0.01, "peak {peak}");
        assert!((trough - 0.963).abs() < 0.01, "trough {trough}");
        assert!((s.value() - 1.0).abs() < 1e-4, "must settle exactly back");
    }

    /// A pixel spring still lands on its target: the fraction must not be so
    /// coarse that the 185 px pill settles short of 185.
    #[test]
    fn a_wide_pixel_spring_still_lands_on_target() {
        let mut s = Spring::new(0.0, SpringConfig::GENERIC);
        s.set_target(185.0);
        let mut t = 0.0;
        while s.is_animating() && t < 2.0 {
            s.step(1.0 / 240.0);
            t += 1.0 / 240.0;
        }
        assert!((s.value() - 185.0).abs() < 0.06, "{}", s.value());
    }

    /// A spring collapsing to zero must still terminate: with a pure fraction
    /// its travel shrinks toward the threshold, and `is_animating` would stay
    /// true forever (the island would never finish closing).
    #[test]
    fn a_spring_collapsing_to_zero_still_settles() {
        let mut s = Spring::new(185.0, SpringConfig::GENERIC);
        s.set_target(0.0);
        let mut t = 0.0;
        while s.is_animating() && t < 2.0 {
            s.step(1.0 / 240.0);
            t += 1.0 / 240.0;
        }
        assert!(t < 2.0, "collapsing to zero never settled");
        assert!(s.value().abs() < 0.06, "{}", s.value());
    }
}
