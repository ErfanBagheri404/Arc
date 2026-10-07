//! Arc — core primitives shared by every layer.
//!
//! This module is the contract boundary: `platform`, `app`, `services` and `ui`
//! all speak these types. Nothing here touches Win32, so it stays testable on any host.

// The core contracts are the crate's shared vocabulary: they are written once,
// ahead of the features that consume them, so a phase's worth of API is
// legitimately unreferenced at any given commit. Dead-code warnings are
// suppressed per module rather than by deleting contract surface (which would
// churn every `use` site in a later phase) — see docs/05-ARCHITECTURE.md.
#[allow(dead_code)]
pub mod anim;
pub mod color;
pub mod imagedb;
#[allow(dead_code)]
pub mod geom;
#[allow(dead_code)]
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
