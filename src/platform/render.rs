//! Renderer: D3D11 flip-model swapchain + DirectComposition + D2D1 + DirectWrite.
//!
//! Phase 1 stub. Replaced by the real implementation.
//!
//! Contract (do not change without updating `app::run`):
//! - `new(hwnd)` once, after the window exists.
//! - `resize(physical_w, physical_h)` whenever the island geometry changes.
//! - `present(frame, dpi_scale, dt_seconds)` exactly once per frame the app loop
//!   asks for; the renderer must not present on its own.

use crate::core::scene::Frame;

/// Runtime counters for the debug overlay and the release perf gate
/// (idle presents must be 0; see docs/05-ARCHITECTURE.md §9).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PerfProbe {
    /// Frames handed to `present` since start.
    pub presents: u64,
    /// Nodes in the last presented frame.
    pub last_node_count: usize,
    /// Rolling average frame time in milliseconds, or `None` when nothing animated.
    pub avg_frame_ms: Option<f32>,
}

/// Replays a scene onto the overlay's swapchain.
pub struct Renderer {
    probe: PerfProbe,
}

impl Renderer {
    /// Build the device chain against `hwnd`.
    pub fn new(_hwnd: std::ptr::NonNull<std::ffi::c_void>) -> Option<Self> {
        Some(Self {
            probe: PerfProbe::default(),
        })
    }

    /// Recreate the swapchain for a new size.
    pub fn resize(&mut self, _width: u32, _height: u32) {}

    /// Draw one frame and present it.
    pub fn present(&mut self, frame: &Frame, _scale: f32, dt: f32) {
        self.probe.presents += 1;
        self.probe.last_node_count = frame.scene.count();
        let ms = dt * 1000.0;
        self.probe.avg_frame_ms = Some(match self.probe.avg_frame_ms {
            Some(prev) => prev * 0.9 + ms * 0.1,
            None => ms,
        });
    }

    /// Counters for the debug overlay.
    pub fn probe(&self) -> PerfProbe {
        self.probe
    }

    /// Logical-pixel width of a text run, for layout.
    pub fn measure_text(&mut self, text: &str, style: &crate::core::scene::TextStyle) -> f32 {
        text.len() as f32 * style.size * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geom::{CornerRadii, Rect, Rgba};
    use crate::core::scene::{Node, Scene};

    fn frame() -> Frame {
        Frame {
            size: (185.0, 32.0),
            scene: Scene::new()
                .push(Node::RoundRect {
                    rect: Rect::new(0.0, 0.0, 185.0, 32.0),
                    radii: CornerRadii::uniform(16.0),
                    fill: Rgba::BLACK,
                })
                .clone(),
        }
    }

    #[test]
    fn probe_starts_at_zero_presents() {
        let r = Renderer::new(std::ptr::NonNull::dangling()).expect("renderer");
        assert_eq!(r.probe().presents, 0);
        assert_eq!(r.probe().avg_frame_ms, None);
    }

    #[test]
    fn present_counts_frames_and_nodes() {
        let mut r = Renderer::new(std::ptr::NonNull::dangling()).expect("renderer");
        r.present(&frame(), 1.0, 1.0 / 60.0);
        r.present(&frame(), 1.0, 1.0 / 60.0);
        assert_eq!(r.probe().presents, 2);
        assert_eq!(r.probe().last_node_count, 1);
        assert!(r.probe().avg_frame_ms.is_some());
    }

    #[test]
    fn resize_is_safe_to_repeat() {
        let mut r = Renderer::new(std::ptr::NonNull::dangling()).expect("renderer");
        r.resize(185, 32);
        r.resize(185, 32);
        assert_eq!(r.probe().presents, 0);
    }
}
