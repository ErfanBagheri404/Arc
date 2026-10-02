//! App layer: state machine + frame loop + event routing.
//!
//! Phase 1 stub: opens the overlay, runs the dirty-frame loop, morphs the island
//! pill ⇄ panel with the reference design's springs.

mod state;

pub use state::{IslandState, WindowMode};

use crate::core::anim::Spring;
use crate::core::anim::SpringConfig;
use crate::platform::{ClickThrough, Event, Overlay, Renderer};
use crate::ui;

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

    loop {
        for event in overlay.pump_events() {
            match event {
                Event::ToggleIsland => state.toggle(),
                Event::Resized { .. } => {}
                Event::Quit => return std::process::ExitCode::SUCCESS,
                _ => {}
            }
        }

        if !state.step() {
            // Nothing animating: park until something wakes us.
            continue;
        }

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
        renderer.present(&frame, overlay.dpi().scale);
    }
}

/// Springs for the island morph, re-exported so tests can assert parity.
pub fn island_springs() -> (Spring, Spring) {
    (
        Spring::new(150.0, SpringConfig::OPEN),
        Spring::new(32.0, SpringConfig::OPEN),
    )
}
