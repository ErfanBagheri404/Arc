//! Overlay window: HWND ownership, placement, DPI, click-through policy.
//!
//! The island is a borderless top-center `WS_POPUP` that must never appear in a
//! taskbar or Alt-Tab list and must never steal focus from the foreground app.
//! Everything that enforces that lives here; `app` only sees [`Event`]s.
//!
//! Invariants this module guarantees:
//! - The window is a fixed max frame; `set_body` clips the *window region* to
//!   the live body via `SetWindowRgn`, so clicks, hover and `WM_MOUSEMOVE`
//!   outside that rect are routed by Windows itself to whatever is below —
//!   exactly what the icons and tray items that live near the notch need.
//!   `WM_NCHITTEST` (`HTCLIENT` over the body, `HTTRANSPARENT` elsewhere) and
//!   the hover dwell stay bounded to the same rect as a second line of defence
//!   (T2 in docs/07-RISKS.md: never a dead zone, never a focus steal).
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
    CreateRectRgn, GetMonitorInfoW, MonitorFromWindow, SetWindowRgn, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, UnregisterHotKey,
    HOT_KEY_MODIFIERS, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, TME_LEAVE, TRACKMOUSEEVENT, VK_A,
    VK_Z,
};
use windows::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowLongW, GetWindowRect, IsWindowVisible,
    PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetWindowLongPtrW,
    GetClassNameW,
    SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, GWL_STYLE,
    HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE, MSG, PM_REMOVE, SET_WINDOW_POS_FLAGS, WM_APP,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_HIDE,
    MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT, SW_SHOWNOACTIVATE, WM_CHAR, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_DROPFILES,
    WM_DWMCOMPOSITIONCHANGED, WM_ERASEBKGND,    WM_HOTKEY, WM_INPUT, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCALCSIZE, WM_NCHITTEST, WM_QUIT, WM_SIZE,
    WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_MAXIMIZE, WS_POPUP,
};

/// TEMP hover-diag: WM_MOUSEMOVE hits on this thread's window.
pub static DBG_MOUSEMOVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// TEMP hover-diag: WM_MOUSELEAVE hits.
pub static DBG_MOUSELEAVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// TEMP hover-diag: WM_INPUT (raw mouse) hits.
pub static DBG_RAWINPUT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// TEMP hover-diag: WM_NCHITTEST hits.
pub static DBG_HITTEST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// TEMP hover-diag: cursor x/y packed (low16=x, high16=y).
pub static DBG_HOVER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// TEMP hover-diag: body left (low16) / right (high16).
pub static DBG_HOVER_R: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// TEMP hover-diag: body bottom edge.
pub static DBG_HOVER_B: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

use super::dpi::Dpi;
use super::{tray, Event};

/// `WM_MOUSELEAVE` is published in the user32 headers but the generated bindings
/// only expose it behind `Win32_UI_Controls`, which this crate does not enable.
/// The value is a fixed part of the Win32 ABI.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Window class name, registered once per process.
const CLASS_NAME: &str = "ArcOverlayWindow";

/// Posted by the `WM_INPUT` handler when a mouse move lands near the live
/// body: raw input *queues* fine but does not break `MsgWaitForMultipleObjectsEx`
/// (injected moves sat parked until the poll timeout on Win11), so the relay
/// post is what actually wakes the parked loop. `WM_APP` window range is
/// reserved for app-internal messages; the skew keeps it from tripping over
/// any future `WM_APP` user.
const WM_MOUSE_WAKE: u32 = WM_APP + 0x1A2C;
/// How far (physical px) around the live body a mouse move still counts as
/// hover-relevant for the wake relay. Generous: there is no harm in waking
/// the loop slightly early — `pointer_over` is the decider.
const WAKE_MARGIN_PX: i32 = 48;

/// Hotkey id for Ctrl+Shift+A. `WM_HOTKEY` carries this in `wParam`.
const HOTKEY_TOGGLE: i32 = 0xA1;
/// Hotkey id for Ctrl+Shift+Z: hide the island, or force it visible everywhere.
const HOTKEY_VISIBILITY: i32 = 0xA2;

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
    /// Left inset of the live body inside the fixed frame, in client physical
    /// px. Used by `WM_NCHITTEST` so only the visible island is clickable —
    /// the transparent ear margins pass through to the desktop.
    body_inset: i32,
    /// Live body width in client physical px (see `body_inset`).
    body_width: i32,
    /// Live body height in client physical px (see `body_inset`). Without this
    /// the hit-test would accept the whole 200px frame column, so the
    /// transparent strip below the pill would silently eat desktop clicks.
    body_height: i32,
    /// Live body top offset in client physical px (0 unless the peek pill is
    /// detached from the screen top). Keeps the hit-test / region aligned.
    body_offset_y: i32,
    /// Last region rect passed to `SetWindowRgn`; skips no-op OS calls.
    applied_rgn: (i32, i32, i32, i32),
}

impl Default for State {
    fn default() -> Self {
        Self {
            events: Vec::with_capacity(16),
            body_inset: 0,
            body_width: 0,
            body_height: 0,
            body_offset_y: 0,
            applied_rgn: (0, 0, 0, 0),
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

fn click_body_inset(hwnd: HWND) -> i32 {
    unsafe { userdata(hwnd) }.map(|s| s.body_inset).unwrap_or(0)
}

fn click_body_height(hwnd: HWND) -> i32 {
    unsafe { userdata(hwnd) }
        .map(|s| s.body_height)
        .unwrap_or(0)
}

fn click_body_offset_y(hwnd: HWND) -> i32 {
    unsafe { userdata(hwnd) }
        .map(|s| s.body_offset_y)
        .unwrap_or(0)
}

fn click_body_width(hwnd: HWND) -> i32 {
    // Mirrors `pointer_over`: the live body width, centred in the fixed frame.
    unsafe { userdata(hwnd) }.map(|s| s.body_width).unwrap_or(0)
}

/// Physical client origin of `hwnd`. The frame is borderless, so this is also
/// the window origin; a failed query degrades to pass-through (0,0 keeps the
/// body inset out of reach on absurd coordinates).
fn client_origin(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    // SAFETY: read-only query on a valid HWND.
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return (0, 0);
    }
    (r.left, r.top)
}

/// Pure hit-test: is client point (`x`,`y`) inside the live body rect? The
/// height bound matters: without it the transparent strip *below* the pill
/// would swallow desktop clicks (the fixed frame is far taller than the pill).
fn hit_body(x: i32, y: i32, inset: i32, width: i32, height: i32, offset_y: i32) -> bool {
    x >= inset && x < inset + width && y >= offset_y && y < offset_y + height
}

/// Relay a mouse move as a wake post when the cursor is near the live body.
///
/// Raw input arrives even though the collapsed window is click-through, but a
/// queued `WM_INPUT` does not break `MsgWaitForMultipleObjectsEx` (measured on
/// Win11: injected moves sat parked until the poll timeout), so the reliable
/// wake is a posted message. This reads the same body rect `WM_NCHITTEST` uses
/// (via `state`'s live fields) expanded by `WAKE_MARGIN_PX`; moves farther out
/// are irrelevant to hover and get dropped, which keeps the relay from
/// flooding the queue during ordinary desktop mouse use. When the cursor is
/// *inside* the body the window is not click-through and regular
/// `WM_MOUSEMOVE`s flow anyway — the relay is for the approach/leave frames.
fn wake_if_near_body(hwnd: HWND) {
    let mut p = POINT::default();
    // SAFETY: `p` is a valid POINT for the call's duration.
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return;
    }
    let origin = client_origin(hwnd);
    let (x, y) = (p.x - origin.0, p.y - origin.1);
    if hit_body(
        x,
        y,
        click_body_inset(hwnd) - WAKE_MARGIN_PX,
        (click_body_width(hwnd) + 2 * WAKE_MARGIN_PX).max(1),
        (click_body_height(hwnd) + 2 * WAKE_MARGIN_PX).max(1),
        click_body_offset_y(hwnd) - WAKE_MARGIN_PX,
    ) {
        // SAFETY: our own HWND; a failed post only costs this wake.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_MOUSE_WAKE, WPARAM(0), LPARAM(0)) };
    }
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

/// True for the shell's desktop windows (`Progman`, `WorkerW`). The
/// desktop covers the whole monitor with its caption stripped — a
/// textbook fullscreen match — but it is where the user *goes* when
/// they show the desktop, not an app taking over the screen, so the
/// probe must never hide the island for it.
fn is_shell_desktop(class: &str) -> bool {
    matches!(class, "Progman" | "WorkerW")
}

/// The window class of `hwnd`, for the desktop-window check. Empty on
/// any failure — which can only mean "not a shell window", since those
/// always have a class.
fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: read-only query, `buf` is a valid u16 buffer.
    let got = unsafe { GetClassNameW(hwnd, &mut buf) };
    if got <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..got as usize])
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
            } else if wparam.0 == HOTKEY_VISIBILITY as usize {
                push_event(hwnd, Event::ToggleVisibility);
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

        WM_INPUT => {
            DBG_RAWINPUT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Raw input is delivered even though the window is click-through,
            // but a queued `WM_INPUT` does not break the `MsgWaitForMultiple-
            // ObjectsEx` the loop parks on. Relay it instead: a posted message
            // *does* break the wait, and posting one for every system-wide
            // move would be spammy — so only moves that land near the live
            // body get relayed (`wake_if_near_body`).
            wake_if_near_body(hwnd);
            // The docs require DefWindowProc for WM_INPUT so the raw buffer
            // is released.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_MOUSE_WAKE => {
            // The relay itself carries no data: being in the queue is the
            // signal, `pointer_over` on the next loop pass is the decider.
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            DBG_MOUSEMOVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            DBG_MOUSELEAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            // The fixed frame is larger than the visible body, so hit-testing
            // is done against the *live* body rect: the pill/panel is clickable
            // (HTCLIENT), the transparent ear margins pass through (HTTRANSPARENT)
            // so they never become a dead zone over the desktop.
            // LPARAM carries *screen* coords; the body inset is client-relative,
            // and the frame is borderless so client origin == window origin.
            let (sx, sy) = lparam_xy(lparam);
            let origin = client_origin(hwnd);
            let (x, y) = (sx as i32 - origin.0, sy as i32 - origin.1);
            let inside_body = hit_body(
                x,
                y,
                click_body_inset(hwnd),
                click_body_width(hwnd).max(1),
                click_body_height(hwnd).max(1),
                click_body_offset_y(hwnd),
            );
            if inside_body {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            } else {
                LRESULT(HTTRANSPARENT as isize)
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

/// Register the overlay HWND as a mouse raw-input sink (`RIDEV_INPUTSINK`).
///
/// Notifications only: raw input neither consumes nor reroutes anything, the
/// normal window-message flow is untouched. Every mouse movement
/// system-wide delivers a `WM_INPUT` here; because a queued `WM_INPUT` does
/// not break `wait_for_input`'s park (see `WM_INPUT` in `wnd_proc`), the
/// handler relays a post when the cursor is near the body. HID usages are
/// fixed values from the USB spec (Generic Desktop page, Mouse usage); they
/// are inlined because pulling in `Win32_Devices_HumanInterfaceDevice` for
/// two `u16`s would be overkill.
fn register_raw_mouse(hwnd: HWND) {
    let devices = [RAWINPUTDEVICE {
        usUsagePage: 0x0001, // HID_USAGE_PAGE_GENERIC
        usUsage: 0x0002,     // HID_USAGE_GENERIC_MOUSE
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    }];
    // SAFETY: `devices` is a valid array for the call's duration.
    if let Err(err) =
        unsafe { RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
    {
        log::error!("arc: RegisterRawInputDevices failed: {err:?}");
    }
}

/// Owns the top-center overlay HWND.
pub struct Overlay {
    hwnd: HWND,
    dpi: Dpi,
    /// Body width in physical px for hover hit-testing (frame is fixed max).
    hover_width_px: i32,
    /// Hover machine's stable rect (physical px): spring targets from the app
    /// state, refreshed every loop pass via `hover_box()`. `pointer_over`
    /// must decide against THIS, never against the live body — the live rect
    /// moves under the cursor mid-morph and makes Collapsed/Peek ping-pong.
    hover_target_px: (i32, i32, i32),
    state: Box<State>,
    monitor: HMONITOR,
    logical_size: (f32, f32),
    fullscreen: bool,
    last_fullscreen_poll: Instant,
    /// False while a morph is running: the window region is only re-cut at rest.
    settled: bool,
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
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
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

        // Raw mouse input sink: while collapsed the window answers
        // `WM_NCHITTEST` with `HTTRANSPARENT` off the body, so it never
        // receives `WM_MOUSEMOVE` and hover enter/leave had to wait out the
        // park's poll interval — the "hover doesn't go to pill / stays pill"
        // lag. The sink delivers `WM_INPUT` for every system-wide mouse move
        // and its handler relays a wake post when the cursor is near the
        // body; `pointer_over` (polled) stays the source of truth and
        // resolves enter/leave within the same frame. Failure is logged
        // inside and only degrades to the old poll cadence.
        register_raw_mouse(hwnd);

        let mut overlay = Self {
            hwnd,
            dpi: Self::query_dpi(hwnd),
            hover_width_px: 185,
            hover_target_px: (185, 32, 0),
            state,
            // SAFETY: read-only query on a valid HWND.
            monitor: unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) },
            logical_size: (0.0, 0.0),
            fullscreen: false,
            last_fullscreen_poll: Instant::now(),
            settled: true,
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
        // Ctrl+Shift+Z is independent: losing it must not cost the A toggle.
        // SAFETY: registering against our own HWND, once per process lifetime.
        let vis = unsafe {
            RegisterHotKey(
                Some(self.hwnd),
                HOTKEY_VISIBILITY,
                hotkey_modifiers(),
                VK_Z.0 as u32,
            )
        };
        if let Err(err) = vis {
            log::error!(
                "arc: could not register Ctrl+Shift+Z ({err:?}) — another app owns the \
                 combination; visibility stays on the fullscreen policy"
            );
        }
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
        // Cheap early-out: the app calls this every animating frame, but the
        // *frame* size only changes on mode/DPI/monitor changes. A SetWindowPos
        // per frame was the expand-animation jank.
        if self.logical_size == (logical_w, logical_h) {
            return;
        }
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

    /// Drain queued window messages into platform events. Non-blocking.
    pub fn pump_events(&mut self) -> Vec<Event> {
        self.pump_messages();
        self.poll_fullscreen();
        std::mem::take(&mut self.state.events)
    }

    /// Is the cursor inside the window rect right now? Polled, not message-
    /// driven: while collapsed the window answers `WM_NCHITTEST` with
    /// `HTTRANSPARENT` off the body, so it never receives `WM_MOUSEMOVE` and
    /// hover-dwell has nothing to listen to. The raw-input sink's wake relay
    /// only changes *when* this gets polled (on approach/leave it is right
    /// after the move), this check stays the single source of truth.
    ///
    /// `margin` grows the band by that many px on left/right/bottom — used
    /// twice with different values as the hover machine's hysteresis pair
    /// (`0` to enter, a few px to keep holding; see `IslandState::hover_step`).
    pub fn pointer_over(&self, margin: i32) -> bool {
        let mut p = POINT::default();
        // SAFETY: `p` is a valid POINT for the call's duration.
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return false;
        }
        let Some(rect) = self.client_rect_physical() else {
            return false;
        };
        // The window is a fixed max frame; the visible body is centred inside
        // it. Bound x *and* y by the hover TARGET rect, not the live body:
        // during a morph the live rect slides under a resting cursor and the
        // Collapsed↔Peek ladder flips back and forth (the pill landing stuck
        // at a mid width). Targets match the rect the machine is heading
        // for, so the decision is stable for the whole morph.
        let (tw, th, toy) = self.hover_target_px;
        let inset = (rect.right - rect.left - tw.max(1)) / 2;
        let (l, r) = (rect.left + inset, rect.right - inset);
        let bottom = rect.top + toy.max(0) + th.max(1);
        let inside = p.x >= l - margin && p.x < r + margin && p.y >= rect.top && p.y < bottom + margin;
        if std::env::var("ARC_DBG_LOOP").is_ok() {
            DBG_HOVER.store(
                ((p.x as usize) & 0xffff) | (((p.y as usize) & 0xffff) << 16),
                std::sync::atomic::Ordering::Relaxed,
            );
            DBG_HOVER_R.store(
                (l as usize & 0xffff) | ((r as usize & 0xffff) << 16),
                std::sync::atomic::Ordering::Relaxed,
            );
            DBG_HOVER_B.store(bottom as usize, std::sync::atomic::Ordering::Relaxed);
        }
        inside
    }

    /// Accept the hover machine's stable rect for this pass: spring targets
    /// in logical px, snapped to physical like `set_body` does.
    pub fn set_hover_target(&mut self, logical_w: f32, logical_h: f32, offset_y: f32) {
        let d = self.dpi();
        self.hover_target_px = (
            d.snap(logical_w).max(1) as i32,
            d.snap(logical_h).max(1) as i32,
            d.snap_offset(offset_y),
        );
    }

    /// Live body size in physical px, for `ARC_DBG_LOOP` frame logs.
    pub fn hover_width_px(&self) -> i32 {
        self.hover_width_px
    }
    pub fn body_height(&self) -> i32 {
        self.state.body_height
    }

    /// Park the loop until input arrives or `timeout` elapses.
    ///
    /// The idle loop used to `sleep(IDLE_POLL)`, which was exactly the hover
    /// latency: the cursor could sit on the pill for a fifth of a second
    /// before the app looked at `pointer_over` again, and the morph then
    /// integrated that whole gap in one clamped step — a 2 px pop followed by
    /// a slow crawl. Waiting on the input queue breaks the wait the moment a
    /// *posted* message arrives — notably the `WM_MOUSE_WAKE` relay from the
    /// raw-input handler (`wake_if_near_body`), which fires while the cursor
    /// approaches or leaves the body. Queued raw input itself does not break
    /// this wait (measured: planted moves sat out the full timeout on Win11),
    /// which is why the relay exists. The timeout is only the fallback for
    /// the periodic polls (power, net, disk).
    pub fn wait_for_input(&self, timeout: std::time::Duration) {
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: no handles, so the wait is on this thread's input queue only;
        // the return value is a wake reason we do not need to inspect.
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(None, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }

    /// Publish the live body's position/size inside the fixed frame.
    ///
    /// Sets the window *region* to exactly the body rect, so Windows itself
    /// routes every click, hover and `WM_MOUSEMOVE` outside that rect to the
    /// window below — the transparent margins around the collapsed pill (and
    /// the strip under it) are untouchable by us, which is precisely what the
    /// desktop icons that live near the notch need. `WM_NCHITTEST` and
    /// `pointer_over` keep the same rect as a second line of defence; all
    /// three read the same `hit_body` helper so they cannot disagree.
    pub fn set_body(
        &mut self,
        logical_w: f32,
        logical_h: f32,
        frame_w: f32,
        offset_y: f32,
        settled: bool,
    ) {
        self.settled = settled;
        let width = self.dpi().snap(logical_w) as i32;
        self.hover_width_px = width.max(1);
        self.state.body_inset = ((self.dpi().snap(frame_w) as i32 - width) / 2).max(0);
        self.state.body_width = width.max(0);
        self.state.body_height = self.dpi().snap(logical_h) as i32;
        self.state.body_offset_y = self.dpi().snap_offset(offset_y);
        // Window coords (popup: client == window): the body rect *extended*
        // out to the ear wings, which the outline now draws OUTSIDE the body,
        // flaring to the frame's top corners (the frame margin exists for them).
        // `WM_NCHITTEST` keeps passing clicks in the ears to the desktop
        // (`HTTRANSPARENT`), so the bigger region only unclips the drawing.
        let ear = self.dpi().snap(crate::ui::layout::EAR_W) as i32;
        let l = (self.state.body_inset - ear).max(0);
        let t = self.state.body_offset_y.max(0);
        let (r, b) = (l + self.state.body_width + 2 * ear, t + self.state.body_height);
        // SetWindowRgn is a compositor round-trip (~30-60 ms on this box), so
        // it must not run per morph frame: that was the transition stall, not
        // the drawing.
        //
        // While any spring is live the region is the WHOLE frame. A grown-only
        // union was not enough: the hover peek slides the body 4 px down, and
        // re-cut the top edge upward mid-drop, so the pill's top (and its
        // shadow) were clipped for the length of the morph — the "hovering
        // sometimes cuts the notch off". The full frame can never clip, and it
        // costs nothing in click-through: `WM_NCHITTEST` still answers
        // `HTTRANSPARENT` everywhere outside `hit_body`, and `pointer_over`
        // bounds hover to the same rect.
        //
        // At rest the region is the exact body (plus the ear wings the outline
        // draws outside it), which is what keeps the transparent margins from
        // eating desktop clicks at all.
        // At rest the region is the body plus a small pad: the island draws a
        // soft shadow past its own rect, and an exact-cut region shaved that
        // off — the "top/bottom of the notch looks clipped" report. The pad
        // stays click-through anyway (`hit_body` is unchanged), it only
        // unclips ink.
        let pad = self.dpi().snap(8.0) as i32;
        let (fw, fh) = self.frame_px();
        let rgn = if settled {
            (
                (l - pad).max(0),
                (t - pad).max(0),
                (r + pad).min(fw),
                (b + pad).min(fh),
            )
        } else {
            (0, 0, fw, fh)
        };
        if rgn != self.state.applied_rgn {
            self.state.applied_rgn = rgn;
            // SAFETY: CreateRectRgn returns an owned HRGN; SetWindowRgn takes
            // ownership on success *and* failure (MSDN), so there is no leak
            // and no double free. The region is in window coordinates.
            let hrgn = unsafe { CreateRectRgn(rgn.0, rgn.1, rgn.2, rgn.3) };
            unsafe { SetWindowRgn(self.hwnd, Some(hrgn), true) };
        }
    }

    /// The fixed frame size in physical px (client == window: WS_POPUP).
    fn frame_px(&self) -> (i32, i32) {
        let mut r = RECT::default();
        // SAFETY: our own HWND; a failure leaves the rect zeroed, which the
        // caller treats as a degenerate region it will retry next frame.
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        ((r.right - r.left).max(1), (r.bottom - r.top).max(1))
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
                        // The desktop itself (`Progman`/`WorkerW`) satisfies both
                        // — showing the desktop is not a fullscreen app, and
                        // hiding for it is the "notch vanishes on the desktop"
                        // bug — so the shell's own windows never qualify.
                        let style = GetWindowLongW(fg, GWL_STYLE);
                        !is_shell_desktop(&window_class(fg))
                            && covers_monitor(&fg_rect, &mi.rcMonitor)
                            && strips_chrome(style)
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
        // SAFETY: unregistering hotkeys we registered against our own HWND.
        // A failed registration means the id is ours to ignore, not to free.
        let _ = unsafe { UnregisterHotKey(Some(self.hwnd), HOTKEY_VISIBILITY) };
        if self.hotkey_registered {
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
    fn hit_body_covers_the_pill_only() {
        // Body rect = inset 330, 231x40 at top offset 0 in the 850x250 frame.
        assert!(hit_body(330, 0, 330, 231, 40, 0), "pill top-left corner");
        assert!(hit_body(445, 39, 330, 231, 40, 0), "pill bottom middle");
        // Ear margins and the strip below the pill must pass through, or the
        // invisible frame eats desktop clicks.
        assert!(!hit_body(329, 20, 330, 231, 40, 0), "left ear");
        assert!(!hit_body(561, 20, 330, 231, 40, 0), "right ear");
        assert!(!hit_body(445, 40, 330, 231, 40, 0), "below the pill");
        // A detached (peek) body sits `offset_y` below the screen top.
        assert!(hit_body(330, 8, 330, 231, 40, 8), "peek body top");
        assert!(hit_body(445, 47, 330, 231, 40, 8), "peek body bottom");
        assert!(!hit_body(445, 0, 330, 231, 40, 8), "gap above detached pill");
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
    fn the_shell_desktop_is_never_treated_as_fullscreen() {
        // The desktop covers the monitor with no caption: geometrically a
        // perfect fullscreen match. Showing the desktop must not hide the
        // island, so the shell's own classes are excluded up front.
        assert!(is_shell_desktop("Progman"));
        assert!(is_shell_desktop("WorkerW"));
        assert!(!is_shell_desktop("Chrome_WidgetWin_1"));
        assert!(!is_shell_desktop(""));
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
