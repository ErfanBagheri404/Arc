//! Shelf tab: pinned file shortcuts as a grid of thumbnail tiles.
//!
//! Pure builder, same contract as `ui::calendar`: `content(fw, fh, top, items)`
//! returns scene nodes in panel space and reads nothing else.
//!
//! A tile draws the file's own bytes as a thumbnail when WIC can decode them,
//! and a file glyph plus the file's name otherwise. Double-click opens; the
//! click routing lives in the app, the geometry here so drawing and hit-testing
//! cannot disagree.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::services::shelf::Item;
use crate::ui::{layout, line_height, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// Side of one thumbnail tile.
pub const TILE: f32 = 64.0;
/// Gap between tiles.
pub const TILE_GAP: f32 = 10.0;
/// Gap above the first tile row.
pub const TOP_PAD: f32 = 8.0;
/// How many characters of a file name fit under a tile.
pub const LABEL_CHARS: usize = 12;

/// The grey track a tile sits on.
const TILE_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);
/// A missing file's tile: red-tinted, so a stale pin is visible at a glance.
const GONE_FILL: Rgba = Rgba::rgba(0.95, 0.35, 0.35, 0.12);

/// How many tiles fit per row in a panel of width `fw`.
pub fn per_row(fw: f32) -> usize {
    let inner = fw - PANEL_PAD * 2.0;
    ((inner + TILE_GAP) / (TILE + TILE_GAP)).floor().max(1.0) as usize
}

/// Each tile's rect in panel space, in item order. Drawing and hit-testing
/// both read this, so a click can never land on a different item than the one
/// drawn.
pub fn tiles(fw: f32, top: f32, count: usize) -> Vec<Rect> {
    let cols = per_row(fw);
    (0..count)
        .map(|i| {
            let col = i % cols;
            let row = i / cols;
            Rect::new(
                PANEL_PAD + (TILE + TILE_GAP) * col as f32,
                top + TOP_PAD + (TILE + TILE_GAP) * row as f32,
                TILE,
                TILE,
            )
        })
        .collect()
}

/// The Shelf tab's content, laid out under `top`.
pub fn content(fw: f32, fh: f32, top: f32, items: &[Item]) -> Vec<Node> {
    let mut out = Vec::new();
    if items.is_empty() {
        let style = TextStyle::numeric(12.0);
        let text = "Shelf is empty — drop a file on the island to pin it".to_string();
        let th = line_height(&style);
        out.push(Node::Text {
            rect: put(
                fw,
                fh,
                Rect::new(PANEL_PAD, top + TOP_PAD, fw - PANEL_PAD * 2.0, th),
            ),
            text,
            style: TextStyle {
                color: Rgba::rgba(1.0, 1.0, 1.0, 0.6),
                ..style
            },
        });
        return out;
    }
    let th = line_height(&TextStyle::numeric(10.0));
    for (item, r) in items.iter().zip(tiles(fw, top, items.len())) {
        out.push(Node::RoundRect {
            rect: put(fw, fh, r),
            radii: CornerRadii::uniform(8.0),
            fill: layout::fade(if item.exists() { TILE_TRACK } else { GONE_FILL }, 1.0),
        });
        if item.thumb.0 != 0 {
            out.push(Node::Image {
                rect: put(fw, fh, r),
                handle: item.thumb,
                radii: CornerRadii::uniform(8.0),
            });
        } else {
            // No thumbnail WIC could decode: show the extension, which is the
            // part of a file name that says what it is.
            let ext = item
                .path
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_uppercase())
                .filter(|e| !e.is_empty() && e.len() <= 5)
                .unwrap_or_else(|| "FILE".to_string());
            let style = TextStyle::numeric(11.0);
            let lh = line_height(&style);
            out.push(Node::Text {
                rect: put(
                    fw,
                    fh,
                    Rect::new(r.x, r.y + (r.h - lh) / 2.0, r.w, lh),
                ),
                text: ext,
                style: TextStyle {
                    color: Rgba::rgba(1.0, 1.0, 1.0, 0.5),
                    ..style
                },
            });
        }
        let mut label = item.label();
        if label.chars().count() > LABEL_CHARS {
            label = label.chars().take(LABEL_CHARS).collect::<String>() + "…";
        }
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(r.x, r.y + r.h + 2.0, r.w, th)),
            text: label,
            style: TextStyle {
                size: 10.0,
                color: Rgba::rgba(1.0, 1.0, 1.0, if item.exists() { 0.8 } else { 0.4 }),
                ..TextStyle::default()
            },
        });
    }
    out
}

/// The tile under `(x, y)`, if any.
pub fn tile_at(fw: f32, top: f32, items: &[Item], x: f32, y: f32) -> Option<usize> {
    tiles(fw, top, items.len())
        .iter()
        .position(|r| r.contains(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scene::ImageHandle;

    fn item(path: &str, thumb: bool) -> Item {
        Item {
            path: path.into(),
            thumb: if thumb { ImageHandle(7) } else { ImageHandle::default() },
        }
    }

    #[test]
    fn empty_shelf_shows_a_hint_not_an_empty_grid() {
        let nodes = content(400.0, 300.0, 0.0, &[]);
        assert_eq!(nodes.len(), 1);
        if let Node::Text { text, .. } = &nodes[0] {
            assert!(text.contains("drop a file"));
        } else {
            panic!("expected text");
        }
    }

    #[test]
    fn tiles_flow_left_to_right_then_down() {
        let rects = tiles(400.0, 0.0, 9);
        assert!(rects[0].x < rects[1].x, "same row, to the right");
        let cols = per_row(400.0);
        assert!(
            rects[cols].y > rects[0].y,
            "a new row starts below: cols={cols}"
        );
    }

    #[test]
    fn an_image_tile_draws_its_thumbnail_and_a_plain_one_does_not() {
        let nodes = content(400.0, 400.0, 0.0, &[item("a.png", true), item("b.txt", false)]);
        // img: track + image + label = 3. txt: track + glyph text + label = 3.
        assert_eq!(nodes.len(), 6);
        assert!(matches!(nodes[1], Node::Image { .. }));
    }

    #[test]
    fn hit_testing_returns_the_tile_under_the_cursor() {
        let items = vec![item("a.png", true), item("b.png", true)];
        let r = tiles(400.0, 0.0, 2)[1];
        assert_eq!(tile_at(400.0, 0.0, &items, r.x + 2.0, r.y + 2.0), Some(1));
        assert_eq!(tile_at(400.0, 0.0, &items, -5.0, -5.0), None);
    }

    #[test]
    fn long_names_are_truncated() {
        let long = "n".repeat(LABEL_CHARS + 8);
        let nodes = content(400.0, 400.0, 0.0, &[item(&long, false)]);
        let label = nodes
            .iter()
            .filter_map(|n| match n {
                Node::Text { text, .. } => Some(text),
                _ => None,
            })
            .next_back()
            .unwrap();
        assert!(label.ends_with('…'));
    }
}