//! Settings: the tray's "Settings…" sheet — every user-facing knob in the app
//! in one scrolling list.
//!
//! The sheet is the single source of truth for what the user can change: the
//! notch's own background color, the idle pet plus its gallery (preview an
//! uninstalled pet, download it, equip it), and the opt-in services. Anything
//! added here is drawn and hit-tested from the same layout pass, so a click
//! can never land on a different row than the one that was drawn — the same
//! contract `ui::tab_boxes` keeps for the notch's tab strip.
//!
//! Pure builder, like every other ui module: [`sheet`] emits the nodes AND the
//! clickable rects, and the app loop turns a hit rect into an [`Action`] it
//! persists. The same builder powers the standalone Settings window.

use crate::core::geom::{CornerRadii, Rect, Rgba};
use crate::core::scene::{Frame, ImageHandle, Node, Scene, TextStyle};
use crate::services::pet::{GallerySnapshot, Tile, CELL_H, CELL_W};
use crate::ui::{layout, line_height, measure, PANEL_PAD};

/// Height of the window's caption block, so the rows start below the title.
/// Shared by [`window_frame`] and [`window_hit`] — one number, so the drawn row
/// and its click target cannot drift apart.
pub const TITLE_H: f32 = 40.0;

/// Height of one settings row.
pub const ROW_H: f32 = 34.0;
/// Height of one pet tile.
pub const TILE_H: f32 = 52.0;
/// Tiles per row in the gallery.
pub const TILE_COLS: usize = 2;
/// Gap above the first row.
pub const TOP_PAD: f32 = 8.0;
/// Gap between rows / tiles.
pub const ROW_GAP: f32 = 8.0;
/// Gap between sections.
pub const SECTION_GAP: f32 = 14.0;

/// Notch background colors offered as swatches. The notch draws white text and
/// a mostly-monochrome mascot, so every swatch is dark enough to keep that
/// legible; the picker is a set of tinted blacks rather than a full palette.
pub const NOTCH_SWATCHES: [Rgba; 8] = [
    Rgba::rgb(0.00, 0.00, 0.00),
    Rgba::rgb(0.09, 0.09, 0.10),
    Rgba::rgb(0.06, 0.07, 0.10),
    Rgba::rgb(0.04, 0.08, 0.07),
    Rgba::rgb(0.11, 0.07, 0.09),
    Rgba::rgb(0.07, 0.06, 0.11),
    Rgba::rgb(0.10, 0.08, 0.05),
    Rgba::rgb(0.14, 0.14, 0.16),
];

/// Human names for [`NOTCH_SWATCHES`], parallel array.
const NOTCH_SWATCH_NAMES: [&str; 8] = [
    "Black", "Graphite", "Midnight", "Forest", "Wine", "Indigo", "Sand", "Silver",
];

/// Smallest/largest pet scale. The ceiling is what still fits the collapsed
/// pill: the mascot is the pill-height cell, so asking for more would only
/// spill the sprite over the notch's rounded ends.
pub const SCALE_MIN: f64 = 0.6;
pub const SCALE_MAX: f64 = 1.2;
/// How much of the pet's size one click on the scale row changes.
pub const SCALE_STEP: f64 = 0.1;

/// Snap a raw scale into range, on whole steps. Stepping through the same
/// value the row displays keeps repeated clicks from drifting (0.1 is not
/// exactly representable, so a running sum would eventually read 99%).
#[must_use]
pub fn scale_snapped(raw: f64) -> f64 {
    ((raw / SCALE_STEP).round() * SCALE_STEP).clamp(SCALE_MIN, SCALE_MAX)
}

/// The grey track every row sits on.
const ROW_TRACK: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.05);
/// The iOS-style green an enabled toggle wears.
const ON: Rgba = Rgba::rgb(0.30, 0.75, 0.40);
/// The dim track a disabled toggle wears.
const OFF: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.14);
/// Section header text, dimmer than a row label.
const DIM: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.45);
/// The highlight a selected tile/swatch wears.
const SELECTED: Rgba = Rgba::rgba(1.0, 1.0, 1.0, 0.13);

/// What a click on the sheet asks the app to change.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    PetToggle,
    Equip(String),
    Install(String),
    Remove(String),
    NotchColor(usize),
    ClipToggle,
    PetScaleDown,
    PetScaleUp,
}

/// Everything the sheet renders, as the app hands it down each frame.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub pet_on: bool,
    pub pet_count: usize,
    /// Display name of the equipped pet, or `None` when no pet is loaded.
    /// Lets the sheet name the default pet without walking the gallery.
    pub equipped: Option<String>,
    pub notch_bg: Rgba,
    pub clip_on: bool,
    pub pet_scale: f64,
    pub gallery: Option<GallerySnapshot>,
    pub scroll: f32,
}

#[derive(Clone, Copy)]
struct Ctx {
    fw: f32,
    fh: f32,
    scroll: f32,
}

impl Ctx {
    fn put(&self, rect: Rect) -> Option<Rect> {
        let r = Rect::new(rect.x, rect.y - self.scroll, rect.w, rect.h);
        if r.max_x() <= 0.0 || r.max_y() <= 0.0 || r.x >= self.fw || r.y >= self.fh {
            return None;
        }
        Some(layout::fit(r, self.fw, self.fh))
    }
    fn hit(&self, rect: Rect) -> Rect {
        Rect::new(rect.x, rect.y - self.scroll, rect.w, rect.h)
    }
}

pub struct Sheet {
    pub nodes: Vec<Node>,
    pub hits: Vec<(Rect, Action)>,
    pub content_h: f32,
}

pub fn sheet(fw: f32, fh: f32, model: &Model) -> Sheet {
    let ctx = Ctx { fw, fh, scroll: model.scroll };
    let mut nodes = Vec::new();
    let mut hits = Vec::new();
    let mut y = TITLE_H + TOP_PAD;

    let section = |nodes: &mut Vec<Node>, y: &mut f32, name: &str| {
        if *y > TITLE_H {
            *y += SECTION_GAP - ROW_GAP;
        }
        let style = header_style();
        let h = line_height(&style);
        if let Some(rc) = ctx.put(Rect::new(PANEL_PAD, *y, fw - PANEL_PAD * 2.0, h)) {
            nodes.push(Node::Text { rect: rc, text: name.to_string(), style });
        }
        *y += h + 6.0;
    };

    section(&mut nodes, &mut y, "APPEARANCE");
    {
        let r = Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, ROW_H);
        y += ROW_H + ROW_GAP;
        let n = NOTCH_SWATCHES.len();
        let s = 20.0_f32;
        let gap = 6.0_f32;
        let sw_w = n as f32 * s + (n as f32 - 1.0) * gap;
        let sx0 = r.max_x() - 10.0 - sw_w;
        let label_right = sw_w + 10.0;
        nodes.extend(row(&ctx, r, "Notch background", label_right));
        for (i, color) in NOTCH_SWATCHES.iter().enumerate() {
            let b = Rect::new(sx0 + i as f32 * (s + gap), r.y + (r.h - s) / 2.0, s, s);
            let active = color.to_hex() == model.notch_bg.to_hex();
            if active {
                let ring = Rect::new(b.x - 3.0, b.y - 3.0, b.w + 6.0, b.h + 6.0);
                if let Some(rc) = ctx.put(ring) {
                    nodes.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(9.0), fill: SELECTED });
                }
            }
            if let Some(rc) = ctx.put(b) {
                nodes.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(6.0), fill: *color });
            }
            hits.push((ctx.hit(b), Action::NotchColor(i)));
        }
        let matched = NOTCH_SWATCHES.iter().position(|c| c.to_hex() == model.notch_bg.to_hex());
        // Name the swatch when one is picked, and fall back to the hex for a
        // hand-edited color that matches no swatch — the row should always say
        // what the notch is actually painted.
        let text = match matched {
            Some(i) => NOTCH_SWATCH_NAMES[i].to_string(),
            None => model.notch_bg.to_hex(),
        };
        let style = value_style();
        let th = line_height(&style);
        let w = measure(&text, &style);
        if let Some(rc) = ctx.put(Rect::new(sx0 - w - 8.0, r.y + (r.h - th) * 0.5, w, th)) {
            nodes.push(Node::Text { rect: rc, text, style });
        }
    }

    section(&mut nodes, &mut y, "PETS");
    {
        let r = Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, ROW_H);
        y += ROW_H + ROW_GAP;
        nodes.extend(row(&ctx, r, "Pet when idle", 36.0 + 10.0));
        nodes.push(toggle_if(&ctx, r, model.pet_on));
        hits.push((ctx.hit(r), Action::PetToggle));

        if model.pet_on && model.pet_count > 0 {
            let r = Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, ROW_H);
            y += ROW_H + ROW_GAP;
            nodes.extend(row(&ctx, r, "Equipped", 96.0));
            let style = value_style();
            let text = model.equipped.clone().unwrap_or_else(|| "None".to_string());
            draw_value(&ctx, &mut nodes, r, &text, &style);

            let r = Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, ROW_H);
            y += ROW_H + ROW_GAP;
            nodes.extend(row(&ctx, r, "Pet size", 96.0));
            let pct = format!("{:.0}%", model.pet_scale * 100.0);
            let style = value_style();
            let th = line_height(&style);
            let pw = measure(&pct, &style);
            if let Some(rc) = ctx.put(Rect::new(r.max_x() - pw - 10.0, r.y + (r.h - th) * 0.5, pw, th)) {
                nodes.push(Node::Text { rect: rc, text: pct, style });
            }
            let mid = r.max_x() - 36.0;
            let left = Rect::new(mid - 34.0, r.y, 34.0, ROW_H);
            let right = Rect::new(mid, r.y, 34.0, ROW_H);
            nodes.extend(stepper(&ctx, left, "‹"));
            nodes.extend(stepper(&ctx, right, "›"));
            hits.push((ctx.hit(left), Action::PetScaleDown));
            hits.push((ctx.hit(right), Action::PetScaleUp));
        }

        match &model.gallery {
            None => {
                let style = value_style();
                let h = line_height(&style);
                if let Some(rc) = ctx.put(Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, h)) {
                    nodes.push(Node::Text { rect: rc, text: "Loading gallery…".to_string(), style });
                }
                y += h + ROW_GAP;
            }
            Some(g) => {
                if g.tiles.is_empty() {
                    let style = value_style();
                    let h = line_height(&style);
                    let text = if g.loading { "Loading gallery…".to_string() } else { "Gallery unreachable — check your connection".to_string() };
                    if let Some(rc) = ctx.put(Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, h)) {
                        nodes.push(Node::Text { rect: rc, text, style });
                    }
                    y += h + ROW_GAP;
                } else {
                    let boxes = tile_boxes(fw);
                    let rows = g.tiles.len().div_ceil(TILE_COLS);
                    for (col, &(bx, bw)) in boxes.iter().enumerate() {
                        for i in 0..rows {
                            let idx = i * TILE_COLS + col;
                            if idx >= g.tiles.len() { continue; }
                            let row_y = y + i as f32 * (TILE_H + ROW_GAP);
                            let tile = &g.tiles[idx];
                            let tr = Rect::new(bx, row_y, bw, TILE_H);
                            let equipped = tile.slug == g.equipped;
                            let rm_x = tr.max_x() - 22.0;
                            let rm_w = 16.0_f32;
                            let body = Rect::new(tr.x, tr.y, (tr.w - (22.0 - 10.0)).max(0.0), tr.h);
                            nodes.extend(tile_node(&ctx, body, tile, equipped));
                            if tile.installed {
                                if let Some(rc) = ctx.put(Rect::new(rm_x, tr.y + 4.0, rm_w, rm_w)) {
                                    nodes.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(rm_w / 2.0), fill: Rgba::rgba(1.0, 1.0, 1.0, 0.12) });
                                }
                                let style = TextStyle { size: 11.0, color: DIM, ..Default::default() };
                                let th = line_height(&style);
                                if let Some(rc) = ctx.put(Rect::new(rm_x, tr.y + 4.0 + (rm_w - th) * 0.5, rm_w, th)) {
                                    nodes.push(Node::Text { rect: rc, text: "×".to_string(), style });
                                }
                                hits.push((ctx.hit(Rect::new(rm_x, tr.y + 4.0, rm_w, rm_w)), Action::Remove(tile.slug.clone())));
                            }
                            hits.push((
                                ctx.hit(body),
                                if tile.installed { Action::Equip(tile.slug.clone()) } else { Action::Install(tile.slug.clone()) },
                            ));
                        }
                    }
                    y += rows as f32 * TILE_H + (rows as f32 - 1.0).max(0.0) * ROW_GAP + ROW_GAP;
                }
            }
        }
    }

    section(&mut nodes, &mut y, "FEATURES");
    {
        let r = Rect::new(PANEL_PAD, y, fw - PANEL_PAD * 2.0, ROW_H);
        y += ROW_H + ROW_GAP;
        nodes.extend(row(&ctx, r, "Clipboard history", 36.0 + 10.0));
        nodes.push(toggle_if(&ctx, r, model.clip_on));
        hits.push((ctx.hit(r), Action::ClipToggle));
    }

    let content_h = y + 8.0;
    Sheet { nodes, hits, content_h }
}

pub fn window_frame(fw: f32, fh: f32, model: &Model) -> Frame {
    let mut scene = Scene::new();
    scene.push(Node::RoundRect {
        rect: Rect::new(0.0, 0.0, fw, fh),
        radii: CornerRadii::uniform(12.0).clamped(fw, fh),
        fill: Rgba::rgb(0.09, 0.09, 0.10),
    });
    let sh = sheet(fw, fh, model);
    for node in sh.nodes {
        scene.push(node);
    }
    Frame { size: (fw, fh), scene }
}

#[must_use]
pub fn window_hit(fw: f32, fh: f32, model: &Model, x: f32, y: f32) -> Option<Action> {
    sheet(fw, fh, model).hits.into_iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a)
}

fn header_style() -> TextStyle {
    TextStyle { size: 11.0, color: DIM, ..Default::default() }
}
fn label_style() -> TextStyle {
    TextStyle { size: 12.0, ..Default::default() }
}
fn value_style() -> TextStyle {
    TextStyle { size: 12.0, color: DIM, ..Default::default() }
}

fn row(ctx: &Ctx, r: Rect, label: &str, right: f32) -> Vec<Node> {
    let style = label_style();
    let h = line_height(&style);
    let mut out = Vec::new();
    if let Some(rc) = ctx.put(r) {
        out.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(9.0), fill: ROW_TRACK });
    }
    let box_w = (r.w - 20.0 - right).max(0.0);
    if let Some(rc) = ctx.put(Rect::new(r.x + 10.0, r.y + (r.h - h) * 0.5, box_w, h)) {
        out.push(Node::Text { rect: rc, text: label.to_string(), style });
    }
    out
}

fn toggle_if(ctx: &Ctx, r: Rect, on: bool) -> Node {
    let (w, h) = (36.0, 20.0);
    let track = Rect::new(r.max_x() - w - 10.0, r.y + (r.h - h) * 0.5, w, h);
    let knob = 16.0;
    let kx = if on { track.max_x() - knob - 2.0 } else { track.x + 2.0 };
    let out_track = ctx.put(track).unwrap_or(track);
    let out_knob = ctx.put(Rect::new(kx, track.y + 2.0, knob, knob)).unwrap_or(Rect::new(kx, track.y + 2.0, knob, knob));
    Node::Group {
        rect: out_track,
        children: vec![
            Node::RoundRect { rect: out_track, radii: CornerRadii::uniform(h / 2.0), fill: if on { ON } else { OFF } },
            Node::RoundRect { rect: out_knob, radii: CornerRadii::uniform(knob / 2.0), fill: Rgba::WHITE },
        ],
    }
}

fn stepper(ctx: &Ctx, r: Rect, glyph: &str) -> Vec<Node> {
    let style = TextStyle { size: 13.0, color: DIM, ..Default::default() };
    let h = line_height(&style);
    let mut out = Vec::new();
    if let Some(rc) = ctx.put(r) {
        out.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(9.0), fill: Rgba::rgba(1.0, 1.0, 1.0, 0.05) });
    }
    if let Some(rc) = ctx.put(Rect::new(r.x, r.y + (r.h - h) * 0.5, r.w, h)) {
        out.push(Node::Text { rect: rc, text: glyph.to_string(), style });
    }
    out
}

fn tile_node(ctx: &Ctx, r: Rect, tile: &Tile, equipped: bool) -> Vec<Node> {
    let mut out = Vec::new();
    if let Some(rc) = ctx.put(r) {
        out.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(9.0), fill: if equipped { SELECTED } else { ROW_TRACK } });
    }
    let th = (r.h - 8.0).min(44.0);
    let art = Rect::new(r.x + 4.0, r.y + (r.h - th) * 0.5, th * (CELL_W / CELL_H), th);
    if let Some(rc) = ctx.put(art) {
        out.push(Node::RoundRect { rect: rc, radii: CornerRadii::uniform(8.0), fill: Rgba::rgba(1.0, 1.0, 1.0, 0.06) });
    }
    if let Some(handle) = tile.thumb.filter(|h| *h != ImageHandle(0)) {
        let (sx, sy, sw, sh) = crate::services::pet::frame_rect(0);
        let inset = Rect::new(art.x + 2.0, art.y + 2.0, (art.w - 4.0).max(0.0), (art.h - 4.0).max(0.0));
        if let Some(rc) = ctx.put(inset) {
            out.push(Node::Sprite { rect: rc, handle, src: Rect::new(sx, sy, sw, sh), opacity: 1.0 });
        }
    }
    let label = label_style();
    let lh = line_height(&label);
    let sub = TextStyle { size: 10.5, color: DIM, ..Default::default() };
    let sh_h = line_height(&sub);
    let text_x = art.max_x() + 8.0;
    let text_w = (r.max_x() - text_x - 8.0).max(0.0);
    if let Some(rc) = ctx.put(Rect::new(text_x, r.y + 8.0, text_w, lh)) {
        out.push(Node::Text { rect: rc, text: tile.name.clone(), style: label });
    }
    let status = if tile.busy {
        "Downloading…".to_string()
    } else if equipped {
        "Equipped".to_string()
    } else if tile.installed {
        format!("{} · click to equip", tile.slug)
    } else {
        format!("{} · click to download", tile.slug)
    };
    if let Some(rc) = ctx.put(Rect::new(text_x, r.y + 8.0 + lh + 2.0, text_w, sh_h)) {
        out.push(Node::Text { rect: rc, text: status, style: sub });
    }
    out
}

/// Draw a right-aligned value label inside a settings row.
fn draw_value(ctx: &Ctx, out: &mut Vec<Node>, r: Rect, text: &str, style: &TextStyle) {
    let th = line_height(style);
    let w = measure(text, style);
    if let Some(rc) = ctx.put(Rect::new(r.max_x() - w - 10.0, r.y + (r.h - th) * 0.5, w, th)) {
        out.push(Node::Text { rect: rc, text: text.to_string(), style: style.clone() });
    }
}

fn tile_boxes(fw: f32) -> Vec<(f32, f32)> {
    let avail = fw - PANEL_PAD * 2.0;
    let w = (avail - (TILE_COLS as f32 - 1.0) * ROW_GAP) / TILE_COLS as f32;
    (0..TILE_COLS).map(|i| (PANEL_PAD + i as f32 * (w + ROW_GAP), w)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model {
            pet_on: true,
            pet_count: 2,
            equipped: Some("Nukey".to_string()),
            notch_bg: NOTCH_SWATCHES[1],
            clip_on: false,
            pet_scale: 1.0,
            gallery: None,
            scroll: 0.0,
        }
    }

    #[test]
    fn notch_color_hits_cover_all_swatches() {
        let m = model();
        let s = sheet(460.0, 560.0, &m);
        let n = s.hits.iter().filter(|(_, a)| matches!(a, Action::NotchColor(_))).count();
        assert_eq!(n, NOTCH_SWATCHES.len());
    }

    #[test]
    fn scrolling_translates_hits() {
        let mut m = model();
        let zero = sheet(460.0, 560.0, &m);
        m.scroll = 30.0;
        let scrolled = sheet(460.0, 560.0, &m);
        assert_eq!(zero.content_h, scrolled.content_h);
        let a = zero.hits[0].clone();
        let b = scrolled.hits[0].clone();
        assert_eq!(a.1, b.1);
        assert!((a.0.y - b.0.y - 30.0).abs() < 0.01);
    }

    #[test]
    fn an_uninstalled_pet_is_downloadable() {
        let mut m = model();
        m.gallery = Some(GallerySnapshot {
            loading: false,
            tiles: vec![
                Tile { slug: "nukey".into(), name: "Nukey".into(), installed: true, ..Default::default() },
                Tile { slug: "moth".into(), name: "Moth".into(), installed: false, ..Default::default() },
            ],
            equipped: "nukey".into(),
            version: 1,
        });
        let s = sheet(460.0, 560.0, &m);
        assert!(s.hits.iter().any(|(_, a)| *a == Action::Install("moth".into())));
        assert!(s.hits.iter().any(|(_, a)| *a == Action::Equip("nukey".into())));
        assert!(s.hits.iter().any(|(_, a)| *a == Action::Remove("nukey".into())));
    }

    #[test]
    fn custom_color_row_shows_hex() {
        // A color that is not one of the swatches — a hand-edited file — has to
        // report itself, or the row would look like no color was ever chosen.
        let mut m = model();
        m.notch_bg = Rgba::rgb(0.5, 0.25, 0.75);
        let s = sheet(460.0, 560.0, &m);
        assert!(s
            .nodes
            .iter()
            .any(|n| matches!(n, Node::Text { text, .. } if text == &"#8040bf")));
    }

    #[test]
    fn scale_steps_snap_inside_range() {
        assert_eq!(scale_snapped(0.0), SCALE_MIN);
        assert_eq!(scale_snapped(9.0), SCALE_MAX);
        assert!((scale_snapped(1.03) - 1.0).abs() < 1e-9);
        // A running sum of `SCALE_STEP` must land exactly on the printed value.
        let mut v = 1.0;
        for _ in 0..20 {
            v = scale_snapped(v + SCALE_STEP);
        }
        assert_eq!(v, SCALE_MAX);
    }

    #[test]
    fn pet_off_hides_scale_row() {
        let mut m = model();
        m.pet_on = false;
        let s = sheet(460.0, 560.0, &m);
        assert!(!s.hits.iter().any(|(_, a)| matches!(a, Action::PetScaleUp)));
    }

    #[test]
    fn equipped_row_names_the_loaded_pet() {
        // The gallery tiles only mark which pet is equipped; the row above them
        // has to say the name outright, or a pet that is not in the gallery
        // (a hand-installed one) is never named anywhere.
        let m = model();
        let s = sheet(460.0, 560.0, &m);
        assert!(s.nodes.iter().any(|n| matches!(n, Node::Text { text, .. } if text == "Nukey")));
    }

    #[test]
    fn equipped_row_is_gone_with_no_pets() {
        let mut m = model();
        m.pet_count = 0;
        let s = sheet(460.0, 560.0, &m);
        assert!(!s.nodes.iter().any(|n| matches!(n, Node::Text { text, .. } if text == "Equipped")));
    }

    #[test]
    fn swatch_clicks_do_not_overlap() {
        let m = model();
        let s = sheet(460.0, 560.0, &m);
        let sw: Vec<Rect> = s.hits.iter().filter(|(_, a)| matches!(a, Action::NotchColor(_))).map(|(r, _)| *r).collect();
        assert_eq!(sw.len(), NOTCH_SWATCHES.len());
        for w in sw.windows(2) {
            assert!(w[0].max_x() <= w[1].x + 0.01, "{:?} overlaps {:?}", w[0], w[1]);
        }
    }
}