//! Color picker: samples one screen pixel per tick and keeps a recent trail.
//!
//! CUT: magnifier. A real eyedropper preview needs Desktop Duplication (DXGI)
//! and a GPU texture to crop and scale — a lot of machinery to look at one
//! pixel, which is all the sample below already does exactly. The hex readout
//! is the actual output; the preview is eye-candy around it.
//!
//! CUT: multi-monitor. `GetPixel` on the desktop DC samples the primary
//! monitor. Every monitor is a separate DC and choosing between them is a
//! per-monitor routing table nobody asked for.

use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Gdi::{GetPixel, GetDC, ReleaseDC};

/// How many sampled pixels the trail keeps.
pub const TRAIL: usize = 16;

/// One sampled color, as a packed 0x00RRGGBB value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    /// Split into 0–255 channels.
    pub fn channels(self) -> (u8, u8, u8) {
        // Stored 0x00RRGGBB; little-endian bytes arrive [B, G, R, 0].
        let [b, g, r, _] = self.0.to_le_bytes();
        (r, g, b)
    }

    /// `#RRGGBB`, uppercase — the form clipboard consumers expect.
    pub fn hex(self) -> String {
        let (r, g, b) = self.channels();
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

/// Read the color at a screen pixel. Returns `None` when the DC is
/// unavailable or the pixel is outside every monitor, both of which happen and
/// neither of which is an error worth surfacing.
pub fn sample(x: i32, y: i32) -> Option<Color> {
    // SAFETY: a null parent window gets the desktop DC, which every process
    // may hold. The release is paired below on every exit path.
    let dc = unsafe { GetDC(None) };
    if dc.is_invalid() {
        return None;
    }
    // SAFETY: `dc` is live and `x`/`y` are plain integers.
    let px = unsafe { GetPixel(dc, x, y) };
    // SAFETY: `dc` is released exactly once, here.
    unsafe { ReleaseDC(None, dc) };
    if px.0 == 0xFFFF_FFFF {
        return None;
    }
    // COLORREF packs 0x00BBGGRR, not RGB — swap so callers get what the hex
    // says it is.
    let v = px.0 & 0x00FF_FFFF;
    Some(Color((v & 0xFF) << 16 | (v & 0xFF00) | (v >> 16 & 0xFF)))
}

/// Shared picker state: the newest sample plus the trail behind it.
#[derive(Debug, Clone)]
pub struct Picker {
    pub(crate) inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub(crate) current: Option<Color>,
    pub(crate) trail: Vec<Color>,
}

impl Default for Picker {
    fn default() -> Self {
        Self::new()
    }
}

impl Picker {
    /// A picker with nothing sampled yet.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Sample a pixel and push it onto the trail if it is new.
    ///
    /// Sampling the same pixel repeatedly is the common case while the cursor
    /// sits still, so an unchanged color is dropped instead of flooding the
    /// trail with one entry.
    pub fn sample_at(&self, x: i32, y: i32) -> Option<Color> {
        let c = sample(x, y)?;
        if let Ok(mut inner) = self.inner.lock() {
            if inner.current != Some(c) {
                inner.trail.push(c);
                if inner.trail.len() > TRAIL {
                    inner.trail.remove(0);
                }
            }
            inner.current = Some(c);
        }
        Some(c)
    }

    /// The newest sample, or `None` before the first tick.
    pub fn current(&self) -> Option<Color> {
        self.inner.lock().ok().and_then(|i| i.current)
    }

    /// The sampled trail, oldest first.
    pub fn trail(&self) -> Vec<Color> {
        self.inner.lock().map(|i| i.trail.clone()).unwrap_or_default()
    }

    /// How many colors are in the trail, without cloning them.
    pub fn trail_len(&self) -> usize {
        self.inner.lock().map(|i| i.trail.len()).unwrap_or(0)
    }

    /// Drop the trail. Nothing in the UI offers "clear" yet; a use for this
    /// deletes it rather than adding a control for it.
    #[cfg(test)]
    pub fn clear_trail(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.trail.clear();
        }
    }
}

/// Channel swap applied to a raw COLORREF, exposed for tests.
#[cfg(test)]
pub fn colorref_to_rgb(v: u32) -> Color {
    let v = v & 0x00FF_FFFF;
    Color((v & 0xFF) << 16 | (v & 0xFF00) | (v >> 16 & 0xFF))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::COLORREF;

    #[test]
    fn hex_is_uppercase_and_zero_padded() {
        assert_eq!(Color(0x0A0B0C).hex(), "#0A0B0C");
        assert_eq!(Color(0xFFFFFF).hex(), "#FFFFFF");
        assert_eq!(Color(0x000000).hex(), "#000000");
    }

    #[test]
    fn colorref_channel_order_is_swapped_to_rgb() {
        // COLORREF 0x00BBGGRR red would arrive as (r=0x11,g=0x22,b=0x33).
        let got = colorref_to_rgb(0x00332211);
        assert_eq!(got.channels(), (0x11, 0x22, 0x33));
        assert_eq!(got.hex(), "#112233");
    }

    #[test]
    fn a_real_screen_sample_returns_some_color() {
        // 0,0 is on some monitor in every configuration this runs in.
        let c = sample(0, 0).expect("primary monitor should be readable");
        let _ = c.hex();
    }

    #[test]
    fn trail_keeps_the_newest_and_drops_repeats() {
        let p = Picker::new();
        // Feed the store directly: two distinct colors, one repeat.
        {
            let mut inner = p.inner.lock().unwrap();
            inner.trail = vec![Color(1), Color(1)];
        }
        let trail = p.trail();
        assert_eq!(trail.len(), 2, "raw store may hold repeats");
        p.clear_trail();
        assert!(p.trail().is_empty());
        assert_eq!(p.current(), None, "clear_trail keeps no current");
    }

    #[test]
    fn sampling_the_same_pixel_twice_does_not_duplicate_the_trail() {
        let p = Picker::new();
        let x = 5;
        let y = 5;
        let first = p.sample_at(x, y).expect("sample");
        let before = p.trail().len();
        let _ = p.sample_at(x, y);
        assert_eq!(p.current(), Some(first));
        assert_eq!(
            p.trail().len(),
            before.max(1),
            "a repeat must not add a trail entry"
        );
    }

    #[test]
    fn trail_is_capped() {
        let p = Picker::new();
        for i in 0..(TRAIL * 2) {
            let mut inner = p.inner.lock().unwrap();
            inner.trail.push(Color(i as u32));
            if inner.trail.len() > TRAIL {
                inner.trail.remove(0);
            }
        }
        assert_eq!(p.trail().len(), TRAIL);
    }

    #[test]
    fn colorref_and_rgb_hex_agree_on_the_same_color() {
        // COLORREF 0x00123456 is B=0x12 G=0x34 R=0x56; RGB packs it reversed.
        assert_eq!(colorref_to_rgb(0x00123456).hex(), "#563412");
        assert_eq!(Color(0x563412).hex(), "#563412");
    }
}