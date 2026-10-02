//! Per-monitor DPI awareness.
//!
//! The island sits at a screen's top edge, so it must be correct on mixed-DPI
//! setups (a 100% laptop panel with a 150% external). Arc opts into
//! PER_MONITOR_AWARE_V2 and converts physical↔logical pixels explicitly.

/// Scale factor for a monitor plus the device context it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dpi {
    pub scale: f32,
}

impl Default for Dpi {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

impl Dpi {
    /// Physical (device) pixels → logical pixels.
    pub fn to_logical(&self, physical: f32) -> f32 {
        if self.scale <= 0.0 {
            physical
        } else {
            physical / self.scale
        }
    }

    /// Logical pixels → physical pixels.
    pub fn to_physical(&self, logical: f32) -> f32 {
        logical * self.scale
    }

    /// Snap a logical size to whole physical pixels so a squircle edge never
    /// lands on a half-pixel and renders blurry.
    pub fn snap(&self, logical: f32) -> u32 {
        let px = self.to_physical(logical).round();
        (px.max(1.0)) as u32
    }

    /// Scale a DPI-derived value (96 dpi == 100% == scale 1.0).
    pub fn from_dpi(dpi_x: u32) -> Self {
        Self {
            scale: dpi_x as f32 / 96.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_at_96_dpi() {
        let d = Dpi::from_dpi(96);
        assert_eq!(d.scale, 1.0);
        assert_eq!(d.to_logical(100.0), 100.0);
        assert_eq!(d.to_physical(100.0), 100.0);
    }

    #[test]
    fn scales_150_percent() {
        let d = Dpi::from_dpi(144);
        assert!((d.scale - 1.5).abs() < 1e-6);
        assert_eq!(d.to_physical(100.0), 150.0);
        assert_eq!(d.to_logical(150.0), 100.0);
    }

    #[test]
    fn snap_rounds_and_never_zero() {
        assert_eq!(Dpi::default().snap(32.0), 32);
        assert_eq!(Dpi::from_dpi(144).snap(32.4), 49);
        assert_eq!(Dpi::default().snap(0.0), 1);
        assert_eq!(Dpi::default().snap(-5.0), 1);
    }

    #[test]
    fn zero_scale_does_not_divide_by_zero() {
        let d = Dpi { scale: 0.0 };
        assert_eq!(d.to_logical(42.0), 42.0);
    }
}
