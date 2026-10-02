//! Overlay window: HWND ownership, placement, DPI, click-through policy.
//!
//! Phase 1 stub. Replaced by the real Win32 implementation.

use super::dpi::Dpi;

/// Whether mouse input passes through to whatever is under the island.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickThrough {
    /// Collapsed island: clicks reach the desktop underneath.
    Yes,
    /// Expanded island: the panel takes input.
    No,
}

/// Owns the top-center overlay HWND.
pub struct Overlay {
    dpi: Dpi,
}

impl Overlay {
    /// Create the window. Returns `None` if the HWND could not be created.
    pub fn new() -> Option<Self> {
        Some(Self {
            dpi: Dpi::default(),
        })
    }

    pub fn dpi(&self) -> Dpi {
        self.dpi
    }

    /// Set the island's logical size and reposition it flush to the top edge.
    pub fn resize(&mut self, _logical_w: f32, _logical_h: f32) {}

    pub fn set_click_through(&mut self, _mode: ClickThrough) {}

    /// Drain queued window messages into platform events.
    pub fn pump_events(&mut self) -> Vec<super::Event> {
        Vec::new()
    }

    pub fn hwnd(&self) -> std::ptr::NonNull<std::ffi::c_void> {
        std::ptr::NonNull::dangling()
    }

    pub fn show(&self) {}

    pub fn hide(&self) {}
}
