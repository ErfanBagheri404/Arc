//! Immediate-mode scene graph: the draw list `ui` produces and `platform` replays.
//!
//! Nodes are plain data. `ui` builds them from state; `platform` walks them with D2D.
//! Nothing here touches Win32, which is what keeps the look testable without a GPU.

use super::geom::{CornerRadii, Rect, Rgba};

/// Draw-order index; higher paints later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Z(pub i32);

/// Text weight, mapped to DirectWrite font weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weight {
    #[default]
    Regular,
    Semibold,
    Bold,
}

/// Horizontal text alignment inside a node's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    /// Apply horizontal alignment to `available` width inside a container of `total`.
    pub fn offset(self, used: f32, total: f32) -> f32 {
        match self {
            Align::Start => 0.0,
            Align::Center => ((total - used) / 2.0).max(0.0),
            Align::End => (total - used).max(0.0),
        }
    }
}

/// Text styling. Kept small on purpose — a style table beats a font engine here.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// Logical pixel size.
    pub size: f32,
    pub weight: Weight,
    pub color: Rgba,
    /// Use tabular figures (essential for timers and progress so digits don't jitter).
    pub tabular: bool,
    pub align: Align,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 14.0,
            weight: Weight::Regular,
            color: Rgba::WHITE,
            tabular: false,
            align: Align::Start,
        }
    }
}

impl TextStyle {
    /// Title style: 20px semibold white, per the reference design's media header.
    pub fn title() -> Self {
        Self {
            size: 20.0,
            weight: Weight::Semibold,
            ..Default::default()
        }
    }

    /// Subtitle style: 16px regular, tinted with the album-art accent.
    pub fn subtitle(accent: Rgba) -> Self {
        Self {
            size: 16.0,
            weight: Weight::Regular,
            color: accent,
            ..Default::default()
        }
    }

    /// Numeric style: tabular, used for every clock/duration so width stays stable.
    pub fn numeric(size: f32) -> Self {
        Self {
            size,
            weight: Weight::Semibold,
            tabular: true,
            ..Default::default()
        }
    }
}

/// One drawable element.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// Filled squircle with optional asymmetric radii.
    RoundRect {
        rect: Rect,
        radii: CornerRadii,
        fill: Rgba,
    },
    /// A text run. `text` is measured by the text engine; `rect` is the draw box.
    Text {
        rect: Rect,
        text: String,
        style: TextStyle,
    },
    /// Decoded image (album art, thumbnails) stretched into `rect`.
    Image {
        rect: Rect,
        /// Handle produced by the platform layer's image cache.
        handle: ImageHandle,
        /// Corner radii applied by clipping the image geometry.
        radii: CornerRadii,
    },
    /// Horizontal progress bar with a filled portion.
    Bar {
        rect: Rect,
        /// 0.0..=1.0
        progress: f32,
        track: Rgba,
        fill: Rgba,
        radii: CornerRadii,
    },
    /// Inline vector glyph, identified by name so art stays data-driven.
    Glyph {
        rect: Rect,
        name: &'static str,
        color: Rgba,
    },
    /// Children painted in order inside `rect` (the caller does layout).
    Group { rect: Rect, children: Vec<Node> },
}

impl Node {
    pub fn rect(&self) -> Rect {
        match self {
            Node::RoundRect { rect, .. }
            | Node::Text { rect, .. }
            | Node::Image { rect, .. }
            | Node::Bar { rect, .. }
            | Node::Glyph { rect, .. }
            | Node::Group { rect, .. } => *rect,
        }
    }

    /// Translate the node (and, for groups, its subtree) by `(dx, dy)`.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        fn walk(n: &mut Node, dx: f32, dy: f32) {
            let r = n.rect();
            n.translate_self(dx, dy);
            let _ = r;
            if let Node::Group { children, .. } = n {
                for c in children {
                    walk(c, dx, dy);
                }
            }
        }
        walk(self, dx, dy);
    }

    fn translate_self(&mut self, dx: f32, dy: f32) {
        let (x, y) = match self {
            Node::RoundRect { rect, .. }
            | Node::Text { rect, .. }
            | Node::Image { rect, .. }
            | Node::Bar { rect, .. }
            | Node::Glyph { rect, .. }
            | Node::Group { rect, .. } => (rect.x, rect.y),
        };
        let new = Rect::new(x + dx, y + dy, self.rect().w, self.rect().h);
        match self {
            Node::RoundRect { rect, .. }
            | Node::Text { rect, .. }
            | Node::Image { rect, .. }
            | Node::Bar { rect, .. }
            | Node::Glyph { rect, .. }
            | Node::Group { rect, .. } => *rect = new,
        }
    }

    /// Depth-first count, used by the perf probe and tests.
    pub fn count(&self) -> usize {
        1 + match self {
            Node::Group { children, .. } => children.iter().map(Node::count).sum::<usize>(),
            _ => 0,
        }
    }

    /// Walk every node depth-first (pre-order).
    pub fn walk(&self, f: &mut impl FnMut(&Node)) {
        f(self);
        if let Node::Group { children, .. } = self {
            for c in children {
                c.walk(f);
            }
        }
    }
}

/// Opaque handle to a platform-decoded image. `0` means "nothing decoded yet";
/// the platform layer substitutes a placeholder rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ImageHandle(pub u64);

/// A scene: an ordered list of top-level nodes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    pub nodes: Vec<Node>,
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, node: Node) -> &mut Self {
        self.nodes.push(node);
        self
    }

    pub fn count(&self) -> usize {
        self.nodes.iter().map(Node::count).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Walk every node in the whole scene, depth-first.
    pub fn walk(&self, f: &mut impl FnMut(&Node)) {
        for n in &self.nodes {
            n.walk(f);
        }
    }
}

/// The whole island surface for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    /// Window-space size the scene was laid out for.
    pub size: (f32, f32),
    pub scene: Scene,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(x: f32, y: f32, w: f32, h: f32) -> Node {
        Node::RoundRect {
            rect: Rect::new(x, y, w, h),
            radii: CornerRadii::uniform(8.0),
            fill: Rgba::BLACK,
        }
    }

    #[test]
    fn align_offsets() {
        assert_eq!(Align::Start.offset(10.0, 100.0), 0.0);
        assert_eq!(Align::Center.offset(10.0, 100.0), 45.0);
        assert_eq!(Align::End.offset(10.0, 100.0), 90.0);
        // Overflow must not push content left of the container.
        assert_eq!(Align::Center.offset(120.0, 100.0), 0.0);
        assert_eq!(Align::End.offset(120.0, 100.0), 0.0);
    }

    #[test]
    fn count_is_depth_first() {
        let scene = Scene::new()
            .push(rr(0.0, 0.0, 10.0, 10.0))
            .push(rr(0.0, 0.0, 10.0, 10.0))
            .clone();
        assert_eq!(scene.count(), 2);

        let nested = Node::Group {
            rect: Rect::new(0.0, 0.0, 50.0, 50.0),
            children: vec![
                rr(0.0, 0.0, 10.0, 10.0),
                Node::Group {
                    rect: Rect::new(0.0, 0.0, 5.0, 5.0),
                    children: vec![rr(0.0, 0.0, 1.0, 1.0)],
                },
            ],
        };
        assert_eq!(nested.count(), 4);
    }

    #[test]
    fn translate_moves_whole_subtree() {
        let mut n = Node::Group {
            rect: Rect::new(0.0, 0.0, 50.0, 50.0),
            children: vec![
                rr(1.0, 2.0, 10.0, 10.0),
                Node::Group {
                    rect: Rect::new(3.0, 4.0, 8.0, 8.0),
                    children: vec![rr(5.0, 6.0, 2.0, 2.0)],
                },
            ],
        };
        n.translate(10.0, 20.0);
        let mut seen = Vec::new();
        n.walk(&mut |node| seen.push(node.rect()));
        assert_eq!(seen[0], Rect::new(10.0, 20.0, 50.0, 50.0));
        assert_eq!(seen[1], Rect::new(11.0, 22.0, 10.0, 10.0));
        assert_eq!(seen[2], Rect::new(13.0, 24.0, 8.0, 8.0));
        assert_eq!(seen[3], Rect::new(15.0, 26.0, 2.0, 2.0));
    }

    #[test]
    fn bar_progress_is_clamped_by_consumer_not_lost() {
        let b = Node::Bar {
            rect: Rect::new(0.0, 0.0, 100.0, 4.0),
            progress: 1.7,
            track: Rgba::WHITE,
            fill: Rgba::BLACK,
            radii: CornerRadii::uniform(2.0),
        };
        match b {
            Node::Bar { progress, .. } => assert_eq!(progress, 1.7),
            _ => panic!("not a bar"),
        }
    }

    #[test]
    fn text_styles_match_reference_spec() {
        assert_eq!(TextStyle::title().size, 20.0);
        assert_eq!(TextStyle::title().weight, Weight::Semibold);
        assert_eq!(TextStyle::subtitle(Rgba::WHITE).size, 16.0);
        assert!(TextStyle::numeric(13.0).tabular);
    }

    #[test]
    fn frame_carries_size() {
        let f = Frame {
            size: (640.0, 200.0),
            scene: Scene::new().push(rr(0.0, 0.0, 640.0, 200.0)).clone(),
        };
        assert_eq!(f.size, (640.0, 200.0));
        assert_eq!(f.scene.count(), 1);
    }

    #[test]
    fn empty_scene_is_empty() {
        let mut s = Scene::new();
        assert!(s.is_empty());
        s.push(rr(0.0, 0.0, 1.0, 1.0));
        assert!(!s.is_empty());
        s.clear();
        assert!(s.is_empty());
    }
}
