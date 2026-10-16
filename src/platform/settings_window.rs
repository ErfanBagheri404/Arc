//! The standalone Settings window — opened from the tray's "Settings…", never
//! from a notch tab.
//!
//! A real, ordinary window: it has a caption, a close button, and can be
//! dragged and parked anywhere. The island is an ornament; settings are a place
//! the user leaves open while they work, so they get a normal HWND rather than
//! a second overlay.
//!
//! The window owns its own [`Renderer`]. DirectComposition hangs the visual off
//! this window's own target, so composing it never disturbs the island and
//! closing it never blanks the notch.
//!
//! It has no message loop of its own: the island's pump already drains **every**
//! message on this thread (`PeekMessageW` with `hwnd = None`), so this window's
//! `WM_SIZE` / `WM_LBUTTONDOWN` / `WM_CLOSE` are dispatched there and land in
//! `WinState` for the app to read.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{HBRUSH, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetWindowLongPtrW,
    IsWindowVisible, PeekMessageW, RegisterClassExW, SetWindowLongPtrW, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, MSG, PM_REMOVE, SW_HIDE, SW_SHOW, WM_CLOSE,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_SIZE,
    WNDCLASSEXW, WS_CAPTION, WS_EX_TOOLWINDOW, WS_SYSMENU, WS_VISIBLE,
};

use super::render::Renderer;
use crate::core::scene::Frame;

/// Window class name, registered once per process.
const CLASS_NAME: &str = "ArcSettingsWindow";

/// Wheel notches per scroll step — the unit `WM_MOUSEWHEEL` reports in.
pub const WHEEL_DELTA: i32 = 120;
/// Logical px a wheel notch travels. A notch is a small flick, not a page.
pub const SCROLL_STEP: f32 = 32.0;

/// Logical client size, in DIPs. The content is laid out for exactly this, so
/// the window keeps a fixed size and there is no reflow to get wrong. It is
/// tall enough that most of the sheet is visible; the pet gallery scrolls
/// inside its own viewport.
pub const LOGICAL_W: f32 = 460.0;
pub const LOGICAL_H: f32 = 560.0;

/// Client size in physical px for a given DPI scale.
fn client_px(scale: f32) -> (i32, i32) {
    (
        (LOGICAL_W * scale).round().max(1.0) as i32,
        (LOGICAL_H * scale).round().max(1.0) as i32,
    )
}

/// Outer window size, in physical px, that leaves exactly [`client_px`] of
/// client area.
///
/// `CreateWindowExW` is handed the *whole* window size, so passing the client
/// size straight through made `WS_CAPTION` and the border eat into the content:
/// the layout was 460x150 logical while the client area was ~40 px shorter, and
/// the bottom row was clipped off the visible sheet.
fn window_px(scale: f32) -> (i32, i32) {
    let (cw, ch) = client_px(scale);
    let mut r = RECT {
        left: 0,
        top: 0,
        right: cw,
        bottom: ch,
    };
    let dpi = (scale * 96.0).round().max(96.0) as u32;
    // SAFETY: a pure measurement call over a stack RECT — no window involved,
    // and the result is only read back below.
    let measured = unsafe {
        AdjustWindowRectExForDpi(
            &mut r,
            WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_TOOLWINDOW,
            dpi,
        )
    };
    if measured.is_err() {
        return (cw, ch);
    }
    (
        (r.right - r.left).max(1),
        (r.bottom - r.top).max(1),
    )
}

/// Events the app hands to `app` after each pump.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsEvent {
    /// A click inside the client area, in client-space physical px.
    Click { x: i32, y: i32 },
    /// A wheel notch. Positive scrolls up (content moves down), the sign
    /// Windows' `WM_MOUSEWHEEL` carries; the app converts it to logical px.
    Scroll { delta: i32 },
    /// The window was closed. Hidden, not destroyed: reopening is instant and
    /// the renderer never has to rebuild its chain.
    Closed,
}

/// What the window needs to remember about itself inside its own `WndProc`.
struct WinState {
    /// Pending events, drained by the app after each pump.
    events: Vec<SettingsEvent>,
    /// Client size in physical px, refreshed on every `WM_SIZE`.
    physical: (u32, u32),
    /// DPI scale, so click coords can be mapped back to logical px.
    scale: f32,
}

static REGISTERED: AtomicBool = AtomicBool::new(false);

/// Ask the app to open the settings window on its next iteration.
///
/// The tray callback must not create windows, so it only raises a flag; the app
/// loop does the real work.
static REQUEST: Mutex<bool> = Mutex::new(false);

pub fn request_open() {
    if let Ok(mut guard) = REQUEST.lock() {
        *guard = true;
    }
}

/// Take the pending open request, if any.
pub fn open_requested() -> bool {
    match REQUEST.lock() {
        Ok(mut guard) => std::mem::replace(&mut *guard, false),
        Err(_) => false,
    }
}

/// UTF-16, NUL-terminated. Win32 copies class names at registration time, so
/// the buffer only has to outlive the call that uses it.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn register_class() -> bool {
    if REGISTERED.load(Ordering::Acquire) {
        return true;
    }
    // SAFETY: a null module name asks for the current executable image.
    let Ok(module) = (unsafe { GetModuleHandleW(PCWSTR::null()) }) else {
        log::error!("arc: GetModuleHandleW failed; settings window unavailable");
        return false;
    };
    let class_name = wide(CLASS_NAME);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hIcon: Default::default(),
        hCursor: Default::default(),
        // No brush: the DirectComposition visual paints the whole client area,
        // so erasing behind it would only flash the default background.
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        lpszMenuName: Default::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        hInstance: HINSTANCE::from(module),
        hIconSm: Default::default(),
    };
    // SAFETY: `class` is correctly sized and its strings are NUL-terminated.
    let atom = unsafe { RegisterClassExW(&class) };
    if atom == 0 {
        log::error!("arc: RegisterClassExW failed for {CLASS_NAME}");
        return false;
    }
    REGISTERED.store(true, Ordering::Release);
    true
}

fn userdata(hwnd: HWND) -> Option<&'static mut WinState> {
    // SAFETY: the pointer is published by `SettingsWindow::open` and cleared in
    // `Drop` before the Box is freed, so it is never dangling while a message
    // can still arrive.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WinState;
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

fn push_event(hwnd: HWND, event: SettingsEvent) {
    if let Some(state) = userdata(hwnd) {
        state.events.push(event);
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // The visual owns the client area; erasing first would flash.
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            // LPARAM carries client coords in physical px.
            push_event(
                hwnd,
                SettingsEvent::Click {
                    x: (lparam.0 & 0xFFFF) as i32,
                    y: ((lparam.0 >> 16) & 0xFFFF) as i32,
                },
            );
            LRESULT(0)
        }
        // The pet gallery is longer than the window, so the sheet scrolls.
        // `wparam`'s high word is the notch count, signed; the low word is the
        // virtual key state, which the sheet has no use for.
        WM_MOUSEWHEEL => {
            push_event(
                hwnd,
                SettingsEvent::Scroll {
                    delta: ((wparam.0 >> 16) as i16) as i32,
                },
            );
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = userdata(hwnd) {
                state.physical =
                    ((lparam.0 & 0xFFFF) as u32, ((lparam.0 >> 16) & 0xFFFF) as u32);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(state) = userdata(hwnd) {
                // Only the scale changes here; the new pixel size arrives as its
                // own WM_SIZE, so recording the scale is enough.
                state.scale = (wparam.0 & 0xFFFF) as f32 / 96.0;
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            push_event(hwnd, SettingsEvent::Closed);
            // SAFETY: our own HWND. Hiding keeps the HWND and its composition
            // chain alive, so reopening costs nothing.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The tray's "Settings…" answer: a normal window, not a notch tab.
pub struct SettingsWindow {
    hwnd: HWND,
    state: Box<WinState>,
    renderer: Renderer,
}

impl SettingsWindow {
    /// Create the window and show it.
    pub fn open() -> Option<Self> {
        if !register_class() {
            return None;
        }
        // SAFETY: a null module name asks for the current executable image.
        let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok()?;
        let instance = HINSTANCE::from(module);
        let class = wide(CLASS_NAME);
        let title = wide("Arc Settings");
        // CreateWindowExW takes the OUTER size, so the caption and border are
        // added on top of the client area we want.
        let dpi = unsafe { GetDpiForSystem() };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let (w, h) = window_px(scale);

        // SAFETY: every argument is owned or borrowed for the call's duration.
        let created = unsafe {
            CreateWindowExW(
                // TOOLWINDOW keeps it out of the taskbar and Alt-Tab: it belongs
                // to the tray icon, so it should not compete with real apps.
                WS_EX_TOOLWINDOW,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                // Caption, close button, and a visible border, but no resize
                // border and no maximise button: the content is laid out for one
                // fixed size.
                WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                w,
                h,
                None,
                None,
                Some(instance),
                None,
            )
        };

        let hwnd = match created {
            Ok(h) if !h.0.is_null() => h,
            Ok(_) => {
                log::error!("arc: CreateWindowExW returned a null settings HWND");
                return None;
            }
            Err(err) => {
                log::error!("arc: CreateWindowExW failed for the settings window: {err:?}");
                return None;
            }
        };

        let mut state = Box::new(WinState {
            events: Vec::new(),
            physical: (w as u32, h as u32),
            scale: 1.0,
        });
        // SAFETY: publishes the Box's stable address for the window's lifetime;
        // cleared in `Drop` before the Box is freed.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *state as *mut WinState) as isize)
        };

        let renderer = Renderer::new(NonNull::new(hwnd.0)?)?;

        let mut window = Self {
            hwnd,
            state,
            renderer,
        };
        window.refresh_metrics();
        // SAFETY: our own HWND. SW_SHOW, not SW_SHOWNOACTIVATE: the user asked
        // for this window, so it is right for it to come forward.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }
        Some(window)
    }

    /// Re-read the client size and DPI from the live window.
    ///
    /// The size is measured off the window, never assumed: `CreateWindowExW`
    /// applies the system DPI before the window lands on its monitor, so the
    /// requested pixel size and the client area that actually exists can differ.
    /// Trusting the request drew the sheet into a client area it did not have,
    /// which is how the bottom row ended up clipped.
    fn refresh_metrics(&mut self) {
        // SAFETY: read-only queries on our own HWND.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        self.state.scale = scale;
        let mut r = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut r) }.is_ok() {
            let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
            self.state.physical = (w as u32, h as u32);
        } else {
            let (w, h) = client_px(scale);
            self.state.physical = (w.max(1) as u32, h.max(1) as u32);
        }
    }

    /// DPI scale of the monitor the window lives on.
    pub fn scale(&self) -> f32 {
        self.state.scale
    }

    /// Client size in logical px — what `ui::settings` lays out against.
    pub fn logical_size(&self) -> (f32, f32) {
        let s = self.state.scale.max(0.01);
        (
            self.state.physical.0 as f32 / s,
            self.state.physical.1 as f32 / s,
        )
    }

    /// Take the events pumped in since the last call.
    pub fn take_events(&mut self) -> Vec<SettingsEvent> {
        std::mem::take(&mut self.state.events)
    }

    /// Resize the backing chain to the live client area, then present `frame`.
    pub fn present(&mut self, frame: &Frame) {
        let (w, h) = self.state.physical;
        self.renderer.resize(w.max(1), h.max(1));
        let scale = self.state.scale;
        self.renderer.present(frame, scale, 0.0, 0.0, true);
    }

    /// Bring a hidden window back, re-reading its live metrics first.
    pub fn show(&mut self) {
        self.refresh_metrics();
        // SAFETY: our own HWND.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    /// Is the window currently on screen?
    pub fn visible(&self) -> bool {
        // SAFETY: pure read-only query on our own HWND.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    /// Drain this window's own message queue before the island's pump runs.
    pub fn pump(&mut self) {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid, correctly sized MSG. Peeking on our own
        // HWND never takes another window's messages.
        while unsafe { PeekMessageW(&mut msg, Some(self.hwnd), 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_DESTROY {
                break;
            }
            // SAFETY: standard message pump, scoped to `msg.hwnd`.
            unsafe {
                let _ = DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        // SAFETY: clears the pointer `wnd_proc` reads before the Box is freed,
        // so no later message can land on freed memory.
        unsafe {
            let _ = SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_size_is_the_layout_contract() {
        assert_eq!(LOGICAL_W, 460.0);
        assert_eq!(LOGICAL_H, 560.0);
    }

    #[test]
    fn client_px_scales_with_dpi() {
        assert_eq!(client_px(1.0), (460, 560));
        // 125 %, the user's display: the client area must grow, not the padding.
        assert_eq!(client_px(1.25), (575, 700));
    }

    /// `CreateWindowExW` is handed the OUTER size, so the client size must never
    /// be passed as it. When it was, `WS_CAPTION` ate the difference and the
    /// bottom settings row was clipped off the visible sheet at 125 %.
    #[test]
    fn the_window_is_bigger_than_its_client_by_the_caption() {
        for scale in [1.0, 1.25, 1.5] {
            let (cw, ch) = client_px(scale);
            let (ww, wh) = window_px(scale);
            assert!(ww > cw, "scale {scale}: width {ww} must exceed client {cw}");
            assert!(wh > ch, "scale {scale}: height {wh} must exceed client {ch}");
            // A caption is never thinner than ~20 px at 96 dpi, and it grows
            // with the scale — a one-pixel difference means the adjust was
            // applied at the wrong DPI or skipped.
            assert!(
                wh - ch >= 20,
                "scale {scale}: caption only {} px of headroom",
                wh - ch
            );
        }
    }

    /// The invariant the renderer depends on: `logical_size` divides the
    /// MEASURED client by the DPI, and the UI must never end up with less room
    /// than it laid out for. `client_px` rounds to whole physical px, so the
    /// round-trip can give back at most half a physical pixel of slack — never a
    /// shortfall, which is what clips the last row.
    #[test]
    fn the_measured_client_never_comes_back_smaller_than_the_contract() {
        for s in [1.0, 1.25, 1.5] {
            let (cw, ch) = client_px(s);
            let slack = 0.5 / s;
            let (lw, lh) = (cw as f32 / s, ch as f32 / s);
            assert!(
                lw >= LOGICAL_W - 1e-3 && lw <= LOGICAL_W + slack + 1e-3,
                "scale {s}: logical width {lw} outside [{}, {}]",
                LOGICAL_W,
                LOGICAL_W + slack
            );
            assert!(
                lh >= LOGICAL_H - 1e-3 && lh <= LOGICAL_H + slack + 1e-3,
                "scale {s}: logical height {lh} outside [{}, {}]",
                LOGICAL_H,
                LOGICAL_H + slack
            );
        }
    }

    #[test]
    fn the_open_flag_is_drained_once() {
        request_open();
        assert!(open_requested());
        assert!(!open_requested());
    }
}
