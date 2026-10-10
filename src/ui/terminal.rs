//! Terminal tab: the ConPTY grid rendered as monospaced text runs.
//!
//! Pure builder, same contract as `ui::calendar`: `content(fw, fh, top, screen)`
//! returns scene nodes in panel space and reads nothing else. The cell grid
//! itself lives in `core::termscreen`; this module only decides where each cell
//! lands and what colour it is.
//!
//! The font size is derived from the panel width so all [`COLS`] columns fit:
//! a fixed size would clip the right edge on a narrow panel, and a fixed
//! column count is what makes the grid a *terminal* rather than a text block.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Node, TextStyle};
use crate::core::termscreen::{self, Screen, COLS, ROWS};
use crate::ui::{layout, PANEL_PAD};

/// Clamp a rect inside the frame, as every ui module does before emitting.
fn put(fw: f32, fh: f32, rect: Rect) -> Rect {
    layout::fit(rect, fw, fh)
}

/// Advance width of one mono cell, as a fraction of the em. Cascadia Mono and
/// Consolas both land here; the renderer falls back through a family list.
const CELL_EM: f32 = 0.6;
/// Line box height, as a multiple of the font size.
const LINE_EM: f32 = 1.3;
/// Gap between the tab strip and the first row.
pub const TOP_PAD: f32 = 6.0;
/// Background of the whole grid, a hair off the panel black so the terminal
/// reads as its own surface.
const GRID_FILL: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.03);

/// Font size that fits [`COLS`] columns inside `fw`.
pub fn font_size(fw: f32) -> f32 {
    let inner = (fw - PANEL_PAD * 2.0).max(1.0);
    inner / COLS as f32 / CELL_EM
}

/// One cell's box, given the grid origin.
fn cell_w(fw: f32) -> f32 {
    font_size(fw) * CELL_EM
}

/// How many grid rows fit between `top` and the footer.
pub fn visible_rows(fw: f32, fh: f32, top: f32) -> usize {
    let line = font_size(fw) * LINE_EM;
    let avail = (fh - crate::ui::FOOTER_BOTTOM_PAD - top - TOP_PAD).max(0.0);
    ((avail / line).floor() as usize).clamp(1, ROWS)
}

/// The Terminal tab's content, laid out under `top`.
///
/// `screen` is `None` before the shell has been started (or when it failed to
/// start), which draws the hint row instead of an empty grid.
pub fn content(fw: f32, fh: f32, top: f32, screen: Option<&Screen>, error: Option<&str>) -> Vec<Node> {
    let mut out = Vec::new();
    let size = font_size(fw);
    let line = size * LINE_EM;

    // The hint / error row replaces the grid entirely: a blank grid with no
    // explanation reads as a hung terminal.
    let (msg, bad) = match (screen, error) {
        (_, Some(e)) => (format!("shell failed to start: {e}"), true),
        (None, None) => ("starting shell…".to_string(), false),
        (Some(_), None) => (String::new(), false),
    };
    if !msg.is_empty() {
        let style = TextStyle {
            size: size.min(13.0),
            color: if bad {
                Rgba::rgba(0.95, 0.4, 0.4, 1.0)
            } else {
                Rgba::rgba(1.0, 1.0, 1.0, 0.5)
            },
            ..TextStyle::default()
        };
        let h = crate::ui::line_height(&style);
        out.push(Node::Text {
            rect: put(fw, fh, Rect::new(PANEL_PAD, top + TOP_PAD, fw - PANEL_PAD * 2.0, h)),
            text: msg,
            style,
        });
        return out;
    }
    let screen = screen.expect("checked above");

    let rows = visible_rows(fw, fh, top);
    // Bottom-aligned: shell output grows downward, so the newest lines are the
    // ones worth the pixels. Row `first` is the top of the visible window.
    let first = ROWS - rows;
    let cw = cell_w(fw);
    let left = PANEL_PAD;

    out.push(Node::RoundRect {
        rect: put(
            fw,
            fh,
            Rect::new(left, top + TOP_PAD, cw * COLS as f32, line * rows as f32),
        ),
        radii: CornerRadii::uniform(6.0),
        fill: GRID_FILL,
    });

    for (i, r) in (first..ROWS).enumerate() {
        let y = top + TOP_PAD + line * i as f32;
        // One text node per run of cells sharing a foreground colour: a node
        // per cell would be 80 draw calls a row for no visual gain, and the
        // whole row as one node would lose colour entirely.
        let mut run = String::new();
        let mut run_fg = None;
        let mut run_col = 0usize;
        let cells = screen.line_cells(r);
        for (c, cell) in cells.iter().enumerate() {
            let fg = Some(cell.fg);
            if run_fg.is_some() && fg != run_fg {
                push_run(&mut out, fw, fh, left, y, cw, run_col, &run, run_fg.unwrap(), cell.bold);
                run.clear();
                run_col = c;
            }
            run_fg = fg;
            run.push(cell.ch);
        }
        if !run.is_empty() {
            push_run(&mut out, fw, fh, left, y, cw, run_col, &run, run_fg.unwrap(), false);
        }
    }
    out
}

/// One same-colour run of cells as a single text node.
#[allow(clippy::too_many_arguments)]
fn push_run(
    out: &mut Vec<Node>,
    fw: f32,
    fh: f32,
    left: f32,
    y: f32,
    cw: f32,
    col: usize,
    text: &str,
    fg: u8,
    bold: bool,
) {
    // Trailing blanks are the common case (every short line), and a run of
    // spaces is invisible: skip it rather than paying a draw call.
    if text.trim_end().is_empty() {
        return;
    }
    let (r, g, b) = termscreen::palette(fg);
    out.push(Node::Text {
        rect: put(
            fw,
            fh,
            Rect::new(left + cw * col as f32, y, cw * text.chars().count() as f32, cw / CELL_EM * LINE_EM),
        ),
        text: text.to_string(),
        style: TextStyle {
            size: cw / CELL_EM,
            weight: if bold {
                crate::core::scene::Weight::Bold
            } else {
                crate::core::scene::Weight::Regular
            },
            color: Rgba::rgba(r, g, b, 1.0),
            tabular: false,
            align: crate::core::scene::Align::Start,
            mono: true,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::layout::PANEL_W_REF;

    #[test]
    fn all_eighty_columns_fit_the_panel() {
        // The whole point of deriving the size: no clipping on the right edge.
        let fw = PANEL_W_REF;
        let right = PANEL_PAD + cell_w(fw) * COLS as f32;
        assert!(
            right <= fw - PANEL_PAD + 0.01,
            "grid runs to {right}, panel inner edge is {}",
            fw - PANEL_PAD
        );
    }

    #[test]
    fn a_blank_grid_draws_only_the_track() {
        let s = Screen::new();
        let nodes = content(PANEL_W_REF, 200.0, 60.0, Some(&s), None);
        assert_eq!(nodes.len(), 1, "blank grid should emit just its track");
        assert!(matches!(nodes[0], Node::RoundRect { .. }));
    }

    #[test]
    fn output_becomes_text_nodes_above_the_footer() {
        let mut s = Screen::new();
        // Enough output to scroll past the top of the grid: only the
        // newest lines are visible (bottom-aligned), so a single line
        // on an otherwise empty screen is above the viewport.
        for _ in 0..ROWS {
            s.feed(b"hello arc\r\n");
        }
        let nodes = content(PANEL_W_REF, 200.0, 60.0, Some(&s), None);
        let text: Vec<&Node> = nodes
            .iter()
            .filter(|n| matches!(n, Node::Text { .. }))
            .collect();
        assert!(!text.is_empty(), "fed output must produce a text node");
        for n in &text {
            assert!(n.rect().max_y() <= 200.0 - crate::ui::FOOTER_BOTTOM_PAD + 0.01);
            assert!(n.rect().max_x() <= PANEL_W_REF + 0.01);
        }
    }

    #[test]
    fn a_failed_shell_explains_itself_instead_of_drawing_a_grid() {
        let nodes = content(PANEL_W_REF, 200.0, 60.0, None, Some("boom"));
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::Text { text, style, .. } => {
                assert!(text.contains("boom"));
                assert!(style.color.r > 0.9, "failure text should be red");
            }
            other => panic!("expected the error row, got {other:?}"),
        }
    }
}
