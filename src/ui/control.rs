//! Control tab: quick settings — connectivity, display, sound.
//!
//! Layout: a row of four pills (Wi-Fi, Bluetooth, Notch pin, Mute) on top,
//! then two sliders (Volume, Brightness) below. Pure — the app thread applies
//! any change and mirrors the new state back into `ViewState`.

use super::{put, ControlHit, ViewState, PANEL_PAD};
use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle, Weight};

const PILL_H: f32 = 40.0;
const PILL_GAP: f32 = 6.0;
const PILL_ICON: f32 = 14.0;
/// Track thickness of the two sliders.
const TRACK_H: f32 = 4.0;
/// Vertical pitch between the two slider tracks (label sits above each).
const TRACK_PITCH: f32 = 22.0;

/// The four pills' rects in panel space, left to right.
pub fn pills(fw: f32, top: f32) -> Vec<(f32, f32, f32, f32)> {
    let inner_w = (fw - PANEL_PAD * 2.0).max(0.0);
    let w = (inner_w - PILL_GAP * 3.0) / 4.0;
    (0..4)
        .map(|i| {
            let x = PANEL_PAD + i as f32 * (w + PILL_GAP);
            (x, top, w, PILL_H)
        })
        .collect()
}

/// Bottom edge of the pill row, so the sliders can anchor below it.
pub fn pills_bottom(fw: f32, top: f32) -> f32 {
    top + PILL_H
}

/// The two slider tracks' rects in panel space: `[volume, brightness]`.
pub fn sliders(fw: f32, top: f32) -> [Rect; 2] {
    let inner_w = (fw - PANEL_PAD * 2.0).max(0.0);
    // Label left, value right, track between: the track takes the remainder.
    let label_w = 68.0;
    let value_w = 44.0;
    let track_x = PANEL_PAD + label_w + 8.0;
    let track_w = (inner_w - label_w - 8.0 - value_w - 6.0).max(40.0);
    [
        Rect::new(track_x, top + (TRACK_PITCH - TRACK_H) / 2.0, track_w, TRACK_H),
        Rect::new(
            track_x,
            top + TRACK_PITCH + (TRACK_PITCH - TRACK_H) / 2.0,
            track_w,
            TRACK_H,
        ),
    ]
}

/// The value text rects, right of each track.
fn slider_values(fw: f32, top: f32) -> [Rect; 2] {
    let tracks = sliders(fw, top);
    [0, 1].map(|i| Rect::new(tracks[i].x + tracks[i].w + 6.0, tracks[i].y - 5.0, 44.0, 14.0))
}

pub fn content(fw: f32, fh: f32, top: f32, state: &ViewState) -> Vec<Node> {
    let mut out = Vec::new();
    let mut st = TextStyle::default();
    st.size = 11.0;
    st.weight = Weight::Semibold;
    st.color = Rgba::rgba(0.91, 0.91, 0.91, 1.0);
    let mut sub = st.clone();
    sub.size = 9.5;
    sub.weight = Weight::Regular;
    sub.color = Rgba::rgba(0.62, 0.62, 0.66, 1.0);

    let labels = ["Wi-Fi", "Bluetooth", "Notch", "Volume"];
    let icons = ["wifi", "bluetooth", "pin", "note"];
    let subs = [
        state.ssid.clone().unwrap_or_else(|| "off".into()),
        "not available".into(),
        if state.pinned { "pinned" } else { "unpinned" }.to_string(),
        if state.muted {
            "muted".to_string()
        } else {
            format!("{}%", state.volume_pct)
        },
    ];

    let pill_rects = pills(fw, top);
    for (i, (x, y, w, h)) in pill_rects.iter().enumerate() {
        // Hover is the only fill above the noise floor (base 0.06 quantizes to
        // the black body in a screenshot) — same rule as the row hover.
        let hover = state.hover_ctl == Some(ControlHit::Pill(i));
        out.push(Node::RoundRect {
            rect: put(fw, fh, Rect::new(*x, *y, *w, *h)),
            radii: CornerRadii::uniform(10.0),
            fill: Rgba::rgba(
                1.0,
                1.0,
                1.0,
                if hover { 0.16 } else { 0.06 },
            ),
        });
        out.push(Node::Glyph {
            rect: put(
                fw,
                fh,
                Rect::new(x + 10.0, y + (PILL_H - PILL_ICON) * 0.5, PILL_ICON, PILL_ICON),
            ),
            name: icons[i],
            color: Rgba::rgba(0.91, 0.91, 0.91, 1.0),
        });
        // Two stacked lines: name over sub-state.
        let tx = x + 10.0 + PILL_ICON + 8.0;
        let tw = (w - (tx - x) - 4.0).max(8.0);
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(tx, y + 6.0, tw, 14.0)),
            text: labels[i].to_string(),
            style: st.clone(),
        });
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(tx, y + 20.0, tw, 12.0)),
            text: subs[i].clone(),
            style: sub.clone(),
        });
    }

    // Sliders.
    let track_top = pills_bottom(fw, top) + 14.0;
    let tracks = sliders(fw, track_top);
    let value_rects = slider_values(fw, track_top);
    let values = [
        state.volume_pct as f32 / 100.0,
        state.bright_pct.unwrap_or(0) as f32 / 100.0,
    ];
    let names = ["Volume", "Brightness"];
    let texts = [
        if state.muted {
            "muted".to_string()
        } else {
            format!("{}%", state.volume_pct)
        },
        match state.bright_pct {
            Some(v) => format!("{v}%"),
            None => "—".to_string(),
        },
    ];
    for i in 0..2 {
        let mut lst = st.clone();
        lst.size = 10.0;
        lst.weight = Weight::Regular;
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(PANEL_PAD, tracks[i].y - 5.0, 68.0, 14.0)),
            text: names[i].to_string(),
            style: lst,
        });
        if state.bright_pct.is_none() && i == 1 {
            // No DDC brightness on this display: dim track, no value.
            out.push(Node::Bar {
                rect: put(fw, fh, tracks[i]),
                progress: 0.0,
                track: Rgba::rgba(1.0, 1.0, 1.0, 0.08),
                fill: Rgba::rgba(1.0, 1.0, 1.0, 0.0),
                radii: CornerRadii::uniform(2.0),
            });
            continue;
        }
        let hover = matches!(state.hover_ctl, Some(ControlHit::Slider(h, _)) if h == i);
        out.push(Node::Bar {
            rect: put(fw, fh, tracks[i]),
            progress: values[i].clamp(0.0, 1.0),
            track: Rgba::rgba(1.0, 1.0, 1.0, if hover { 0.24 } else { 0.14 }),
            fill: Rgba::rgba(0.91, 0.91, 0.91, 0.9),
            radii: CornerRadii::uniform(2.0),
        });
        let mut vst = st.clone();
        vst.size = 10.0;
        vst.weight = Weight::Regular;
        vst.tabular = true;
        vst.color = Rgba::rgba(0.62, 0.62, 0.66, 1.0);
        out.push(Node::Text {
            rect: put(fw, fh, value_rects[i]),
            text: texts[i].clone(),
            style: vst,
        });
    }
    out
}

/// Which pill a panel-space point lands on, if any. Same rects as [`pills`].
pub fn pill_at(fw: f32, top: f32, x: f32, y: f32) -> Option<usize> {
    pills(fw, top)
        .into_iter()
        .position(|(rx, ry, w, h)| x >= rx && x <= rx + w && y >= ry && y <= ry + h)
}

/// Which slider a panel-space point lands on, with the fraction of the track
/// the x maps to: `Some((0, frac))` = volume, `Some((1, frac))` = brightness.
pub fn slider_at(fw: f32, top: f32, x: f32, y: f32) -> Option<(usize, f32)> {
    let tracks = sliders(fw, top);
    for (i, r) in tracks.iter().enumerate() {
        // Generous vertical pad: the track is 4 px and needs a wider target.
        if x >= r.x - 4.0 && x <= r.x + r.w + 4.0 && y >= r.y - 8.0 && y <= r.y + r.h + 8.0 {
            let frac = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0);
            return Some((i, frac));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FW: f32 = 640.0;

    #[test]
    fn pill_at_round_trips_the_four_rects() {
        let rects = pills(FW, 40.0);
        assert_eq!(rects.len(), 4);
        for (i, (x, y, w, h)) in rects.iter().enumerate() {
            assert_eq!(pill_at(FW, 40.0, x + w * 0.5, y + h * 0.5), Some(i));
        }
        assert!(pill_at(FW, 40.0, 0.0, 0.0).is_none());
    }

    #[test]
    fn slider_at_maps_the_track_fraction() {
        let track_top = pills_bottom(FW, 40.0) + 14.0;
        let tracks = sliders(FW, track_top);
        let (which, frac) = slider_at(FW, track_top, tracks[0].x, tracks[0].y).unwrap();
        assert_eq!(which, 0);
        assert!(frac.abs() < 0.02);
        let (which, frac) =
            slider_at(FW, track_top, tracks[0].x + tracks[0].w, tracks[0].y).unwrap();
        assert_eq!(which, 0);
        assert!(frac > 0.98);
        assert!(slider_at(FW, track_top, tracks[0].x, tracks[0].y - 30.0).is_none());
    }
}
