//! App layer: state machine + frame loop + event routing.
//!
//! Owns the run loop. The loop is *dirty-frame driven*: it renders only when state
//! changed or a spring is mid-flight, and parks otherwise. Idle cost must be zero
//! presents and ~0% CPU — that budget is a release gate (docs/05 §9).

pub mod hud;
mod state;
pub mod timer;

pub use state::IslandState;

use std::time::{Duration, Instant};

use crate::platform::{ClickThrough, Event, Overlay, Renderer};
use crate::services::media::Media;
use crate::services::audio::{self, Audio};
use crate::services::clipboard::Clipboard;
use crate::services::picker::Picker;
use crate::services::weather::Weather;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use crate::services::processes::Processes;
use crate::services::metrics::Sampler;
use crate::services::power;
use crate::ui;
use hud::Hud;
use timer::{Timer, PRESETS};

/// The wall clock for a countdown, in the app thread.
struct Countdown {
    /// Snapshot handed to the UI each frame.
    view: Timer,
    /// When the current second last rolled over.
    last: Instant,
    /// True between a preset click and the run reaching zero.
    running: bool,
}

impl Countdown {
    fn new(now: Instant) -> Self {
        Self {
            view: Timer::default(),
            last: now,
            running: false,
        }
    }

    /// Start (or restart) a run from preset `index`.
    fn start(&mut self, index: usize, now: Instant) {
        let preset = index.min(PRESETS.len() - 1);
        self.view = Timer {
            remaining: PRESETS[preset],
            preset,
        };
        self.last = now;
        self.running = true;
    }

    /// Roll the countdown down. Returns `true` on the tick where the run hit
    /// zero, so the caller fires exactly one notification.
    fn step(&mut self, now: Instant) -> bool {
        if !self.running {
            return false;
        }
        if now.duration_since(self.last) < Duration::from_secs(1) {
            return false;
        }
        self.last = now;
        match self.view.remaining.checked_sub(1) {
            Some(0) | None => {
                self.view.remaining = 0;
                self.running = false;
                true
            }
            Some(left) => {
                self.view.remaining = left;
                false
            }
        }
    }

    /// Whether a preset click should start a fresh run: any click starts or
    /// restarts, which is what a single "1 min" button must do.
    fn click(&mut self, index: usize, now: Instant) -> bool {
        self.start(index, now);
        true
    }
}

/// Nominal frame budget. Springs integrate against real elapsed time, so a slower
/// display changes smoothness, not the motion curve.
const NOMINAL_FRAME: Duration = Duration::from_micros(16_667);
/// When fully idle, yield this long between message-queue polls instead of spinning.
/// Also the hover-polling cadence: a collapsed island is click-through, so the
/// pointer has to be sampled rather than awaited. 25 ms is under one frame at
/// 40 Hz and still leaves the process ~0% CPU.
const IDLE_POLL: Duration = Duration::from_millis(25);
/// Power-state poll cadence. `GetSystemPowerStatus` is cheap, but there is no
/// change notification worth wiring, and 500 ms is well inside the HUD's 3 s
/// window, so a plug/unplug is never missed.
const POWER_POLL: Duration = Duration::from_millis(500);

/// Arm a volume HUD when the master volume or mute state *changes*. Same rule as
/// the battery: the transition is the event, holding the key just extends it.
/// Returns `true` when the layer was armed and a repaint is due.
fn volume_hud(
    a: &audio::AudioState,
    last: &mut Option<(u8, bool)>,
    now: Instant,
    layer: &mut hud::HudLayer,
) -> bool {
    let (Some(percent), Some(muted)) = (a.volume, a.muted) else {
        return false;
    };
    // First reading only announces a muted endpoint; a routine 30 % at startup
    // is not news.
    let show = match last.replace((percent, muted)) {
        None => muted,
        Some(prev) => prev != (percent, muted),
    };
    if !show {
        return false;
    }
    layer.arm(hud::Hud::Volume { percent, muted }, now);
    true
}

/// Arm a battery HUD when the power state *changes* to something worth showing.
///
/// Firing on every poll would pin the HUD to the pill forever; the point is the
/// transition (plugged in, unplugged, crossed into low), not the steady state.
/// Returns `true` when the layer was armed and a repaint is due.
fn power_hud(
    p: &power::Power,
    last: &mut Option<power::Power>,
    now: Instant,
    layer: &mut hud::HudLayer,
) -> bool {
    let previous = last.replace(*p);
    // No battery (desktop): nothing to show, and no transition to react to.
    let Some(percent) = p.percent else {
        return false;
    };
    let show = match previous {
        // First reading: only announce a low battery, never a routine 80 %.
        None => power::is_low(Some(percent)),
        Some(prev) => {
            prev.charging != p.charging
                || power::is_low(Some(percent)) != power::is_low(prev.percent)
        }
    };
    if !show {
        return false;
    }
    layer.arm(
        hud::Hud::Battery {
            percent,
            charging: p.charging,
            low: !p.charging && power::is_low(Some(percent)),
        },
        now,
    );
    true
}

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

    // The media worker owns its own COM apartment and only publishes plain data,
    // so the UI thread never touches WinRT. Started once for the process.
    // The metrics sampler also runs on its own thread and publishes a plain
    // snapshot; PDH and Win32 reads never happen on the UI thread.
    let stats = Sampler::spawn();
    let media = Media::start();

    // HUD layer + the power reading it reacts to. Owned by the run loop; the UI
    // only ever sees the current HUD through `ViewState`.
    let mut hud = hud::HudLayer::default();
    let mut last_power = Instant::now() - POWER_POLL;
    let mut last_power_state: Option<power::Power> = None;
    // Audio (volume / mic) runs on its own worker thread — the COM pointer and
    // 250 ms cadence live there, the app only reads the snapshot.
    let audio = Audio::start();
    let procs = Processes::start();
    let clip = Clipboard::start();
    let picker = Picker::new();
    let weather = Weather::new();
    let mut last_audio_state: Option<(u8, bool)> = None;

    // Countdown: the app owns the clock, the UI only formats. Kept next to the
    // island state because the run continues while the island is collapsed.
    let mut timer = Countdown::new(Instant::now());

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
                    let (lx, ly) = (x / scale, y / scale);
                    if let Some(cmd) = view.media_command(lx, ly) {
                        media.send(cmd);
                        redraw = true;
                    } else if let Some(preset) = view.timer_preset(lx, ly) {
                        if timer.click(preset, Instant::now()) {
                            view.timer = timer.view;
                            redraw = true;
                        }
                    } else if let Some(hit) = view.picker_hit(lx, ly) {
                        // 0 is the readout; every other hit is trail swatch
                        // `i - 1`. Either way, copy that color's hex.
                        let hex = if hit == 0 {
                            picker.current().map(|c| c.hex())
                        } else {
                            picker.trail().get(hit - 1).map(|c| c.hex())
                        };
                        if let Some(hex) = hex {
                            let _ = Clipboard::restore(&hex);
                        }
                    } else if let Some(hit) = view.clipboard_hit(lx, ly) {
                        // 0 is the consent row when off or the capture row when
                        // on; every other hit is history row `i - 1`.
                        if !clip.enabled() {
                            clip.set_enabled(true);
                        } else if hit == 0 {
                            clip.clear();
                        } else if let Some(entry) = view.clip_entries.get(hit - 1) {
                            let _ = Clipboard::restore(&entry.text.clone());
                        }
                        redraw = true;
                    } else if view.click_tab(lx, ly).is_some() {
                        redraw = true;
                    }
                }
                Event::CursorMoved { x, y } => {
                    let (lx, ly) = (x / scale, y / scale);
                    let tab = view.tab_at(lx, ly);
                    let row = view.row_at(lx, ly);
                    if tab != view.hover_tab || row != view.hover_row {
                        view.hover_tab = tab;
                        view.hover_row = row;
                        redraw = true;
                    }
                }
                Event::CursorLeft => {
                    if view.hover_tab.take().is_some() | view.hover_row.take().is_some() {
                        redraw = true;
                    }
                }
                Event::Resized { .. } | Event::Redraw => redraw = true,
                Event::Quit => return std::process::ExitCode::SUCCESS,
            }
        }

        // Pull the latest snapshot every iteration. The worker publishes at 1 Hz,
        // so this is a mutex read, not a COM call.
        let snapshot = media.snapshot();
        if snapshot != view.media {
            view.media = snapshot;
            redraw = true;
        }

        // Same rule for the sampler: the 1 Hz worker republishes, this only
        // notices a change and marks the frame dirty.
        let latest = stats.snapshot();
        if latest.current != view.stats.current {
            view.stats = latest;
            redraw = true;
        }

        let now = Instant::now();
        let dt = now.duration_since(last).min(Duration::from_millis(100));

        // HUD layer: expiry first (a HUD that just went away needs a repaint),
        // then the power poll. The getter is cheap and in-process, so it runs on
        // a half-second cadence right here rather than on a worker thread.
        if hud.step(now) {
            view.hud = None;
            redraw = true;
        }
        if now.duration_since(last_power) >= POWER_POLL {
            last_power = now;
            if let Some(p) = power::read() {
                let armed = power_hud(&p, &mut last_power_state, now, &mut hud);
                if armed {
                    view.hud = hud.current();
                    redraw = true;
                }
            }
        }
        // Audio: read the worker's snapshot every frame (a mutex read) and arm
        // the volume HUD on change. The mic flag is not a HUD — it is a standing
        // indicator, so it just rides on the view.
        let a = audio.snapshot();
        view.procs = procs.snapshot();
        // The picker samples wherever the cursor is, tab or no tab: one
        // GetCursorPos plus one GetPixel per tick, both microseconds.
        if view.active_tab == ui::PICKER_TAB {
            let mut pt = POINT::default();
            // SAFETY: `pt` is a valid POINT for the call's duration.
            if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                let before = picker.current();
                if picker.sample_at(pt.x, pt.y).is_some() && picker.current() != before {
                    redraw = true;
                }
            }
        }
        let clip_entries = clip.snapshot();
        let clip_enabled = clip.enabled();
        if view.active_tab == ui::PICKER_TAB {
            view.picker = picker.clone();
            view.weather = weather.snapshot();
        }
        if clip_entries != view.clip_entries || clip_enabled != view.clip_enabled {
            view.clip_entries = clip_entries;
            view.clip_enabled = clip_enabled;
            redraw = true;
        }
        if a.mic_active != view.mic_active {
            view.mic_active = a.mic_active;
            redraw = true;
        }
        if volume_hud(&a, &mut last_audio_state, now, &mut hud) {
            view.hud = hud.current();
            redraw = true;
        }

        // The countdown runs on its own clock: while one is up the pill shows
        // it, so the frame is dirty every second regardless of hover or media.
        if timer.view.remaining != view.timer.remaining || timer.view.preset != view.timer.preset
        {
            view.timer = timer.view;
            redraw = true;
        }
        if timer.step(now) {
            // Hit zero: tray balloon, then the pill returns to idle.
            crate::platform::tray::notify("Timer", &format!("{} is up", view.timer.label()));
            hud.arm(Hud::Timer { remaining: 0, running: false }, now);
            view.hud = hud.current();
            redraw = true;
        } else if timer.running {
            hud.arm(
                Hud::Timer {
                    remaining: timer.view.remaining,
                    running: true,
                },
                now,
            );
            view.hud = hud.current();
            redraw = true;
        }

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
