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

use crate::platform::settings_window::{self, SettingsEvent, SettingsWindow};
use crate::platform::{Event, Overlay, Renderer};
use crate::services::audio::{self, Audio};
use crate::services::calendars::Calendar;
use crate::services::clipboard::Clipboard;
use crate::services::downloads;
use crate::services::media::Media;
use crate::services::metrics::Sampler;
use crate::services::net::{self, NetState};
use crate::services::picker::Picker;
use crate::services::power;
use crate::services::processes::Processes;
use crate::services::shelf::Shelf;
use crate::services::status;
use crate::services::terminal::Terminal;
use crate::services::weather::Weather;
use crate::services::{pet, settings};
use crate::ui;
use hud::Hud;
use std::path::PathBuf;
use timer::{Timer, PRESETS};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// Apply a Settings sheet click and persist it. Sheet clicks are how the user
/// reaches every knob now, so the view carries the mirrored state (pet on/off,
/// name, size, notch color) and the caller repaints from it. Every knob writes
/// to `settings.toml` immediately — settings are read at boot, so a change that
/// was not written would not survive a restart.
///
/// Returns the newly configured pet slug when the equipped pet changed, so the
/// caller can keep its boot-time copy in sync with what was just written —
/// `None` for every other action.
fn apply_settings_action(
    action: &ui::settings::Action,
    pet: &mut pet::PetState,
    pet_on: &mut bool,
    clip: &Clipboard,
    view: &mut ui::ViewState,
) -> Option<String> {
    let mut t = settings::load();
    let equipped = match action {
        ui::settings::Action::PetToggle => {
            *pet_on = !*pet_on;
            settings::set_bool(&mut t, "pet_enabled", *pet_on);
            view.pet_on = *pet_on;
            None
        }
        ui::settings::Action::Equip(slug) => {
            pet.reload(slug);
            pet::set_equipped(slug);
            settings::set_string(&mut t, "pet_slug", slug);
            Some(slug.clone())
        }
        ui::settings::Action::Install(slug) => {
            pet::request_install(slug);
            None
        }
        ui::settings::Action::Remove(slug) => {
            pet::request_uninstall(slug);
            None
        }
        ui::settings::Action::NotchColor(i) => {
            if let Some(color) = ui::settings::NOTCH_SWATCHES.get(*i) {
                // The swatch is stored as the hex it came from, so a restart
                // restores exactly the color that was clicked.
                settings::set_string(&mut t, "notch_bg", &color.to_hex());
                view.notch_bg = *color;
            }
            None
        }
        ui::settings::Action::ClipToggle => {
            let next = !clip.enabled();
            clip.set_enabled(next);
            settings::set_bool(&mut t, "clip_enabled", next);
            view.clip_enabled = next;
            None
        }
        ui::settings::Action::AccentProgressToggle => {
            let next = !view.accent_progress;
            settings::set_bool(&mut t, "accent_progress", next);
            view.accent_progress = next;
            None
        }
        ui::settings::Action::PetScaleDown | ui::settings::Action::PetScaleUp => {
            let up = matches!(action, ui::settings::Action::PetScaleUp);
            let step = ui::settings::SCALE_STEP * if up { 1.0 } else { -1.0 };
            let next = ui::settings::scale_snapped(settings::get_f64(&t, "pet_scale", 1.0) + step);
            settings::set_f64(&mut t, "pet_scale", next);
            // The view carries the offset from neutral, not the absolute size,
            // so `ViewState::default()` already reads as the un-zoomed pet.
            view.pet_zoom = (next - 1.0) as f32;
            None
        }
    };
    settings::save(&t);
    equipped
}

/// The notch body background color from settings. Falls back to the notch's
/// historical black for a missing or hand-mangled value: a settings file that
/// was edited by hand must not surface as a color nobody picked.
fn notch_bg_from(table: &toml::Table) -> crate::core::geom::Rgba {
    crate::core::geom::Rgba::parse_hex(&settings::get_string(table, "notch_bg", ""))
        .unwrap_or(crate::core::geom::Rgba::BLACK)
}

/// Build the Settings sheet's state for one paint: the knobs' current values
/// plus the gallery the user is actually looking at.
fn settings_model(
    pet_on: bool,
    pet_slugs: &[String],
    clip_on: bool,
    equipped: Option<String>,
    view: &ui::ViewState,
    gallery: pet::GallerySnapshot,
    scroll: f32,
) -> ui::settings::Model {
    ui::settings::Model {
        pet_on,
        pet_count: pet_slugs.len(),
        equipped,
        notch_bg: view.notch_bg,
        clip_on,
        accent_progress: view.accent_progress,
        pet_scale: settings::get_f64(&settings::load(), "pet_scale", 1.0),
        gallery: Some(gallery),
        scroll,
    }
}

/// Hold the Settings sheet's scroll inside its content, so the wheel can never
/// leave the sheet parked past its last row with the window looking empty.
/// Rebuilds the sheet from the caller's already-built model to learn its height,
/// which is cheap (it is pure layout, no I/O) and keeps the two from disagreeing.
/// `content_h` does not depend on the model's scroll, so the caller's value is fine.
fn clamp_settings_scroll(fh: f32, model: &ui::settings::Model, wanted: f32) -> f32 {
    let max = (ui::settings::sheet(settings_window::LOGICAL_W, fh, model).content_h - fh).max(0.0);
    wanted.clamp(0.0, max)
}

/// The wall-clock runner entry point.
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
const IDLE_POLL: Duration = Duration::from_millis(200);

/// TEMP DEBUG: which site set redraw this iteration.
static DBG_REASON: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const DBG_REASONS: [&str; 31] = [
    "none", "vis_toggle", "filedrop", "pill_toggle", "media_cmd", "timer_preset", "clip_hit",
    "cal_hit", "shelf_hit", "settings_hit", "tab_click", "cursor_moved", "cursor_left", "key",
    "resize", "media_snap", "stats_snap", "petframe", "hud_expire", "power", "drive", "picker",
    "clip_snap", "cal_snap", "usage_snap", "dl_snap", "mic", "volume", "timer_view",
    "timer_zero", "timer_run",
];
/// TEMP DEBUG: microseconds spent in the draw block of the last painted frame.
static DBG_DRAW_US: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static DBG_BUILD_US: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static DBG_PRESENT_US: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn dbg_reason(i: usize) {
    DBG_REASON.fetch_or(1 << i, std::sync::atomic::Ordering::Relaxed);
}
fn dbg_take_reasons() -> String {
    let bits = DBG_REASON.swap(0, std::sync::atomic::Ordering::Relaxed);
    let mut out = Vec::new();
    for (i, name) in DBG_REASONS.iter().enumerate() {
        if bits & (1 << i) != 0 {
            out.push(*name);
        }
    }
    if out.is_empty() {
        "none".to_string()
    } else {
        out.join("+")
    }
}


/// TEMP DEBUG
pub fn dbg_spawn_log(name: &str) {
    use std::io::Write as _;
    if std::env::var("ARC_DBG_LOOP").is_err() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\Users\mrenm\AppData\Local\hermes\cache\scratch\arc_dbg.log")
    {
        let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        let _ = writeln!(f, "SPAWN {} tid={}", name, tid);
    }
    let _ = name;
}
/// Power-state poll cadence. `GetSystemPowerStatus` is cheap, but there is no
/// change notification worth wiring, and 500 ms is well inside the HUD's 3 s
/// window, so a plug/unplug is never missed.
const POWER_POLL: Duration = Duration::from_millis(500);
/// Connectivity, VPN tunnel, brightness: state, not events — 2 s is plenty
/// and keeps the GetIfTable2 walk off the hot path.
const NET_POLL: Duration = Duration::from_secs(2);

/// Arm a volume HUD when the master volume or mute state *changes*. Same rule as
/// the battery: the transition is the event, holding the key just extends it.
/// Returns `true` when the layer was armed and a repaint is due.
/// Is either shift key down right now? Modifiers are not delivered as window
/// messages to a click-through overlay, so this is polled at the click.
fn shift_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LSHIFT, VK_RSHIFT, VK_SHIFT,
    };
    // SAFETY: a pure state query, no pointers.
    unsafe {
        [VK_SHIFT, VK_LSHIFT, VK_RSHIFT]
            .iter()
            .any(|k| GetAsyncKeyState(k.0 as i32) < 0)
    }
}

/// A subscription's display name from its URL: the host, which is what the
/// user recognises. `None` when the clipboard text is not a URL at all.
fn sub_name(url: &str) -> Option<String> {
    let t = url.trim();
    let rest = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    (!host.is_empty() && host.contains('.')).then(|| host.to_string())
}

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

/// Arm a connectivity HUD when the network state *changes*.
///
/// VPN shows once per tunnel appearance (not every poll), offline shows on
/// loss, online shows on recovery. Same transition rule as `power_hud`: the
/// event is the change, not the steady state.
fn net_hud(
    s: &NetState,
    last: &mut Option<NetState>,
    now: Instant,
    layer: &mut hud::HudLayer,
) -> bool {
    let previous = last.replace(s.clone());
    let hud = match previous {
        // First reading: a connected VPN is worth naming; a plain network isn't.
        None => s.vpn.as_ref().map(|n| hud::Hud::Vpn(Some(n.clone()))),
        Some(prev) => {
            if prev.online && !s.online {
                Some(hud::Hud::Online(false, None))
            } else if !prev.online && s.online {
                Some(hud::Hud::Online(true, s.ssid.clone()))
            } else if prev.vpn != s.vpn {
                Some(hud::Hud::Vpn(s.vpn.clone()))
            } else {
                None
            }
        }
    };
    match hud {
        Some(h) => {
            layer.arm(h, now);
            true
        }
        None => false,
    }
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
    let mut last_net = Instant::now() - NET_POLL;
    let mut last_net_state: Option<NetState> = None;
    let mut next_notif = Instant::now();
    let mut last_focus: Option<bool> = None;
    let mut last_brightness: Option<u8> = None;
    // drives: outer None = first reading, inner None = nothing plugged in
    let mut last_drive: Option<Option<String>> = None;
    let mut last_power_state: Option<power::Power> = None;
    // Audio (volume / mic) runs on its own worker thread — the COM pointer and
    // 250 ms cadence live there, the app only reads the snapshot.
    let audio = Audio::start();
    let procs = Processes::start();
    let clip = Clipboard::start();
    let picker = Picker::new();
    let weather = Weather::new();
    let cals = Calendar::new();
    let usage = crate::services::usage::Usage::new();
    let downloads = downloads::Downloads::new();
    let mut shelf = Shelf::load();
    let mut last_audio_state: Option<(u8, bool)> = None;
    // The terminal is a long-lived shell: spawn once, keep it for the app's
    // lifetime. A failed spawn leaves `terminal` as a dead-but-valid surface
    // whose `error()` the UI shows instead of a blank grid.
    let terminal = Terminal::new();
    // Focus is expensive to change and easy to thrash, so it flips only on a
    // transition, not every frame.
    let mut had_keys = false;

    // Countdown: the app owns the clock, the UI only formats. Kept next to the
    // island state because the run continues while the island is collapsed.
    let mut timer = Countdown::new(Instant::now());

    let mut state = IslandState::collapsed();
    // The tab selection and hover persist across frames; everything else the UI
    // needs is derived from the size springs.
    let mut view = ui::ViewState::default();
    let mut settings_win: Option<SettingsWindow> = None;
    let mut settings_dirty = false;
    // The idle-notch pet: settings decide whether it shows and which petdex
    // install it uses. Only the selected sheet is read and interned; the
    // Settings picker needs the install count and the current name, nothing more.
    let boot = settings::load();
    let mut pet = pet::PetState::default();
    let mut pet_on = settings::get_bool(&boot, "pet_enabled", true);
    let mut pet_slug = settings::get_string(&boot, "pet_slug", "");
    let mut pet_slugs = pet::installed();
    pet.reload(&pet_slug);
    view.pet_on = pet_on;
    view.notch_bg = notch_bg_from(&boot);
    view.pet_zoom = (ui::settings::scale_snapped(settings::get_f64(&boot, "pet_scale", 1.0)) - 1.0) as f32;
    view.accent_progress = settings::get_bool(&boot, "accent_progress", false);
    // The gallery lives in the pet service's worker; asking for a snapshot at
    // boot starts the manifest fetch, so the sheet has tiles to show the first
    // time the user opens it instead of an empty "Loading…".
    let mut gallery = pet::snapshot();
    let mut gallery_v = gallery.version;
    pet::request_thumbs(&gallery.tiles.iter().map(|t| t.slug.clone()).collect::<Vec<_>>());
    // Settings window scroll, in logical px. The window is fixed-size, so this
    // is the only thing that moves the sheet's contents.
    let mut settings_scroll = 0.0_f32;
    overlay.show();

    // First paint before the loop: with no events yet, the dirty-frame loop would
    // park on frame zero and show an undefined (never-presented) window until the
    // first hotkey or click.
    overlay.resize(state.frame_width(), state.frame_height());
    renderer.resize(
        overlay.dpi().snap(state.frame_width()),
        overlay.dpi().snap(state.frame_height()),
    );
    view.island_width = state.logical_width();
    view.island_height = state.logical_height();
    // The header pin mirrors the island's pinned/hidden user-vis state so
    // the corner icon and the Ctrl+Shift+Z hotkey can never disagree.
    view.pinned = state.user_vis() == crate::app::state::UserVis::Pinned;
    view.peek = state.mode() == crate::app::state::WindowMode::Peek;
    let frame = ui::build(&view);
    renderer.present(&frame, overlay.dpi().scale, 0.0, state.body_offset_x(), true);

    let mut last = Instant::now();
    loop {
        let mut redraw = false;
        let scale = overlay.dpi().scale;
        // The persistent view must carry the current geometry: this iteration's
        // clicks are hit-tested against it.
        view.island_width = state.logical_width();
        view.island_height = state.logical_height();
        // The header pin mirrors the island's pinned/hidden user-vis state so
        // the corner icon and the Ctrl+Shift+Z hotkey can never disagree.
        view.pinned = state.user_vis() == crate::app::state::UserVis::Pinned;
        view.peek = state.mode() == crate::app::state::WindowMode::Peek;
        overlay.set_body(
            state.logical_width(),
            state.logical_height(),
            state.frame_width(),
        );

        for event in overlay.pump_events() {
            match event {
                Event::ToggleIsland => state.toggle(),
                // Ctrl+Shift+Z: user owns visibility from here on.
                Event::ToggleVisibility => {
                    state.toggle_visibility();
                    dbg_reason(1);
                    redraw = true;
                }
                // A file dropped on the island pins to the shelf.
                Event::FilesDropped { paths } => {
                    // NotchDrop's gesture: the notch answers the drop by
                    // opening on the Shelf so the user sees where the file
                    // went, instead of a file silently vanishing into a tab
                    // they were not looking at.
                    if state.open_for_drop() {
                        dbg_reason(2);
                        redraw = true;
                    }
                    if !paths.is_empty() {
                        view.select_tab(ui::SHELF_TAB);
                    }
                    for p in paths {
                        if shelf.pin(std::path::Path::new(&p)) {
                            dbg_reason(2);
                            redraw = true;
                        }
                    }
                    view.shelf = shelf.items().to_vec();
                }
                Event::FullscreenEnter => state.fullscreen_enter(),
                // Back to whatever we were showing before the fullscreen app —
                // not unconditionally the panel.
                Event::FullscreenExit => state.fullscreen_exit(),
                // Clicks arrive in physical client pixels; the UI lays out in
                // logical ones, so scale before hit-testing.
                Event::LeftClick { x, y } => {
                    // The window is the fixed max frame; the body sits centred
                    // inside it. Translate into body space and ignore clicks on
                    // the transparent ear margins — they look like desktop and
                    // must behave like desktop.
                    let lx = x / scale - state.body_offset_x();
                    let ly = y / scale;
                    if lx < 0.0 || lx > state.logical_width() {
                        continue;
                    }
                    // A collapsed pill is pressable: any click inside it
                    // toggles the island (DynamicNotch/Atoll behaviour).
                    if !state.expanded() {
                        state.toggle();
                        dbg_reason(3);
                        redraw = true;
                    }
                    // The tab strip is checked FIRST, above every content
                    // hit. Content controls may be drawn close to the strip;
                    // letting them win meant a content hit swallowed the
                    // click and the island refused to leave the tab.
                    if view.click_tab(lx, ly).is_some() {
                        dbg_reason(10);
                        redraw = true;
                    } else if let Some(corner) = view.corner_at(lx, ly) {
                        match corner {
                            ui::CornerIcon::Settings => {
                                // Same path as the tray item: raise the flag,
                                // let the app loop own window creation.
                                crate::platform::settings_window::request_open();
                                dbg_reason(16);
                                redraw = true;
                            }
                            ui::CornerIcon::Pin => {
                                state.toggle_visibility();
                                dbg_reason(17);
                                redraw = true;
                            }
                        }
                    } else if let Some(cmd) = view.media_command(lx, ly) {
                        media.send(cmd);
                        dbg_reason(4);
                        redraw = true;
                    } else if let Some(preset) = view.timer_preset(lx, ly) {
                        if timer.click(preset, Instant::now()) {
                            view.timer = timer.view;
                            dbg_reason(5);
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
                        // VNotch's copy surface: confirm the restore with a
                        // transient pill toast so the gesture is visible.
                        if clip.enabled() && hit > 0 {
                            hud.arm(
                                hud::Hud::Notification {
                                    app: "Clipboard".into(),
                                    title: "Copied".into(),
                                    body: view
                                        .clip_entries
                                        .get(hit - 1)
                                        .map(|e| e.text.clone())
                                        .unwrap_or_default(),
                                },
                                std::time::Instant::now(),
                            );
                            view.hud = hud.current();
                        }
                        dbg_reason(6);
                        redraw = true;
                    } else if let Some(hit) = view.calendar_hit(lx, ly) {
                        // Row 0 pastes the clipboard as a new `.ics` URL; any
                        // other row removes that subscription.
                        if hit == 0 {
                            if let Some(url) = crate::services::clipboard::paste() {
                                if let Some(name) = sub_name(url.trim()) {
                                    cals.subscribe(&name, url.trim());
                                    view.cal = cals.snapshot();
                                }
                            }
                        } else if let Some(sub) = view.cal.subs.get(hit - 1) {
                            cals.unsubscribe(&sub.url.clone());
                            view.cal = cals.snapshot();
                        }
                        dbg_reason(7);
                        redraw = true;
                    } else if let Some(i) = view.shelf_hit(lx, ly) {
                        // A pinned tile opens with its default handler; a
                        // missing one drops from the shelf. Shift sends it to
                        // LocalSend instead, which opens its own target picker.
                        if let Some(item) = view.shelf.get(i) {
                            if shift_held() {
                                crate::services::localsend::handoff(&[PathBuf::from(&item.path)]);
                            } else if !crate::services::shelf::open(std::path::Path::new(
                                &item.path,
                            )) {
                                shelf.unpin(&item.path.clone());
                                view.shelf = shelf.items().to_vec();
                            }
                        }
                        dbg_reason(8);
                        redraw = true;
                    }
                }
                Event::CursorMoved { x, y } => {
                    // Same body-space translation as clicks (only arrives while
                    // the panel is interactive, so the frame-vs-body gap is the
                    // ear margin plus the collapsed-width centring).
                    let lx = x / scale - state.body_offset_x();
                    let ly = y / scale;
                    let tab = view.tab_at(lx, ly);
                    let row = view.row_at(lx, ly);
                    if tab != view.hover_tab || row != view.hover_row {
                        view.hover_tab = tab;
                        view.hover_row = row;
                        dbg_reason(11);
                        redraw = true;
                    }
                }
                Event::CursorLeft => {
                    if view.hover_tab.take().is_some() | view.hover_row.take().is_some() {
                        dbg_reason(12);
                        redraw = true;
                    }
                }
                // Printable keys go to the shell while the Terminal tab is
                // open. The island is click-through when collapsed, so these
                // only arrive while the panel is interactive.
                Event::Key { ch } => {
                    if view.active_tab == ui::TERMINAL_TAB && view.content_visible() {
                        if ch == '\r' {
                            terminal.line("");
                        } else if ch == '\x7f' || ch == '\u{8}' {
                            terminal.key(0x08);
                        } else if !ch.is_control() {
                            let mut buf = [0u8; 4];
                            terminal.write(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        dbg_reason(13);
                        redraw = true;
                    }
                }
                Event::Resized { .. } | Event::Redraw => {
                    // The grid is sized to the panel, so a resize means the
                    // shell's viewport changed too.
                    if view.active_tab == ui::TERMINAL_TAB {
                        terminal.resize(80, 24);
                    }
                    dbg_reason(14);
                    redraw = true;
                }
                Event::Quit => return std::process::ExitCode::SUCCESS,
                // Tray right-click → "Settings…": the standalone window answers,
                // not the notch. This arm only nudges the renderer; the real
                // toggle lives next to the settings window's pump further down.
                Event::OpenSettings => {
                    crate::platform::settings_window::request_open();
                }
            }
        }

        if std::env::var("ARC_DBG_SKIP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0) == 1 {
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        // Pull the latest snapshot every iteration. The worker publishes at 1 Hz,
        // so this is a mutex read, not a COM call.
        // A probe switch for the playback visuals: the app renders as if a
        // track is playing (placeholder art + fake title), so the pill row and
        // the auto-expand can be verified without a live media session.
        let snapshot = if std::env::var("ARC_DEMO_MEDIA").ok().as_deref() == Some("1") {
            let mut m = crate::services::media::MediaState::default();
            m.title = "Late Night Drive".into();
            m.artist = "Example Artist".into();
            m.source = "Demo".into();
            m.playing = true;
            m.position = std::time::Duration::from_secs(85);
            m.duration = Some(std::time::Duration::from_secs(210));
            m
        } else {
            media.snapshot()
        };
        if snapshot != view.media {
            // Playback policy (MusicNotch / Boring Notch behaviour): a track
            // starting opens the island, and playback ending closes it again —
            // but only if WE opened it, never a user-opened panel.
            let (was, now_playing) = (view.media.playing, snapshot.playing);
            if !was && now_playing {
                if std::env::var("ARC_DEMO_MEDIA_NOEXPAND").ok().as_deref() != Some("1")
                    && state.open_for_playback()
                {
                    dbg_reason(18);
                }
            } else if was && !now_playing && state.close_after_playback() {
                dbg_reason(19);
            }
            view.media = snapshot;
            dbg_reason(15);
            redraw = true;
        }

        // Same rule for the sampler: the 1 Hz worker republishes, this only
        // notices a change and marks the frame dirty.
        let latest = stats.snapshot();
        if latest.current != view.stats.current {
            view.stats = latest;
            dbg_reason(16);
            redraw = true;
        }


        if std::env::var("ARC_DBG_SKIP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0) == 2 {
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        let now = Instant::now();
        let dt = now.duration_since(last).min(Duration::from_millis(100));

        // The idle pet animates on its own clock: the frame index is a pure
        // function of wall time, so a changed frame means repaint. Only the
        // collapsed pill draws the sprite, so the panel-open case skips the
        // check entirely — no 5 Hz wakeups while the user reads the panel.
        if !state.expanded() && pet_on {
            let frame = pet.frame_at(now);
            if frame != view.pet {
                view.pet = frame;
                dbg_reason(17);
                redraw = true;
            }
        }

        // HUD layer: expiry first (a HUD that just went away needs a repaint),
        // then the power poll. The getter is cheap and in-process, so it runs on
        // a half-second cadence right here rather than on a worker thread.
        if hud.step(now) {
            view.hud = None;
            dbg_reason(18);
            redraw = true;
        }
        // Notification-banner probe: `ARC_DEMO_NOTIFY=1` fires a toast every
        // 5 s so the two-line banner can be verified without a real listener.
        if std::env::var("ARC_DEMO_NOTIFY").ok().as_deref() == Some("1")
            && now >= next_notif
        {
            next_notif = now + std::time::Duration::from_secs(5);
            hud.arm(
                hud::Hud::Notification {
                    app: "Outlook".into(),
                    title: "Design review".into(),
                    body: "Arc notch dock is ready for review".into(),
                },
                now,
            );
            view.hud = hud.current();
            dbg_reason(20);
            redraw = true;
        }
        if now.duration_since(last_power) >= POWER_POLL {
            last_power = now;
            if let Some(p) = power::read() {
                let armed = power_hud(&p, &mut last_power_state, now, &mut hud);
                if armed {
                    view.hud = hud.current();
                    dbg_reason(19);
                    redraw = true;
                }
            }
        }
        if now.duration_since(last_net) >= NET_POLL {
            last_net = now;
            let mut armed = net_hud(&net::sample(), &mut last_net_state, now, &mut hud);
            // Focus: transition-only, same rule as the other HUDs.
            let focus = status::focus_on();
            if last_focus.replace(focus) == Some(!focus) {
                hud.arm(hud::Hud::Focus(focus), now);
                armed = true;
            }
            // Brightness: DDC/CI read; arm on a real change (hotkey, slider).
            // Debounced by 2 %: the panel's own ramp crosses many values per
            // key-hold, and the HUD's re-arm timer already handles a stream.
            if let Some(b) = status::brightness() {
                let jumped = match last_brightness.replace(b) {
                    Some(prev) => prev.abs_diff(b) >= 2,
                    None => false,
                };
                if jumped {
                    hud.arm(hud::Hud::Brightness(b), now);
                    armed = true;
                }
            }
            // Removable drives: appear/eject transitions only, same rule.
            let drive = status::removable_drive();
            if matches!(last_drive, Some(prev) if prev != drive) {
                hud.arm(hud::Hud::Drive(drive.clone()), now);
                armed = true;
            }
            last_drive = Some(drive);
            if armed {
                view.hud = hud.current();
                dbg_reason(20);
                redraw = true;
            }
        }

        if std::env::var("ARC_DBG_SKIP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0) == 3 {
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        // Audio: read the worker's snapshot every frame (a mutex read) and arm
        // the volume HUD on change. The mic flag is not a HUD — it is a standing
        // indicator, so it just rides on the view.
        let a = audio.snapshot();
        view.procs = procs.snapshot();
        // The terminal's grid is owned by the service (a reader thread feeds
        // it); the view only mirrors it so `build` stays pure. A dead shell
        // stops repainting, which is the correct look.
        if view.active_tab == ui::TERMINAL_TAB {
            view.terminal = Some(terminal.screen());
            view.terminal_error = terminal.error().map(str::to_string);
        }
        // The picker samples wherever the cursor is, tab or no tab: one
        // GetCursorPos plus one GetPixel per tick, both microseconds.
        if view.active_tab == ui::PICKER_TAB {
            let mut pt = POINT::default();
            // SAFETY: `pt` is a valid POINT for the call's duration.
            if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                let before = picker.current();
                if picker.sample_at(pt.x, pt.y).is_some() && picker.current() != before {
                    dbg_reason(21);
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
            dbg_reason(22);
            redraw = true;
        }
        let cal = cals.snapshot();
        if cal != view.cal {
            view.cal = cal;
            dbg_reason(23);
            redraw = true;
        }

        if std::env::var("ARC_DBG_SKIP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0) == 4 {
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        let usg = usage.snapshot();
        if usg != view.usage {
            view.usage = usg;
            dbg_reason(24);
            redraw = true;
        }
        let dl = downloads.snapshot();
        if dl != view.downloads {
            view.downloads = dl;
            dbg_reason(25);
            redraw = true;
        }
        if a.mic_active != view.mic_active {
            view.mic_active = a.mic_active;
            dbg_reason(26);
            redraw = true;
        }
        if volume_hud(&a, &mut last_audio_state, now, &mut hud) {
            view.hud = hud.current();
            dbg_reason(27);
            redraw = true;
        }

        // The countdown runs on its own clock: while one is up the pill shows
        // it, so the frame is dirty every second regardless of hover or media.
        if timer.view.remaining != view.timer.remaining || timer.view.preset != view.timer.preset {
            view.timer = timer.view;
            dbg_reason(28);
            redraw = true;
        }
        if timer.step(now) {
            // Hit zero: tray balloon, then the pill returns to idle.
            crate::platform::tray::notify("Timer", &format!("{} is up", view.timer.label()));
            hud.arm(
                Hud::Timer {
                    remaining: 0,
                    running: false,
                },
                now,
            );
            view.hud = hud.current();
            dbg_reason(29);
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
            dbg_reason(30);
            redraw = true;
        }

        // Hover is polled, not event-driven (the collapsed window is
        // click-through), so it has to be stepped even on frames where nothing
        // else woke us up. It only ever folds a panel whose cursor left.
        let po = overlay.pointer_over();
        let hover_changed = state.hover_step(po);
        // The tab swap runs its own springs; while either is live the island
        // repaints so the crossfade/rise plays out.
        let swap_busy = view.tab_swap.step(dt.as_secs_f32());
        // Visualizer bars chase the output meter (or idle-shimmer when paused).
        // One peak feeds all four bars with a slight per-bar slant so the
        // row reads as a spectrum rather than a solid block.
        let p = audio.snapshot().peak.unwrap_or(0.0);
        let lvl = |i: usize| (p * (1.0 - 0.12 * i as f32)).max(0.0);
        let viz_busy = view.viz.step(
            dt.as_secs_f32(),
            [lvl(0), lvl(1), lvl(2), lvl(3)],
            view.media.playing,
        );
        let animating = state.step_dt(dt.as_secs_f32()) || swap_busy || viz_busy;


        if std::env::var("ARC_DBG_SKIP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0) == 5 {
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        // A hidden island is not just un-drawn: it is removed from the screen so
        // it cannot sit over the fullscreen app it is hiding for.
        if state.hidden() {
            overlay.hide();
        } else {
            overlay.show();
        }

        // The tray's "Settings…" lands here: the callback only raises a flag, so
        // the window is created on the app thread. Kept alive for the process —
        // closing it hides it, and reopening is instant.
        if settings_window::open_requested() {
            settings_win = match settings_win.take() {
                Some(mut existing) => {
                    existing.show();
                    Some(existing)
                }
                None => SettingsWindow::open(),
            };
            settings_dirty = true;
        }
        // The pet gallery lives on its own worker thread (manifest fetch,
        // thumbnail and full downloads), so the app polls its version instead of
        // blocking on it. A bumped version means the sheet's tiles changed —
        // mark it dirty if it is open, and refresh the installed list so a
        // finished download can be equipped. Thumbs are requested for every
        // tile that does not yet have one; the worker ignores slugs it has
        // already tried, so this converges instead of looping.
        let snap = pet::snapshot();
        if snap.version != gallery_v {
            gallery_v = snap.version;
            gallery = snap;
            let missing: Vec<String> = gallery
                .tiles
                .iter()
                .filter(|t| t.thumb.is_none() && !t.busy && !t.failed)
                .map(|t| t.slug.clone())
                .collect();
            if !missing.is_empty() {
                pet::request_thumbs(&missing);
            }
            let installed_now = pet::installed();
            if installed_now != pet_slugs {
                pet_slugs = installed_now;
                // Auto-load the pet if its install just finished and it is the
                // configured one — the notch should light up without the user
                // having to click Equipped after downloading.
                if !pet_slug.is_empty()
                    && pet_slugs.iter().any(|s| s == &pet_slug)
                    && pet.slug().is_none_or(|s| s != pet_slug)
                {
                    pet.reload(&pet_slug);
                }
            }
            settings_dirty = true;
        }
        if let Some(win) = settings_win.as_mut() {
            win.pump();
            let (fw, fh) = win.logical_size();
            let scale = win.scale().max(0.01);
            for ev in win.take_events() {
                match ev {
                    SettingsEvent::Click { x, y } => {
                        // Client-space physical px → logical px.
                        let model = settings_model(
                            pet_on,
                            &pet_slugs,
                            clip.enabled(),
                            pet.name().map(str::to_string),
                            &view,
                            gallery.clone(),
                            settings_scroll,
                        );
                        if let Some(action) = ui::settings::window_hit(
                            fw,
                            fh,
                            &model,
                            x as f32 / scale,
                            y as f32 / scale,
                        ) {
                            if let Some(slug) =
                                apply_settings_action(&action, &mut pet, &mut pet_on, &clip, &mut view)
                            {
                                pet_slug = slug;
                            }
                        }
                    }
                    SettingsEvent::Scroll { delta } => {
                        let step = delta as f32 / settings_window::WHEEL_DELTA as f32;
                        let model = settings_model(
                            pet_on,
                            &pet_slugs,
                            clip.enabled(),
                            pet.name().map(str::to_string),
                            &view,
                            gallery.clone(),
                            settings_scroll,
                        );
                        settings_scroll = clamp_settings_scroll(
                            fh,
                            &model,
                            settings_scroll - step * settings_window::SCROLL_STEP,
                        );
                    }
                    SettingsEvent::Closed => {}
                }
                settings_dirty = true;
            }
            if settings_dirty && win.visible() {
                settings_dirty = false;
                let model = settings_model(
                    pet_on,
                    &pet_slugs,
                    clip.enabled(),
                    pet.name().map(str::to_string),
                    &view,
                    gallery.clone(),
                    settings_scroll,
                );
                let frame = ui::settings::window_frame(fw, fh, &model);
                win.present(&frame);
            }
        }

        if std::env::var("ARC_DBG_LOOP").is_ok() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(r"C:\Users\mrenm\AppData\Local\hermes\cache\scratch\arc_dbg.log")
            {
                let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
                let _ = writeln!(
                    f,
                    "{} tid={} anim={} swap={} viz={} hover={} redraw={} po={} mode={} lw={} lh={} hw={} bh={} draw={} build={} present={} why={} acq={} enddraw={} rpresent={} nodes={}",
                    now.elapsed().as_micros(),
                    tid,
                    animating,
                    view.tab_swap.debug_busy(),
                    view.viz.debug_busy(),
                    hover_changed,
                    redraw,
                    po,
                    format!("{:?}", state.mode()),
                    state.logical_width(),
                    state.logical_height(),
                    overlay.hover_width_px(),
                    overlay.body_height(),
                    DBG_DRAW_US.load(std::sync::atomic::Ordering::Relaxed),
                    DBG_BUILD_US.load(std::sync::atomic::Ordering::Relaxed),
                    DBG_PRESENT_US.load(std::sync::atomic::Ordering::Relaxed),
                    dbg_take_reasons(),
                    crate::platform::render::DBG_ACQ_US.load(std::sync::atomic::Ordering::Relaxed),
                    crate::platform::render::DBG_ENDDRAW_US.load(std::sync::atomic::Ordering::Relaxed),
                    crate::platform::render::DBG_RS_PRESENT_US.load(std::sync::atomic::Ordering::Relaxed),
                    crate::platform::render::DBG_NODES.load(std::sync::atomic::Ordering::Relaxed)
                );
            }
        }

        if std::env::var("ARC_DBG_SKIP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0) == 6 {
            last = Instant::now();
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        if animating || hover_changed || redraw {
            last = now;
            let dbg_draw_t0 = Instant::now();

            // Fixed frame: these calls early-out unless mode/DPI/monitor
            // changed, so the morph costs no SetWindowPos and no ResizeBuffers.
            overlay.resize(state.frame_width(), state.frame_height());
            // Keyboard focus follows the Terminal tab: the island is
            // NOACTIVATE, so without this `WM_CHAR` goes to whatever app the
            // user is actually typing in — the wrong target for a shell.
            let wants_keys = state.expanded() && view.active_tab == ui::TERMINAL_TAB;
            if wants_keys != had_keys {
                had_keys = wants_keys;
                if wants_keys {
                    overlay.take_focus();
                } else {
                    overlay.release_focus();
                }
            }
            renderer.resize(
                overlay.dpi().snap(state.frame_width()),
                overlay.dpi().snap(state.frame_height()),
            );
            view.island_width = state.logical_width();
            view.island_height = state.logical_height();
            let dbg_t1 = Instant::now();
            let frame = ui::build(&view);
            let dbg_t2 = Instant::now();
            renderer.present(
                &frame,
                overlay.dpi().scale,
                dt.as_secs_f32(),
                state.body_offset_x(),
                animating,
            );
            let dbg_t3 = Instant::now();
            DBG_DRAW_US.store(
                dbg_t2.duration_since(dbg_draw_t0).as_micros() as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            DBG_BUILD_US.store(
                dbg_t1.duration_since(dbg_draw_t0).as_micros() as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            DBG_PRESENT_US.store(
                dbg_t3.duration_since(dbg_t2).as_micros() as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
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
