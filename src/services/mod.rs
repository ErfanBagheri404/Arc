//! Service layer: OS data providers that publish into `AppState`.
//!
//! Phase 1 ships only the skeleton; each provider lands in its own phase.

pub mod audio;
pub mod clipboard;
pub mod picker;
pub mod media;
pub mod metrics;
pub mod processes;
pub mod power;
