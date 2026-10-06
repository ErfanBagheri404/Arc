//! Platform layer: the only place allowed to touch Win32 / D3D / DirectWrite.
//!
//! Contract consumed by `app`:
//! - [`window::Overlay`] owns the HWND, placement, DPI, and click-through policy.
//! - [`render::Renderer`] owns the D3D/DirectComposition/D2D device chain and
//!   replays a [`crate::core::scene::Frame`] onto it.
//!
//! Both are created against the same HWND: window first, then renderer.

pub mod dpi;
pub mod render;
pub mod tray;
pub mod window;

// Re-exports consumed by `app`; see the note on the core barrel about
// `allow(unused_imports)`.
#[allow(unused_imports)]
pub use dpi::Dpi;
pub use render::Renderer;
pub use window::{ClickThrough, Overlay};

/// Input/notification events the window layer hands to `app` each pump.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Hotkey toggled the island (Ctrl+Shift+A by default).
    ToggleIsland,
    /// Cursor moved over the window (window-space px, plus time-since-enter).
    CursorMoved {
        x: f32,
        y: f32,
    },
    /// Cursor left the window.
    CursorLeft,
    LeftClick {
        x: f32,
        y: f32,
    },
    /// Display/DPI/window-placement change; renderer must resize.
    Resized {
        width: u32,
        height: u32,
    },
    /// Foreground window became fullscreen on our monitor → hide.
    FullscreenEnter,
    FullscreenExit,
    /// Ask for a repaint (state changed, or a service published data).
    Redraw,
    Quit,
}
