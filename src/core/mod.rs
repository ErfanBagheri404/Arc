//! Arc — core primitives shared by every layer.
//!
//! This module is the contract boundary: `platform`, `app`, `services` and `ui`
//! all speak these types. Nothing here touches Win32, so it stays testable on any host.

pub mod anim;
pub mod geom;
pub mod scene;

pub use anim::{Spring, SpringConfig};
pub use geom::{Rect, Rgba};
