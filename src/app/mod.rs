//! App layer: state machine + frame loop + event routing.
//!
//! Owns the run loop. The loop is *dirty-frame driven*: it renders only when state
//! changed or a spring is mid-flight, and parks otherwise. Idle cost must be zero
//! presents and ~0% CPU — that budget is a release gate (docs/05 §9).

mod state;

pub use state::IslandState;

use std::time::{Duration, Instant};

use crate::platform::{ClickThrough, Event, Overlay, Renderer};
use crate::ui;

/// Nominal frame budget. Springs integrate against real elapsed time, so a slower
/// display changes smoothness, not the motion curve.
const NOMINAL_FRAME: Duration = Duration::from_micros(16_667);
/// When fully idle, yield this long between message-queue polls instead of spinning.
/// Also the hover-polling cadence: a collapsed island is click-through, so the
/// pointer has to be sampled rather than awaited. 25 ms is under one frame at
/// 40 Hz and still leaves the process ~0% CPU.
const IDLE_POLL: Duration = Duration::from_millis(25);

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

    if !overlay.hotkey_ok() {
        // Never fatal: the island still works, the user just cannot toggle it
        // from a hotkey (another app owns Ctrl+Shift+A, or a locked-down session
        // refused registration).
        log::warn!("global hotkey Ctrl+Shift+A was not registered; toggle unavailable");
    }

    let mut state = IslandState::collapsed();
    // The tab selection and hover persist across frames; everything else the UI
    // needs is derived from the size springs.
    let mut view = ui::ViewState::default();
    overlay.show();

    // First paint before the loop: with no events yet, the dirty-frame loop would
    // park on frame zero and show an undefined (never-presented) window until the
    // first hotkey or click.
    overlay.resize(state.logical_width(), state.logical_height());
    renderer.resize(
        overlay.dpi().snap(state.logical_width()),
        overlay.dpi().snap(state.logical_height()),
    );
    view.island_width = state.logical_width();
    view.island_height = state.logical_height();
    let frame = ui::build(&view);
    renderer.present(&frame, overlay.dpi().scale, 0.0);

    let mut last = Instant::now();
    loop {
        let mut redraw = false;
        let scale = overlay.dpi().scale;
        // The persistent view must carry the current geometry: this iteration's
        // clicks are hit-tested against it.
        view.island_width = state.logical_width();
        view.island_height = state.logical_height();
        for event in overlay.pump_events() {
            match event {
                Event::ToggleIsland => state.toggle(),
                Event::FullscreenEnter => state.hide(),
                // Back to whatever we were showing before the fullscreen app —
                // not unconditionally the panel.
                Event::FullscreenExit => state.restore(),
                // Clicks arrive in physical client pixels; the UI lays out in
                // logical ones, so scale before hit-testing.
                Event::LeftClick { x, y } => {
                    let hit = view.click_tab(x / scale, y / scale);
                    if hit.is_some() {
                        redraw = true;
                    }
                }
                Event::CursorMoved { x, y } => {
                    let hit = view.tab_at(x / scale, y / scale);
                    if hit != view.hover_tab {
                        view.hover_tab = hit;
                        redraw = true;
                    }
                }
                Event::CursorLeft => {
                    if view.hover_tab.take().is_some() {
                        redraw = true;
                    }
                }
                Event::Resized { .. } | Event::Redraw => redraw = true,
                Event::Quit => return std::process::ExitCode::SUCCESS,
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last).min(Duration::from_millis(100));
        // Hover is polled, not event-driven (the collapsed window is
        // click-through), so it has to be stepped even on frames where nothing
        // else woke us up.
        let hover_changed = state.hover_step(overlay.pointer_over(), dt.as_secs_f32());
        let animating = state.step_dt(dt.as_secs_f32());

        // A hidden island is not just un-drawn: it is removed from the screen so
        // it cannot sit over the fullscreen app it is hiding for.
        if state.hidden() {
            overlay.hide();
        } else {
            overlay.show();
        }

        if animating || hover_changed || redraw {
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
            view.island_width = state.logical_width();
            view.island_height = state.logical_height();
            let frame = ui::build(&view);
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
