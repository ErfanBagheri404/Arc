//! Service layer: OS data providers that publish into `AppState`.
//!
//! Phase 1 ships only the skeleton; each provider lands in its own phase.

pub mod audio;
pub mod calendar;
pub mod calendars;
pub mod clipboard;
pub mod downloads;
pub mod localsend;
pub mod media;
pub mod metrics;
pub mod net;
pub mod pet;
pub mod picker;
pub mod power;
pub mod processes;
pub mod settings;
pub mod shelf;
pub mod status;
pub mod terminal;
pub mod usage;
pub mod weather;
