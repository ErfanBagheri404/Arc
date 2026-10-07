//! Accent-color extraction from album art.
//!
//! Pure, no I/O — testable on any host. The algorithm downsamples the image to a
//! small grid, buckets pixels by saturation × count, and picks the most saturated
//! bucket whose average luminance is readable on a black background.

use crate::core::geom::Rgba;

/// Extract an accent color from BGRA pixel data.
///
/// `pixels` is `width * height * 4` bytes, BGRA order, premultiplied alpha.
/// Returns a color clamped to a readable luminance range (0.35–0.85) so text on
/// black stays legible.
#[must_use]
pub fn extract_accent(pixels: &[u8], width: u32, height: u32) -> Rgba {
    if pixels.is_empty() || width == 0 || height == 0 {
        return Rgba::rgb(0.8, 0.2, 0.3); // fallback warm red
    }
    // Downsample to at most 16×16 for speed.
    let step_x = (width / 16).max(1);
    let step_y = (height / 16).max(1);
    // Bucket: (sum_r, sum_g, sum_b, count) for pixels with saturation > 0.15.
    let mut buckets: Vec<(f32, f32, f32, u32)> = Vec::new();
    for y in (0..height).step_by(step_y as usize) {
        for x in (0..width).step_by(step_x as usize) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 3 >= pixels.len() {
                continue;
            }
            let b = pixels[idx] as f32 / 255.0;
            let g = pixels[idx + 1] as f32 / 255.0;
            let r = pixels[idx + 2] as f32 / 255.0;
            let a = pixels[idx + 3] as f32 / 255.0;
            if a < 0.1 {
                continue;
            }
            let (h, s, v) = Rgba::rgb(r, g, b).to_hsv();
            if s < 0.15 || v < 0.1 {
                continue;
            }
            // Quantize hue to 12 buckets.
            let bucket = ((h / 30.0) as usize).min(11);
            if bucket >= buckets.len() {
                buckets.resize(bucket + 1, (0.0, 0.0, 0.0, 0));
            }
            let entry = &mut buckets[bucket];
            entry.0 += r;
            entry.1 += g;
            entry.2 += b;
            entry.3 += 1;
        }
    }
    // Pick the bucket with the most pixels (most dominant saturated color).
    let Some(best) = buckets.iter().filter(|b| b.3 > 0).max_by_key(|b| b.3) else {
        return Rgba::rgb(0.8, 0.2, 0.3);
    };
    let count = best.3 as f32;
    let mut color = Rgba::rgb(best.0 / count, best.1 / count, best.2 / count);
    // Clamp luminance to readable range.
    let lum = color.luminance();
    if lum < 0.35 {
        // Blend toward white, not scale up: a pure red channel hits 1.0 before
        // its luminance (0.2126) can reach 0.35, so scaling stalls. Luminance is
        // linear in RGB, so a fixed blend hits the target exactly.
        let t = (0.35 - lum) / (1.0 - lum);
        color = Rgba::rgb(
            color.r + t * (1.0 - color.r),
            color.g + t * (1.0 - color.g),
            color.b + t * (1.0 - color.b),
        );
    } else if lum > 0.85 {
        let scale = 0.85 / lum;
        color = Rgba::rgb(color.r * scale, color.g * scale, color.b * scale);
    }
    color.clamped()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_fallback() {
        let c = extract_accent(&[], 0, 0);
        assert!(c.r > 0.5, "fallback should be warm red");
    }

    #[test]
    fn red_image_yields_red_accent() {
        // 4×4 all red.
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            pixels.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
        }
        let c = extract_accent(&pixels, 4, 4);
        assert!(c.r > 0.7, "red dominant: {c:?}");
        assert!(c.g < 0.3, "green low: {c:?}");
    }

    #[test]
    fn blue_image_yields_blue_accent() {
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            pixels.extend_from_slice(&[255, 0, 0, 255]); // BGRA blue
        }
        let c = extract_accent(&pixels, 4, 4);
        assert!(c.b > 0.7, "blue dominant: {c:?}");
    }

    #[test]
    fn dark_image_clamps_luminance() {
        // Very dark red.
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            pixels.extend_from_slice(&[0, 0, 30, 255]); // BGRA dark red
        }
        let c = extract_accent(&pixels, 4, 4);
        assert!(c.luminance() >= 0.34, "clamped up: {c:?}");
    }

    #[test]
    fn bright_image_clamps_luminance() {
        // Very bright green.
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            pixels.extend_from_slice(&[0, 255, 0, 255]); // BGRA bright green
        }
        let c = extract_accent(&pixels, 4, 4);
        assert!(c.luminance() <= 0.86, "clamped down: {c:?}");
    }

    #[test]
    fn grey_image_returns_fallback() {
        // All grey (saturation 0).
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            pixels.extend_from_slice(&[128, 128, 128, 255]);
        }
        let c = extract_accent(&pixels, 4, 4);
        assert!(c.r > 0.5, "grey → fallback: {c:?}");
    }
}
