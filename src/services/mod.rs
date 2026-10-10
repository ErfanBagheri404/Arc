//! Service layer: OS data providers that publish into `AppState`.
//!
//! Phase 1 ships only the skeleton; each provider lands in its own phase.

pub mod audio;
pub mod clipboard;
pub mod picker;
pub mod settings;
pub mod weather;
pub mod calendar;
pub mod downloads;
pub mod localsend;
pub mod shelf;
pub mod terminal;
pub mod calendars;
pub mod media;
pub mod metrics;
pub mod processes;
pub mod power;
