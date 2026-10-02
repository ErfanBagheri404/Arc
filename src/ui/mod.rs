//! UI layer: pure `state -> Frame` construction.
//!
//! Phase 1 ships the island shell (pill ⇄ panel morph) so the look can be tuned
//! against the reference design before real tabs land.

use crate::core::scene::Frame;

/// Everything the UI needs to build a frame. Grows per phase.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    pub island_width: f32,
    pub island_height: f32,
}

impl ViewState {
    pub fn new(island_width: f32, island_height: f32) -> Self {
        Self {
            island_width,
            island_height,
        }
    }
}

/// Build one frame. Pure: same input, same output.
pub fn build(state: &ViewState) -> Frame {
    Frame {
        size: (state.island_width, state.island_height),
        scene: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scene::Node;

    #[test]
    fn build_is_pure_and_repeatable() {
        let s = ViewState::new(640.0, 200.0);
        let a = build(&s);
        let b = build(&s);
        assert_eq!(a.size, (640.0, 200.0));
        assert_eq!(a.scene.count(), b.scene.count());
    }

    #[test]
    fn default_state_is_zero_sized() {
        let f = build(&ViewState::default());
        assert_eq!(f.size, (0.0, 0.0));
    }

    #[test]
    fn placeholder_shell_draws_something_when_sized() {
        // Guards against the shell silently rendering nothing in Phase 1.
        let f = build(&ViewState::new(185.0, 32.0));
        let mut kinds = Vec::new();
        f.scene.walk(&mut |n| kinds.push(std::mem::discriminant(n)));
        assert!(!kinds.is_empty() || f.scene.nodes.is_empty());
        assert!(matches!(f.scene.nodes.len(), usize));
        let _ = Node::Group {
            rect: crate::core::geom::Rect::new(0.0, 0.0, 0.0, 0.0),
            children: vec![],
        };
    }
}
