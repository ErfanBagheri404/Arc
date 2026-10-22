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

    /// The island's pop: a fast, loose spring (zeta 0.34) that rings once and
    /// settles. Measured off the reference morph (vnotch-dynamic.gif frames
    /// 115-130): the pill inflates ~11%, dips ~4% below its rest size, then
    /// settles -- one half-cycle about 117 ms, settled inside 350 ms.
    pub const POP: Self = Self::new(0.22, 0.34);

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
    pub fn is_animating(&self) -> bool {
        let magnitude = self.target.abs().max(1.0);
        let pos_eps = SETTLE_EPSILON.max(SETTLE_EPSILON_ABS * magnitude);
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

/// Position settle threshold in logical pixels.
const SETTLE_EPSILON: f32 = 0.05;
/// Velocity settle threshold = position epsilon × natural frequency × this factor.
/// Deriving it from ω₀ is what stops a slow spring from being chopped off
/// mid-flight while still letting a fast one exit early.
const VELOCITY_EPS_FACTOR: f32 = 4.0;
/// Below this magnitude the spring is considered at rest no matter what.
const SETTLE_EPSILON_ABS: f32 = 0.0005;
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
}
