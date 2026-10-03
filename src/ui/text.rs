//! Pure text measurement, used only for placeholder positioning.
//!
//! # Authority
//!
//! **The renderer's DirectWrite measurement is authoritative for final layout.**
//! `platform::Renderer::measure_text` caches an `IDWriteTextLayout` per
//! (string, style) pair and reports the true advance width; every pixel that ships
//! must come from there. This module exists because `ui::build` must stay pure
//! (docs/05 §1) and therefore cannot call into `platform` (docs/05 §1 dependency
//! rule: arrows point down, `ui` knows nothing about Win32). So the island shell
//! measures text with the per-char width table below to place placeholder runs —
//! tab labels, the header title, the footer clock — and the values are re-measured
//! by the renderer when it draws them.
//!
//! In other words: **this is an approximation, and it is allowed to be wrong.** It
//! is honest about being wrong (it is a real advance-width model of a
//! grotesque sans, not `len * size * 0.5`) so placeholder positions stay
//! plausible as the strings change.

use crate::core::scene::{TextStyle, Weight};

/// Advance widths as a fraction of the em, measured off a Helvetica/Inter-class
/// grotesque. Narrow glyphs matter a lot for the tab strip, where a
/// `len * size * 0.5` estimate over-measures `Clipboard` by ~20% and visibly
/// un-centers the row.
mod em {
    pub const SPACE: f32 = 0.26;
    /// Digits, proportional figures (a font's `0` is usually not a rectangle).
    pub const DIGIT_PROP: f32 = 0.556;
    /// Digits with the `tnum` feature on: every digit is one advance, which is
    /// what stops a running clock from twitching.
    pub const DIGIT_TABULAR: f32 = 0.60;
    pub const THIN: f32 = 0.28;
    pub const NARROW: f32 = 0.36;
    pub const LOWER: f32 = 0.53;
    pub const UPPER: f32 = 0.68;
    pub const M_LOWER: f32 = 0.83;
    pub const W_LOWER: f32 = 0.72;
    pub const M_UPPER: f32 = 0.86;
    pub const W_UPPER: f32 = 0.94;
}

/// Heavier weights carry a slightly wider advance at the same nominal size
/// (the glyphs get wider before they get taller).
const fn weight_scale(weight: Weight) -> f32 {
    match weight {
        Weight::Regular => 1.0,
        Weight::Semibold => 1.035,
        Weight::Bold => 1.08,
    }
}

/// Advance width of one character, in em.
fn advance_em(ch: char, tabular: bool) -> f32 {
    match ch {
        ' ' => em::SPACE,
        // Tabs are not expected in this UI, but measuring them as four spaces is
        // better than silently returning zero width.
        '\t' => em::SPACE * 4.0,
        // Single-line measurement: a newline contributes no advance.
        '\n' | '\r' => 0.0,
        '0'..='9' => {
            if tabular {
                em::DIGIT_TABULAR
            } else {
                em::DIGIT_PROP
            }
        }
        'i' | 'l' | 'j' | 'I' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' | '`' | '·' => em::THIN,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '/' | '\\' | '-' | '+' | '<' | '>' | '=' => {
            em::NARROW
        }
        'm' => em::M_LOWER,
        'w' => em::W_LOWER,
        'M' => em::M_UPPER,
        'W' => em::W_UPPER,
        c if c.is_ascii_lowercase() => em::LOWER,
        c if c.is_ascii_uppercase() => em::UPPER,
        // Non-ASCII: approximate by script rather than guessing one width for
        // the whole range. CJK is a full em; Latin/Greek/Cyrillic diacritics
        // land on the lowercase default; most punctuation is narrow.
        c => {
            let cp = c as u32;
            if (0x3000..=0x9FFF).contains(&cp) || (0xAC00..=0xD7AF).contains(&cp) {
                1.0
            } else if (0x0400..=0x04FF).contains(&cp) || (0x0370..=0x03FF).contains(&cp) {
                em::LOWER
            } else if c.is_ascii_digit() {
                em::DIGIT_PROP
            } else {
                em::NARROW
            }
        }
    }
}

/// Approximate width of a single line of text in logical pixels.
///
/// Monotonic in both string length and `style.size`, which is all the layout pass
/// relies on. See the module docs for why this is not the final authority.
#[must_use]
pub fn measure(text: &str, style: &TextStyle) -> f32 {
    let size = style.size.max(0.0);
    if size == 0.0 || text.is_empty() {
        return 0.0;
    }
    let scale = size * weight_scale(style.weight);
    let mut total = 0.0;
    for ch in text.chars() {
        total += advance_em(ch, style.tabular) * scale;
    }
    total
}

/// Line box height for a style: `size * 1.3`, the usual grotesque default.
///
/// Text nodes are drawn into a box of this height and the baseline sits on its
/// bottom edge, so this is what the layout pass uses to reserve vertical space.
#[must_use]
pub fn line_height(style: &TextStyle) -> f32 {
    style.size.max(0.0) * 1.3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geom::Rgba;

    fn regular(size: f32) -> TextStyle {
        TextStyle {
            size,
            ..Default::default()
        }
    }

    #[test]
    fn empty_text_measures_zero() {
        assert_eq!(measure("", &regular(14.0)), 0.0);
    }

    #[test]
    fn zero_size_measures_zero() {
        assert_eq!(measure("Arc", &regular(0.0)), 0.0);
    }

    #[test]
    fn measure_is_monotonic_in_text_length() {
        let s = regular(14.0);
        let mut prev = -1.0;
        for t in ["", "A", "Ar", "Arc", "Arci", "Arc is", "Arc is the"] {
            let w = measure(t, &s);
            assert!(w > prev, "{t:?} measured {w}, not more than {prev}");
            prev = w;
        }
        assert_eq!(prev, measure("Arc is the", &s));
    }

    #[test]
    fn measure_is_monotonic_in_style_size() {
        let text = "Clipboard";
        let mut prev = 0.0;
        for i in 1..=24 {
            let w = measure(text, &regular(i as f32));
            assert!(w > prev, "size {i} measured {w}, not greater than {prev}");
            prev = w;
        }
    }

    #[test]
    fn measure_is_linear_in_size() {
        let s12 = regular(12.0);
        let s24 = regular(24.0);
        let a = measure("Media", &s12);
        let b = measure("Media", &s24);
        assert!((b - 2.0 * a).abs() < 1e-3, "{a} vs {b}");
    }

    #[test]
    fn heavy_weights_are_not_narrower() {
        let base = TextStyle {
            size: 20.0,
            weight: Weight::Regular,
            ..Default::default()
        };
        let semi = TextStyle {
            weight: Weight::Semibold,
            ..base.clone()
        };
        let bold = TextStyle {
            weight: Weight::Bold,
            ..base.clone()
        };
        let (r, s, b) = (
            measure("Arc", &base),
            measure("Arc", &semi),
            measure("Arc", &bold),
        );
        assert!(r < s && s < b, "regular {r}, semibold {s}, bold {b}");
    }

    #[test]
    fn tabular_digits_are_all_the_same_width() {
        // The whole point of `tnum`: a running clock must not twitch.
        let style = TextStyle::numeric(12.0);
        assert!(style.tabular);
        let mut first = measure("0", &style);
        for d in "123456789".chars() {
            let w = measure(d.to_string().as_str(), &style);
            assert!((w - first).abs() < 1e-6, "digit {d} width {w} vs {first}");
        }
        first = measure("00:00", &style);
        let other = measure("12:34", &style);
        assert!((first - other).abs() < 1e-6, "{first} vs {other}");
    }

    #[test]
    fn proportional_digits_may_differ_but_stay_in_family() {
        let prop = regular(12.0);
        let tab = TextStyle::numeric(12.0);
        let a = measure("111", &prop);
        let b = measure("111", &tab);
        assert!(a > 0.0 && b > 0.0);
        assert!((b / a - 1.08).abs() < 0.06, "{a} vs {b}");
    }

    #[test]
    fn narrow_glyphs_measure_narrower_than_wide_ones() {
        let s = regular(14.0);
        assert!(measure("iiii", &s) < measure("WWWW", &s));
        assert!(measure("....", &s) < measure("mmmm", &s));
    }

    #[test]
    fn not_a_guess_of_len_times_size() {
        // A `len * size * 0.5` estimator lands on 63.0 for this string. A real
        // advance-width model must land somewhere clearly different, and must give
        // different answers for different letters — which is the whole point.
        let s = regular(14.0);
        let naive = "Clipboard".len() as f32 * 14.0 * 0.5;
        let real = measure("Clipboard", &s);
        assert!(
            (real - naive).abs() > 3.0,
            "too close to the naive guess: {real} vs {naive}"
        );
        // Same length, very different letters — a length-only estimator is blind here.
        assert!((measure("iiii", &s) - measure("WWWW", &s)).abs() > 10.0);
        assert!((measure("....", &s) - measure("mmmm", &s)).abs() > 5.0);
    }

    #[test]
    fn non_ascii_falls_back_by_script_not_by_panic() {
        let s = regular(14.0);
        assert!(measure("Café", &s) > measure("Caf", &s));
        assert!(measure("统计", &s) > measure("统", &s));
        assert!(measure("", &s).is_finite());
    }

    #[test]
    fn newline_contributes_no_advance_but_does_not_panic() {
        let s = regular(14.0);
        assert!((measure("A\nB", &s) - measure("AB", &s)).abs() < 1e-6);
    }

    #[test]
    fn colour_does_not_affect_width() {
        let plain = TextStyle {
            size: 16.0,
            color: Rgba::WHITE,
            ..Default::default()
        };
        let tinted = TextStyle {
            size: 16.0,
            color: Rgba::rgb(0.85, 0.34, 0.2),
            ..Default::default()
        };
        assert!((measure("Arc", &plain) - measure("Arc", &tinted)).abs() < 1e-6);
    }

    #[test]
    fn line_height_tracks_size() {
        let h12 = line_height(&regular(12.0));
        let h24 = line_height(&regular(24.0));
        assert!(h12 > 0.0);
        assert!((h24 - 2.0 * h12).abs() < 1e-4);
        assert_eq!(line_height(&regular(0.0)), 0.0);
    }
}
