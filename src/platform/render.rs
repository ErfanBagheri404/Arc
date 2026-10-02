//! Renderer: D3D11 flip-model swapchain + DirectComposition + D2D1 + DirectWrite.
//!
//! Phase 1 stub. Replaced by the real implementation.

use crate::core::scene::Frame;

/// Replays a scene onto the overlay's swapchain.
pub struct Renderer;

impl Renderer {
    /// Build the device chain against `hwnd`.
    pub fn new(_hwnd: std::ptr::NonNull<std::ffi::c_void>) -> Option<Self> {
        Some(Self)
    }

    /// Recreate the swapchain for a new size.
    pub fn resize(&mut self, _width: u32, _height: u32) {}

    /// Draw one frame and present it.
    pub fn present(&mut self, _frame: &Frame, _scale: f32) {}

    /// Logical-pixel width of a text run, for layout.
    pub fn measure_text(&mut self, _text: &str, _style: &crate::core::scene::TextStyle) -> f32 {
        _text.len() as f32 * _style.size * 0.5
    }
}
