//! App layer: state machine + frame loop + event routing.
//!
//! Owns the run loop. The loop is *dirty-frame driven*: it renders only when state
//! changed or a spring is mid-flight, and parks otherwise. Idle cost must be zero
//! presents and ~0% CPU — that budget is a release gate (docs/05 §9).

mod state;

pub use state::{IslandState, WindowMode};

use std::time::{Duration, Instant};

use crate::core::anim::{Spring, SpringConfig};
use crate::platform::{ClickThrough, Event, Overlay, Renderer};
use crate::ui;

/// Nominal frame budget. Springs integrate against real elapsed time, so a slower
/// display changes smoothness, not the motion curve.
const NOMINAL_FRAME: Duration = Duration::from_micros(16_667);
/// When fully idle, yield this long between message-queue polls instead of spinning.
const IDLE_POLL: Duration = Duration::from_millis(16);

/// Entry point.
pub fn run() -> std::process::ExitCode {
    let Some(mut overlay) = Overlay::new() else {
        eprintln!("arc: could not create the overlay window");
        return std::process::ExitCode::FAILURE;
    };
    let Some(mut renderer) = Renderer::new(overlay.hwnd()) else {
        eprintln!("arc: could not create the render device chain");
        return std::process::ExitCode::FAILURE;
    };

    let mut state = IslandState::collapsed();
    overlay.show();

    let mut last = Instant::now();
    loop {
        let mut redraw = false;
        for event in overlay.pump_events() {
            match event {
                Event::ToggleIsland => state.toggle(),
                Event::FullscreenEnter => state.hide(),
                Event::FullscreenExit => state.toggle(),
                Event::Resized { .. } | Event::Redraw => redraw = true,
                Event::Quit => return std::process::ExitCode::SUCCESS,
                _ => redraw = true,
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last).min(Duration::from_millis(100));
        let animating = state.step_dt(dt.as_secs_f32());
        if animating || redraw {
            last = now;

            overlay.resize(state.logical_width(), state.logical_height());
            overlay.set_click_through(if state.expanded() {
                ClickThrough::No
            } else {
                ClickThrough::Yes
            });
            renderer.resize(
                overlay.dpi().snap(state.logical_width()),
                overlay.dpi().snap(state.logical_height()),
            );
            let frame = ui::build(&ui::ViewState::new(
                state.logical_width(),
                state.logical_height(),
            ));
            renderer.present(&frame, overlay.dpi().scale, dt.as_secs_f32());
        } else {
            // Park: no presents, no spring integration. Re-arm the clock so the
            // first animating frame measures a sane dt.
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
        }

        if animating {
            let elapsed = NOMINAL_FRAME.saturating_sub(last.elapsed());
            if !elapsed.is_zero() {
                std::thread::sleep(elapsed);
            }
        }
    }
}

/// Springs for the island morph, re-exported so tests can assert parity with the
/// reference design's open/close parameters.
pub fn island_springs() -> (Spring, Spring) {
    (
        Spring::new(185.0, SpringConfig::OPEN),
        Spring::new(32.0, SpringConfig::OPEN),
    )
}
