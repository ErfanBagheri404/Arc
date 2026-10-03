//! Arc — core primitives shared by every layer.
//!
//! This module is the contract boundary: `platform`, `app`, `services` and `ui`
//! all speak these types. Nothing here touches Win32, so it stays testable on any host.

pub mod anim;
pub mod geom;
pub mod scene;

// Re-export barrel: this is the crate's internal public surface, used by layers
// below. Individual items are not all referenced yet (the island shell landed before
// some consumers), so the unused-import lint is suppressed deliberately rather than
// by deleting the barrel and churning every `use` site.
// SAFETY: none — compile-time only.
#[allow(unused_imports)]
pub use anim::{Spring, SpringConfig};
#[allow(unused_imports)]
pub use geom::{Rect, Rgba};
