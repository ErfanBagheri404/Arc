//! Arc — a dynamic-island-style overlay for Windows.
//!
//! Layers (see `docs/05-ARCHITECTURE.md`): platform → core → app → services → ui.
//! Only `platform` may touch Win32; `ui` is a pure function of state.

mod app;
mod core;
mod platform;
mod services;
mod ui;

/// Minimal stderr logger, so a failed GPU chain is visible in a console or a
/// captured log instead of vanishing. `ponytail: no file/rolling sinks, no level
/// filter — swap in `env_logger` if Arc ever needs a log file.`
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("arc {}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: Stderr = Stderr;

fn main() -> std::process::ExitCode {
    // Fails only if something else already claimed the global logger.
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Warn));
    app::run()
}
