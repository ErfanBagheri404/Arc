//! Arc — a dynamic-island-style notch overlay for Windows.
//!
//! Layers (see `docs/05-ARCHITECTURE.md`): platform → core → app → services → ui.
//! Only `platform` may touch Win32; `ui` is a pure function of state.

mod app;
mod core;
mod platform;
mod services;
mod ui;

fn main() -> std::process::ExitCode {
    app::run()
}
