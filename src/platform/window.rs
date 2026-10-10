//! Overlay window: HWND ownership, placement, DPI, click-through policy.
//!
//! The island is a borderless top-center `WS_POPUP` that must never appear in a
//! taskbar or Alt-Tab list and must never steal focus from the foreground app.
//! Everything that enforces that lives here; `app` only sees [`Event`]s.
//!
//! Invariants this module guarantees:
//! - [`ClickThrough::Yes`] (collapsed) sets `WS_EX_TRANSPARENT`, so clicks fall
//!   through to whatever sits under the island. [`ClickThrough::No`] clears it.
//!   `WM_NCHITTEST` mirrors the same policy with `HTTRANSPARENT` (T2 in
//!   docs/07-RISKS.md: never a dead zone, never a focus steal).
//! - Placement is always recomputed from the *live* monitor work area, never from
//!   a cached rect, so `resize()` cannot drift off the top edge after a DPI change
//!   or a monitor re-arrangement.
//! - The process opts into `PER_MONITOR_AWARE_V2` **before** any window exists
//!   (docs/05 §2), so mixed-DPI desktops place the island on the right monitor.

use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, UnregisterHotKey,
    HOT_KEY_MODIFIERS, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, TME_LEAVE, TRACKMOUSEEVENT, VK_A,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowLongW, GetWindowRect, IsWindowVisible,
    PeekMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, GWL_EXSTYLE,
    GWL_STYLE, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE, MSG, PM_REMOVE, SET_WINDOW_POS_FLAGS,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_DWMCOMPOSITIONCHANGED,
    WM_DROPFILES, WM_ERASEBKGND, WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_NCCALCSIZE, WM_NCHITTEST, WM_QUIT, WM_SIZE, WM_WINDOWPOSCHANGING,
    WNDCLASSEXW, WS_CAPTION,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_MAXIMIZE, WS_POPUP,
};

use super::dpi::Dpi;
use super::{tray, Event};

/// `WM_MOUSELEAVE` is published in the user32 headers but the generated bindings
/// only expose it behind `Win32_UI_Controls`, which this crate does not enable.
/// The value is a fixed part of the Win32 ABI.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Window class name, registered once per process.
const CLASS_NAME: &str = "ArcOverlayWindow";

/// Hotkey id for Ctrl+Shift+A. `WM_HOTKEY` carries this in `wParam`.
const HOTKEY_TOGGLE: i32 = 0xA1;

/// Logical pixels of breathing room kept across the monitor, so an expanded
/// panel never touches the bezel (docs/05 §6: `clamp(…, 0, monitor_w − 60)`).
pub const EDGE_MARGIN_PX: f32 = 60.0;

/// Fullscreen detection cadence. 1 Hz is plenty for a top-edge ornament: a late
/// auto-hide is invisible, an early one is a visible flash.
const FULLSCREEN_POLL: Duration = Duration::from_millis(1_000);

/// UTF-16, NUL-terminated. Win32 copies class names at registration time, so the
/// buffer only has to outlive the call that uses it.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Process-wide setup performed exactly once, before any window exists: DPI
/// awareness, then the window class.
///
/// `HINSTANCE` is a raw pointer and therefore not `Sync`; the address is stored
/// as a `usize` so the `OnceLock` can live in a `static`.
struct Global {
    instance_addr: usize,
}

static GLOBAL: OnceLock<Option<Global>> = OnceLock::new();

/// Message Explorer broadcasts after restarting (T14). Registered lazily; the id
/// is all we need in order to recognise it.
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

fn global() -> Option<&'static Global> {
    GLOBAL
        .get_or_init(|| {
            // Must happen before the first HWND exists: per-monitor-v2 is what
            // makes the island land on the correct monitor at mixed DPI.
            if let Err(err) =
                unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            {
                // Pre-1607 Windows has no v2 context. Only log: the island still
                // works, it just scales per-system instead of per-monitor.
                log::warn!(
                    "arc: PER_MONITOR_AWARE_V2 unavailable ({err:?}); \
                     continuing with process-wide DPI awareness"
                );
            }

            // SAFETY: a null module name asks for the current executable image.
            let module = unsafe { GetModuleHandleW(None) }.ok()?;
            let instance = HINSTANCE::from(module);

            let class_name = wide(CLASS_NAME);
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                // The icon comes from the exe's first resource (`app.rc`):
                // WM_GETICON / Alt-Tab / taskbar previews all pick it up.
                // A missing resource leaves these null — never fatal.
                hIcon: tray::load_icon().unwrap_or_default(),
                hCursor: Default::default(),
                hbrBackground: Default::default(),
                lpszMenuName: Default::default(),
                lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
                hInstance: instance,
                hIconSm: tray::load_icon().unwrap_or_default(),
            };

            let atom = unsafe { RegisterClassExW(&class) };
            if atom == 0 {
                // SAFETY: pure query for diagnostics.
                let err = unsafe { GetLastError() };
                log::error!("arc: RegisterClassExW failed for {CLASS_NAME} (err {err:?})");
                return None;
            }
            log::debug!("arc: window class {CLASS_NAME} registered (atom {atom})");

            Some(Global {
                instance_addr: instance.0 as usize,
            })
        })
        .as_ref()
}

/// `WM_WINDOWPOSCHANGING` carries a `WINDOWPOS*`; mirror the Win32 layout so
/// placement can be corrected *before* the move is applied.
#[repr(C)]
struct WindowPos {
    hwnd: HWND,
    hwnd_insert_after: HWND,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    flags: SET_WINDOW_POS_FLAGS,
}

/// Everything the window procedure needs, reachable through `GWLP_USERDATA`.
///
/// The `Overlay` owns this in a `Box` and passes the pointer to the window
/// procedure, which is how a plain `extern "system"` callback reaches the event
/// queue without a global.
struct State {
    events: Vec<Event>,
    click_through: ClickThrough,
}

/// Whether the island swallows mouse input.
///
/// `Yes` is the right initial state: the island starts collapsed and must not
/// eat clicks. `No` is used while the panel is open so it can be interacted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClickThrough {
    /// Clicks pass through to whatever is underneath (the collapsed island).
    #[default]
    Yes,
    /// The island takes input (the open panel).
    No,
}

impl Default for State {
    fn default() -> Self {
        Self {
            events: Vec::with_capacity(16),
            click_through: ClickThrough::Yes,
        }
    }
}

/// The classic HWND→state channel. Only ever called with our own HWND, and only
/// while the owning `Overlay` is alive (the slot is cleared in `Drop`).
unsafe fn userdata(hwnd: HWND) -> Option<&'static mut State> {
    if hwnd.0.is_null() {
        return None;
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if ptr.is_null() {
        None
    } else {
        Some(&mut *ptr)
    }
}

fn push_event(hwnd: HWND, event: Event) {
    if hwnd.0.is_null() {
        return;
    }
    // SAFETY: single-threaded (the app pumps messages on the owning thread), and
    // the pointer stays valid until `Overlay::drop` clears it.
    if let Some(state) = unsafe { userdata(hwnd) } {
        state.events.push(event);
    }
}

fn click_through_active(hwnd: HWND) -> bool {
    // SAFETY: see `userdata`. Defaults to pass-through when the slot is empty,
    // which is the safe direction (never swallow a click we did not intend to).
    unsafe { userdata(hwnd) }
        .map(|s| s.click_through == ClickThrough::Yes)
        .unwrap_or(true)
}

/// `LPARAM`-packed client coordinates are a signed 16-bit pair.
fn lparam_xy(lparam: LPARAM) -> (i16, i16) {
    let v = lparam.0 as u32;
    (
        (v & 0xffff) as u16 as i16,
        ((v >> 16) & 0xffff) as u16 as i16,
    )
}

/// Pure: clamp a *physical* island width to `monitor_w − EDGE_MARGIN_PX`
/// (docs/05 §6). Never returns <= 0, whatever nonsense it is handed.
fn clamp_physical_width(width: i32, monitor_w: i32) -> i32 {
    let max_w = (monitor_w as f32 - EDGE_MARGIN_PX).max(1.0);
    (width as f32).clamp(1.0, max_w) as i32
}

/// Pure: physical origin for an island of the given *physical* width on a monitor
/// work area. Flush against `work.top`, horizontally centred (T5: the zero top
/// gap is what stops the island reading as "another floating widget").
fn placement(work: &RECT, physical_w: i32) -> POINT {
    let monitor_w = work.right - work.left;
    let w = clamp_physical_width(physical_w, monitor_w);
    POINT {
        x: work.left + (monitor_w - w) / 2,
        y: work.top,
    }
}

/// Pure: does `fg` cover `monitor` well enough to count as fullscreen?
///
/// Rejects the three false-positive classes that matter: empty/minimised rects,
/// ordinary maximized windows (work-area sized, not monitor sized), and windows
/// that are monitor-sized but offset away from the monitor origin.
fn covers_monitor(fg: &RECT, monitor: &RECT) -> bool {
    let w = fg.right - fg.left;
    let h = fg.bottom - fg.top;
    if w <= 0 || h <= 0 {
        return false;
    }
    // A couple of physical pixels of slop for border rounding and DPI rounding.
    const SLOP: i32 = 2;
    w >= monitor.right - monitor.left - SLOP
        && h >= monitor.bottom - monitor.top - SLOP
        && fg.left <= monitor.left + SLOP
        && fg.top <= monitor.top + SLOP
}

/// True when a window's style means it has given up its title bar — the
/// discriminator between a real fullscreen app and a merely maximized one.
///
/// A maximized window keeps `WS_CAPTION` and only grows to the *work* area; a
/// fullscreen app (game, F11 browser, video player) strips the caption and covers
/// the whole *monitor*. Rect comparison alone is not enough: on a monitor with no
/// taskbar reserved — a secondary display, or a primary with the taskbar
/// auto-hidden — the work area equals the monitor, so every maximized window would
/// otherwise read as fullscreen and hide the island permanently.
fn strips_chrome(style: i32) -> bool {
    // Frameless-by-design windows (Electron, Qt, custom-chrome apps) also lack
    // WS_CAPTION, and when maximized on a taskbar-less work area they would read
    // as fullscreen permanently. A real fullscreen app (game, F11 video) is
    // never `WS_MAXIMIZE`d — it positions itself over the monitor.
    style & (WS_CAPTION.0 as i32) == 0 && style & (WS_MAXIMIZE.0 as i32) == 0
}

/// Pure: hotkey modifiers for Ctrl+Shift+A. `MOD_NOREPEAT` keeps a held key from
/// toggling the island once per autorepeat (T12).
fn hotkey_modifiers() -> HOT_KEY_MODIFIERS {
    MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT
}

fn work_area(hwnd: HWND) -> RECT {
    // SAFETY: read-only query on a valid HWND.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    monitor_work_area(monitor)
}

fn monitor_work_area(monitor: HMONITOR) -> RECT {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT::default(),
        rcWork: RECT::default(),
        dwFlags: 0,
    };
    // SAFETY: `mi` is a correctly sized, fully initialised MONITORINFO.
    if unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool() {
        mi.rcWork
    } else {
        // Defensive fallback: a failed query must not produce garbage geometry.
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // T2: a click on the island must never take activation away from the app
        // the user is actually working in. Belt-and-braces with WS_EX_NOACTIVATE.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),

        // Claim the whole window rect as client area: no caption, no border, so
        // the island can sit flush at y=0 with no shadowless-contact gap (T5).
        // wParam == 0 means "no size change requested" — then defer.
        WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),

        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }

        // Files dropped from Explorer. The list is queried into a buffer
        // of the size Shell reports, then the drop handle is freed.
        WM_DROPFILES => {
            let hdrop = HDROP(lparam.0 as *mut core::ffi::c_void);
            let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                // Query once for the length, then once into a buffer.
                let n = unsafe { DragQueryFileW(hdrop, i, None) };
                let mut buf = vec![0u16; n as usize + 1];
                let got = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
                if got > 0 {
                    buf.truncate(got as usize);
                    paths.push(String::from_utf16_lossy(&buf));
                }
            }
            unsafe { DragFinish(hdrop) };
            push_event(hwnd, Event::FilesDropped { paths });
            LRESULT(0)
        }

        // The DirectComposition surface owns every pixel; suppress the default
        // erase so there is no white flash before the renderer's first Present.
        WM_ERASEBKGND => LRESULT(1),

        WM_HOTKEY => {
            if wparam.0 == HOTKEY_TOGGLE as usize {
                push_event(hwnd, Event::ToggleIsland);
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }

        // Printable keypresses. The island is a click-through overlay,
        // so these only fire while the panel is interactive (not
        // click-through); routed to the terminal grid in `app`.
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wparam.0 as u32) {
                push_event(hwnd, Event::Key { ch });
            }
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lparam);
            push_event(
                hwnd,
                Event::CursorMoved {
                    x: x as f32,
                    y: y as f32,
                },
            );
            LRESULT(0)
        }

        WM_MOUSELEAVE => {
            push_event(hwnd, Event::CursorLeft);
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            // Capture so the matching WM_LBUTTONUP still lands on us if the
            // cursor drifts outside during the press.
            SetCapture(hwnd);
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            // No state to recover: if the capture was already gone, the click
            // below is still delivered to us.
            let _ = unsafe { ReleaseCapture() };
            let (x, y) = lparam_xy(lparam);
            push_event(
                hwnd,
                Event::LeftClick {
                    x: x as f32,
                    y: y as f32,
                },
            );
            LRESULT(0)
        }

        WM_SIZE => {
            push_event(
                hwnd,
                Event::Resized {
                    width: (lparam.0 & 0xffff).max(0) as u32,
                    height: ((lparam.0 >> 16) & 0xffff).max(0) as u32,
                },
            );
            LRESULT(0)
        }

        WM_DPICHANGED => {
            // The suggested rect is a hint; we own placement. Take the position,
            // then let `Overlay::apply_placement` re-resolve DPI and re-clamp on
            // the next frame, which is also when the renderer is resized.
            let suggested = &*(lparam.0 as *const RECT);
            let width = (suggested.right - suggested.left).max(0) as u32;
            let height = (suggested.bottom - suggested.top).max(0) as u32;
            // SAFETY: our own HWND; no z-order or activation change.
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                width.max(1) as i32,
                height.max(1) as i32,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            );
            push_event(hwnd, Event::Resized { width, height });
            LRESULT(0)
        }

        WM_DISPLAYCHANGE => {
            // Resolution or monitor topology changed. The renderer must rebuild
            // its swapchain; `apply_placement` re-reads monitor + DPI.
            push_event(hwnd, Event::Redraw);
            LRESULT(0)
        }

        WM_DWMCOMPOSITIONCHANGED => {
            // T14: DWM restarted. The renderer has to rebuild its DComp device;
            // from here we only ask for a redraw and never touch the stale one.
            log::info!("arc: WM_DWMCOMPOSITIONCHANGED — renderer should rebuild");
            push_event(hwnd, Event::Redraw);
            LRESULT(0)
        }

        WM_NCHITTEST => {
            // Click-through is a *policy* (docs/05 §2), not just a style bit.
            // HTTRANSPARENT keeps the collapsed island a true pass-through with
            // no dead zone, and leaves a hook for click-outside later.
            if click_through_active(hwnd) {
                LRESULT(HTTRANSPARENT as isize)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }

        WM_WINDOWPOSCHANGING => {
            // Re-pin to the top edge of the current monitor on *every* move, not
            // only from resize(), so the island stays flush when monitors are
            // re-arranged or the taskbar changes edge.
            let pos = &mut *(lparam.0 as *mut WindowPos);
            if !pos.flags.contains(SWP_NOMOVE) {
                let work = work_area(hwnd);
                let monitor_w = work.right - work.left;
                let width = clamp_physical_width(pos.cx, monitor_w);
                pos.x = work.left + (monitor_w - width) / 2;
                pos.y = work.top;
                pos.cx = width;
            }
            LRESULT(0)
        }

        // Tray callback: left click toggles, right click shows the menu. The
        // queue push happens here so `State` stays private to this module.
        msg if msg == tray::WM_TRAY => {
            if let Some(event) = tray::on_message(hwnd, lparam) {
                push_event(hwnd, event);
            }
            LRESULT(0)
        }

        _ => {
            let taskbar_created = *TASKBAR_CREATED.get_or_init(|| {
                let name = wide("TaskbarCreated");
                RegisterWindowMessageW(windows::core::PCWSTR(name.as_ptr()))
            });
            if taskbar_created != 0 && msg == taskbar_created {
                // T14: Explorer restarted. The island survives; repaint so shell
                // state and the swapchain agree again — and the tray icon comes
                // back, since Explorer rebuilt its own store from scratch.
                log::info!("arc: TaskbarCreated — shell restarted, requesting redraw");
                tray::refresh();
                push_event(hwnd, Event::Redraw);
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
    }
}

/// Owns the top-center overlay HWND.
pub struct Overlay {
    hwnd: HWND,
    dpi: Dpi,
    state: Box<State>,
    monitor: HMONITOR,
    logical_size: (f32, f32),
    fullscreen: bool,
    last_fullscreen_poll: Instant,
    hotkey_registered: bool,
}

impl Overlay {
    /// Create the window. Returns `None` if the HWND could not be created — it
    /// never panics and never yields a null handle.
    pub fn new() -> Option<Self> {
        Self::create()
    }

    fn create() -> Option<Self> {
        let g = global()?;
        let mut state = Box::<State>::default();
        let state_ptr: *mut State = &mut *state as *mut State;
        let class_name = wide(CLASS_NAME);

        let created = unsafe {
            CreateWindowExW(
                // TOOLWINDOW: no taskbar and no Alt-Tab entry. NOACTIVATE: never
                // steal focus. TOPMOST: stay above maximized apps. TRANSPARENT:
                // start click-through, matching the initial collapsed state.
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
                windows::core::PCWSTR(class_name.as_ptr()),
                windows::core::PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                // 1x1 rather than 0x0: a zero-sized popup never receives a
                // meaningful WM_SIZE and gives DirectComposition no target.
                1,
                1,
                None,
                None,
                Some(HINSTANCE(g.instance_addr as *mut std::ffi::c_void)),
                None,
            )
        };

        let hwnd = match created {
            Ok(h) if !h.0.is_null() => h,
            Ok(_) => {
                log::error!("arc: CreateWindowExW returned a null HWND");
                return None;
            }
            Err(err) => {
                log::error!("arc: CreateWindowExW failed: {err:?}");
                return None;
            }
        };

        // SAFETY: publishes the Box's stable address for the lifetime of this
        // Overlay; cleared again in `Drop` before the Box is freed.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };

        // Accept file drops; they arrive as `WM_DROPFILES`. Registered after
        // the HWND exists, so the message can never arrive before the handler.
        // SAFETY: valid HWND owned by this process.
        unsafe { DragAcceptFiles(hwnd, true) };

        let mut overlay = Self {
            hwnd,
            dpi: Self::query_dpi(hwnd),
            state,
            // SAFETY: read-only query on a valid HWND.
            monitor: unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) },
            logical_size: (0.0, 0.0),
            fullscreen: false,
            last_fullscreen_poll: Instant::now(),
            hotkey_registered: false,
        };

        overlay.register_hotkey();
        // Tray entry: a second, conventional way to reach the island. Failure
        // is logged inside and never blocks startup (like a lost hotkey).
        if tray::install(hwnd) {
            log::debug!("arc: tray icon installed");
        }
        // Place once immediately so the island is never briefly visible at 0,0.
        overlay.apply_placement(185.0, 32.0);

        Some(overlay)
    }

    fn query_dpi(hwnd: HWND) -> Dpi {
        // SAFETY: pure query. A 0 return (pre-8.1) falls back to 96 DPI = 100%.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            Dpi::default()
        } else {
            Dpi::from_dpi(dpi)
        }
    }

    fn register_hotkey(&mut self) {
        // T12: RegisterHotKey fails when another app already owns the combo.
        // That must degrade to "no hotkey", never to "the app does not start".
        // SAFETY: registering against our own HWND, once per process lifetime.
        let registered = unsafe {
            RegisterHotKey(
                Some(self.hwnd),
                HOTKEY_TOGGLE,
                hotkey_modifiers(),
                VK_A.0 as u32,
            )
        };
        match registered {
            Ok(()) => self.hotkey_registered = true,
            Err(err) => {
                self.hotkey_registered = false;
                log::error!(
                    "arc: could not register Ctrl+Shift+A ({err:?}) — another app owns the \
                     combination; the island will respond to mouse input only"
                );
            }
        }
    }

    /// DPI of the monitor the island currently lives on.
    pub fn dpi(&self) -> Dpi {
        self.dpi
    }

    /// Set the island's logical size and reposition it flush to the top edge.
    pub fn resize(&mut self, logical_w: f32, logical_h: f32) {
        self.logical_size = (logical_w, logical_h);
        self.apply_placement(logical_w, logical_h);
    }

    fn apply_placement(&mut self, logical_w: f32, logical_h: f32) {
        // Re-resolve monitor and DPI on every placement: WM_DISPLAYCHANGE, a
        // monitor re-arrangement, or a DPI change can all move the island
        // without `app` asking for a resize.
        // SAFETY: read-only queries on a valid HWND.
        unsafe {
            self.monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
        }
        self.dpi = Self::query_dpi(self.hwnd);

        let work = monitor_work_area(self.monitor);
        let physical_w = self.dpi.snap(logical_w.max(0.0)) as i32;
        let physical_h = self.dpi.snap(logical_h.max(0.0)) as i32;
        let pt = placement(&work, physical_w);
        let width = clamp_physical_width(physical_w, work.right - work.left);

        // NOZORDER keeps WS_EX_TOPMOST meaningful; NOACTIVATE is redundant with
        // WS_EX_NOACTIVATE but free insurance. Deliberately *without*
        // SWP_NOSENDCHANGING so WM_WINDOWPOSCHANGING still enforces the pin.
        // SAFETY: our own HWND.
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                pt.x,
                pt.y,
                width.max(1),
                physical_h.max(1),
                SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
            )
        };
    }

    /// Toggle whether the island swallows mouse input. Collapsed ⇒ pass-through,
    /// expanded ⇒ the panel takes input.
    /// Take keyboard focus for the island, so `WM_CHAR` / `WM_KEYDOWN` are
    /// delivered here instead of to the app underneath.
    ///
    /// The island is `WS_EX_NOACTIVATE` on purpose — hovering it must never
    /// steal focus from the user's real work. Typing into the Terminal tab is
    /// the one case where focus is wanted, so it is explicit: the app calls
    /// this only while that tab is open, and [`Self::release_focus`] gives it
    /// back.
    pub fn take_focus(&mut self) {
        // SAFETY: our own HWND; SetFocus on a NOACTIVATE window is legal and
        // does not raise it to the foreground.
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    /// Drop keyboard focus back to the foreground app.
    pub fn release_focus(&mut self) {
        // SAFETY: `None` drops focus to the desktop rather than to another
        // window, which is the documented way to leave nothing focused.
        unsafe {
            let _ = SetFocus(None);
        }
    }

    pub fn set_click_through(&mut self, mode: ClickThrough) {
        if self.state.click_through == mode {
            return;
        }
        self.state.click_through = mode;

        // SAFETY: pure ex-style bit update on our own window.
        let current = unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) } as u32;
        let mut next = current;
        if mode == ClickThrough::Yes {
            next |= WS_EX_TRANSPARENT.0;
        } else {
            next &= !WS_EX_TRANSPARENT.0;
        }
        if next != current {
            // SAFETY: our own HWND.
            unsafe { SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, next as isize) };
        }
    }

    /// Drain queued window messages into platform events. Non-blocking.
    pub fn pump_events(&mut self) -> Vec<Event> {
        self.pump_messages();
        self.poll_fullscreen();
        std::mem::take(&mut self.state.events)
    }

    /// Is the cursor inside the window rect right now? Polled, not message-
    /// driven: while collapsed the window is `WS_EX_TRANSPARENT`, so it never
    /// receives `WM_MOUSEMOVE` and hover-dwell has nothing to listen to.
    pub fn pointer_over(&self) -> bool {
        let mut p = POINT::default();
        // SAFETY: `p` is a valid POINT for the call's duration.
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return false;
        }
        let Some(rect) = self.client_rect_physical() else {
            return false;
        };
        p.x >= rect.left && p.x < rect.right && p.y >= rect.top && p.y < rect.bottom
    }

    /// Window rect in physical screen pixels (not DPI-virtualized).
    fn client_rect_physical(&self) -> Option<RECT> {
        let mut r = RECT::default();
        // SAFETY: `r` is a valid RECT for the call's duration; a false return
        // just means no rect, which the caller treats as "not over".
        if unsafe { GetWindowRect(self.hwnd, &mut r) }.is_err() {
            return None;
        }
        Some(r)
    }

    fn pump_messages(&mut self) {
        let mut msg = MSG::default();
        loop {
            // SAFETY: `msg` is a valid, correctly sized MSG for the call's
            // duration. Peek (not Get) so the app loop keeps its own cadence and
            // never blocks on this call.
            let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
            if !got.as_bool() {
                break;
            }
            if msg.message == WM_QUIT {
                self.state.events.push(Event::Quit);
                continue;
            }
            if msg.message == WM_MOUSEMOVE {
                // WM_MOUSELEAVE only ever gets generated because of this request.
                self.request_leave_tracking();
            }
            // SAFETY: standard message pump. DispatchMessageW only dispatches to
            // a window that owns `msg.hwnd`.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn request_leave_tracking(&self) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: single owner of `tme`; failure only costs the leave event.
        let _ = unsafe { TrackMouseEvent(&mut tme) };
    }

    /// 1 Hz foreground probe (docs/05 §2): a fullscreen app on *our* monitor
    /// hides the island.
    ///
    /// Ignores our own HWND, invisible windows, and merely maximized windows. A
    /// null foreground window (session lock, UAC prompt) counts as "not
    /// fullscreen" so the island can never get stuck hidden with no way back.
    fn poll_fullscreen(&mut self) {
        if self.last_fullscreen_poll.elapsed() < FULLSCREEN_POLL {
            return;
        }
        self.last_fullscreen_poll = Instant::now();

        // SAFETY: read-only queries; every handle is null-checked first.
        let now_fullscreen = unsafe {
            let fg = GetForegroundWindow();
            if fg.0.is_null() || fg == self.hwnd || !IsWindowVisible(fg).as_bool() {
                false
            } else {
                let mut fg_rect = RECT::default();
                if GetWindowRect(fg, &mut fg_rect).is_err() {
                    false
                } else {
                    let fg_monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
                    let mut mi = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        rcMonitor: RECT::default(),
                        rcWork: RECT::default(),
                        dwFlags: 0,
                    };
                    if !GetMonitorInfoW(fg_monitor, &mut mi).as_bool() {
                        false
                    } else if fg_monitor.0 != self.monitor.0 {
                        // Fullscreen somewhere else on the desktop: irrelevant.
                        false
                    } else {
                        // Both conditions: monitor-sized *and* chrome stripped.
                        let style = GetWindowLongW(fg, GWL_STYLE);
                        covers_monitor(&fg_rect, &mi.rcMonitor) && strips_chrome(style)
                    }
                }
            }
        };

        if now_fullscreen != self.fullscreen {
            self.fullscreen = now_fullscreen;
            self.state.events.push(if now_fullscreen {
                Event::FullscreenEnter
            } else {
                Event::FullscreenExit
            });
        }
    }

    /// The live overlay HWND. Never dangling and never null: [`Overlay::new`]
    /// returned `None` rather than constructing a handle-less overlay.
    pub fn hwnd(&self) -> NonNull<std::ffi::c_void> {
        // SAFETY: `create` rejects a null HWND, so this is always non-null.
        unsafe { NonNull::new_unchecked(self.hwnd.0) }
    }

    pub fn show(&self) {
        // SW_SHOWNOACTIVATE, never SW_SHOW: showing must not steal focus.
        // SAFETY: our own HWND.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        // SAFETY: our own HWND.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Logical size last requested through [`Overlay::resize`].
    /// Last logical size the app asked for — feeds the Phase-2 debug overlay.
    #[allow(dead_code)]
    pub fn logical_size(&self) -> (f32, f32) {
        self.logical_size
    }

    /// Whether the Ctrl+Shift+A hotkey actually registered. `false` means
    /// another app owns the combination (T12).
    pub fn hotkey_ok(&self) -> bool {
        self.hotkey_registered
    }

    /// Whether a fullscreen app currently owns our monitor.
    /// Current fullscreen-suppression state — feeds the Phase-2 debug overlay.
    #[allow(dead_code)]
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Before the HWND dies: the shell needs a live window to accept NIM_DELETE.
        tray::remove();
        if self.hotkey_registered {
            // SAFETY: unregistering a hotkey we registered against our own HWND.
            let _ = unsafe { UnregisterHotKey(Some(self.hwnd), HOTKEY_TOGGLE) };
        }
        // Clear the userdata slot *before* the Box dies so a message already in
        // flight cannot observe freed memory.
        // SAFETY: our own HWND.
        unsafe {
            let _ = SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn clamp_honours_the_sixty_pixel_bezel_margin() {
        // 1920 wide monitor: legal maximum is 1920 - 60 = 1860.
        assert_eq!(clamp_physical_width(640, 1920), 640);
        assert_eq!(clamp_physical_width(1900, 1920), 1860);
    }

    #[test]
    fn clamp_never_returns_zero_or_negative() {
        assert_eq!(clamp_physical_width(0, 1920), 1);
        assert_eq!(clamp_physical_width(-500, 1920), 1);
        // A monitor narrower than the margin still yields a usable window.
        assert_eq!(clamp_physical_width(100, 40), 1);
    }

    #[test]
    fn placement_is_flush_to_the_top_edge_and_centred() {
        let work = r(0, 0, 1920, 1040); // taskbar at the bottom
        let p = placement(&work, 640);
        assert_eq!(p.y, work.top, "must be flush with the top edge (T5)");
        assert_eq!(p.x, (1920 - 640) / 2);
    }

    #[test]
    fn placement_respects_a_non_zero_origin_monitor() {
        // Second monitor to the right of the primary.
        let work = r(1920, 0, 3840, 1080);
        let p = placement(&work, 400);
        assert_eq!(p.y, 0);
        assert_eq!(p.x, 1920 + (1920 - 400) / 2);
    }

    #[test]
    fn placement_clamps_before_centering() {
        // A 5000px island on a 1920px monitor must not push x off-screen.
        let work = r(0, 0, 1920, 1080);
        let p = placement(&work, 5000);
        assert_eq!(p.x, (1920 - 1860) / 2);
        assert!(p.x >= 0);
    }

    #[test]
    fn placement_respects_a_top_taskbar_work_area() {
        // Taskbar at the top: flush to the work-area top, which sits just below
        // the taskbar rather than at the monitor's y=0.
        let work = r(0, 40, 1920, 1080);
        assert_eq!(placement(&work, 300).y, 40);
    }

    #[test]
    fn fullscreen_detection_matches_monitor_covering_windows() {
        let mon = r(0, 0, 1920, 1080);
        assert!(covers_monitor(&r(0, 0, 1920, 1080), &mon));
        // A pixel of border slop still counts as fullscreen.
        assert!(covers_monitor(&r(0, 0, 1921, 1081), &mon));
    }

    #[test]
    fn fullscreen_detection_ignores_ordinary_windows() {
        let mon = r(0, 0, 1920, 1080);
        // Maximized, not fullscreen: covers the work area, not the monitor.
        assert!(!covers_monitor(&r(0, 0, 1920, 1040), &mon));
        // An ordinary browser window.
        assert!(!covers_monitor(&r(100, 100, 900, 700), &mon));
        // Minimised / zero-size rect.
        assert!(!covers_monitor(&r(0, 0, 0, 0), &mon));
    }

    #[test]
    fn fullscreen_detection_is_scoped_to_one_monitor() {
        let primary = r(0, 0, 1920, 1080);
        let secondary = r(1920, 0, 3840, 1080);
        // Fullscreen on the other monitor must not hide our island ...
        assert!(!covers_monitor(&secondary, &primary));
        // ... but it does count as fullscreen for that monitor.
        assert!(covers_monitor(&secondary, &secondary));
    }

    #[test]
    fn fullscreen_detection_ignores_windows_offset_off_the_top_left() {
        let mon = r(0, 0, 1920, 1080);
        // Monitor-sized but floating below the origin: not covering it.
        assert!(!covers_monitor(&r(0, 40, 1920, 1120), &mon));
    }

    #[test]
    fn chrome_detection_separates_fullscreen_from_maximized() {
        // A maximized window keeps WS_CAPTION ...
        assert!(!strips_chrome(WS_CAPTION.0 as i32));
        assert!(!strips_chrome((WS_CAPTION.0 | 0x0004_0000) as i32));
        // ... a fullscreen one has dropped it (borderless popup).
        assert!(strips_chrome(WS_POPUP.0 as i32));
        assert!(strips_chrome(0x8000_0000u32 as i32));
    }

    #[test]
    fn monitor_sized_maximized_window_is_not_fullscreen() {
        // The regression: no taskbar reserved, so work area == monitor rect and a
        // maximized window covers it exactly. Rect alone said "fullscreen"; the
        // caption check must veto it.
        let mon = r(0, 0, 1920, 1080);
        let maximized = r(0, 0, 1920, 1080);
        assert!(covers_monitor(&maximized, &mon));
        assert!(
            !(covers_monitor(&maximized, &mon) && strips_chrome(WS_CAPTION.0 as i32)),
            "a captioned window must not hide the island"
        );
        // A genuinely fullscreen window still passes both.
        assert!(covers_monitor(&maximized, &mon) && strips_chrome(WS_POPUP.0 as i32));
    }

    #[test]
    fn hotkey_is_ctrl_shift_a_with_autorepeat_suppressed() {
        assert_eq!(
            hotkey_modifiers().0,
            MOD_CONTROL.0 | MOD_SHIFT.0 | MOD_NOREPEAT.0
        );
        assert_eq!(VK_A.0 as u32, 0x41);
    }

    #[test]
    fn lparam_decodes_signed_client_coords() {
        // Negative coordinates arrive as unsigned 16-bit two's complement.
        assert_eq!(lparam_xy(LPARAM(-1)), (-1i16, -1i16));
        // High word 7 (y), low word 3 (x) — the Win32 packing order.
        assert_eq!(
            lparam_xy(LPARAM((((7u32) << 16) | 3u32) as isize)),
            (3i16, 7i16)
        );
    }

    #[test]
    fn class_name_wide_buffer_is_nul_terminated() {
        let buf = wide(CLASS_NAME);
        assert_eq!(*buf.last().unwrap(), 0);
        assert_eq!(buf.len(), CLASS_NAME.encode_utf16().count() + 1);
    }
}
