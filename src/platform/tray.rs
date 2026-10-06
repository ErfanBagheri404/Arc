//! System-tray presence via a single `Shell_NotifyIcon`.
//!
//! Owns only the notification template and the NIM/menu calls. `Overlay`
//! forwards [`WM_TRAY`] from its wnd_proc here and pushes whatever
//! [`on_message`] returns, so the event queue stays encapsulated over there.
//! Left click toggles the island, right click opens a Quit menu, an Explorer
//! restart re-adds the icon.
//!
//! Degrade, never fail: if the icon resource or a shell call does not
//! cooperate, the island still runs tray-less.

use std::sync::Mutex;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NOTIFY_ICON_DATA_FLAGS, NOTIFY_ICON_MESSAGE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, HICON, IMAGE_ICON,
    LR_SHARED, LoadImageW, MF_SEPARATOR, MF_STRING, SM_CXSMICON, SetForegroundWindow,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_LBUTTONUP, WM_RBUTTONUP, WM_USER,
};

use super::Event;

/// Shell posts our callback message here; the low word of `lParam` carries the
/// mouse message. Private range, fixed by us.
pub const WM_TRAY: u32 = WM_USER + 1;

const TRAY_UID: u32 = 1;
const ID_QUIT: usize = 1;

// `NIM_*` and `NIF_*` are not re-exported by the `windows` crate at 0.62, but
// their values are fixed part of the Win32 ABI — same pattern as
// `WM_MOUSELEAVE` in `window.rs`.
const NIM_ADD: NOTIFY_ICON_MESSAGE = NOTIFY_ICON_MESSAGE(0);
const NIM_DELETE: NOTIFY_ICON_MESSAGE = NOTIFY_ICON_MESSAGE(2);
const NIF_MESSAGE: NOTIFY_ICON_DATA_FLAGS = NOTIFY_ICON_DATA_FLAGS(0x1);
const NIF_ICON: NOTIFY_ICON_DATA_FLAGS = NOTIFY_ICON_DATA_FLAGS(0x2);
const NIF_TIP: NOTIFY_ICON_DATA_FLAGS = NOTIFY_ICON_DATA_FLAGS(0x4);

/// Our copy of the notification template. Raw handles are stored as `usize` so
/// this is plain `Copy` and can live in a `Mutex` static; the shell only reads
/// them, on our own single GUI thread.
#[derive(Clone, Copy)]
struct Nid {
    hwnd: usize,
    icon: usize,
    tip: [u16; 128],
}

impl Nid {
    fn to_shell(self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(self.hwnd as *mut core::ffi::c_void),
            uID: TRAY_UID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: HICON(self.icon as *mut core::ffi::c_void),
            szTip: self.tip,
            ..unsafe { std::mem::zeroed() }
        }
    }
}

static TRAY: Mutex<Option<Nid>> = Mutex::new(None);

/// The exe's first icon resource (`1 ICON …` in `app.rc`), sized for a tray /
/// small-caption slot. `None` when the resource is missing.
pub(crate) fn load_icon() -> Option<HICON> {
    // MAKEINTRESOURCEW(1): app.rc declares the icon as `1 ICON "…"`.
    let name = windows::core::PCWSTR(1 as *const u16);
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    // SAFETY: null module name resolves *this* exe, which owns the resource.
    let module = unsafe { GetModuleHandleW(None) }.ok()?;
    // SAFETY: LR_SHARED means the system owns the handle, so it needs no
    // DestroyIcon and stays valid for the process lifetime.
    match unsafe {
        LoadImageW(
            Some(HINSTANCE(module.0 as *mut core::ffi::c_void)),
            name,
            IMAGE_ICON,
            size,
            size,
            LR_SHARED,
        )
    } {
        Ok(h) if !h.is_invalid() => Some(HICON(h.0)),
        _ => None,
    }
}

/// Add the icon. Returns false on any failure — the app then runs tray-less,
/// exactly like a lost hotkey registration.
pub fn install(hwnd: HWND) -> bool {
    let Some(icon) = load_icon() else {
        log::warn!("arc: tray icon resource missing — running without a tray icon");
        return false;
    };
    let mut tip = [0u16; 128];
    for (dst, src) in tip.iter_mut().zip("Arc".encode_utf16()) {
        *dst = src;
    }
    let nid = Nid {
        hwnd: hwnd.0 as usize,
        icon: icon.0 as usize,
        tip,
    };
    // SAFETY: the shell only reads the struct for the call's duration.
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid.to_shell()) }.as_bool();
    if ok {
        *TRAY.lock().unwrap() = Some(nid);
    } else {
        log::warn!("arc: Shell_NotifyIcon(NIM_ADD) failed");
    }
    ok
}

/// Remove the icon. Must run while the HWND is still alive, so `Overlay::drop`
/// calls it *before* `DestroyWindow`.
pub fn remove() {
    if let Some(nid) = TRAY.lock().unwrap().take() {
        // SAFETY: best effort; a failure here just leaves a stale shell entry.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid.to_shell()) };
    }
}

/// Explorer restarted: the tray was rebuilt and our icon is gone even though we
/// never deleted it. Delete the stale entry, then add it back.
pub fn refresh() {
    let snapshot = *TRAY.lock().unwrap();
    if let Some(nid) = snapshot {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid.to_shell()) };
        let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &nid.to_shell()) };
    }
}

/// Route a `WM_TRAY` callback. Returns the event `Overlay` should push, if any.
pub fn on_message(hwnd: HWND, lparam: LPARAM) -> Option<Event> {
    // The shell packs the mouse message into the low word of lParam.
    let mouse = (lparam.0 as u32) & 0xffff;
    if mouse == WM_LBUTTONUP {
        return Some(Event::ToggleIsland);
    }
    if mouse == WM_RBUTTONUP {
        return show_menu(hwnd);
    }
    None
}

fn show_menu(hwnd: HWND) -> Option<Event> {
    // SAFETY: creates an owned menu; every early exit destroys it below.
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    let quit = wide("Quit Arc");
    // A separator above Quit mirrors every other tray menu; no other commands
    // exist yet, so nothing else is added.
    // SAFETY: menu is live, PCWSTR points at `quit` for the call's duration.
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, windows::core::PCWSTR(quit.as_ptr()));
    }

    let mut pt = POINT::default();
    let _ = unsafe { GetCursorPos(&mut pt) };
    // SetForegroundWindow is the documented requirement for a tray popup to
    // dismiss when the user clicks elsewhere. Our window is WS_EX_NOACTIVATE,
    // so it may fail; the menu still works, it just may linger.
    let _ = unsafe { SetForegroundWindow(hwnd) };
    // SAFETY: menu is live and owned here; TPM_RETURNCMD makes the call hand
    // back the chosen id instead of posting WM_COMMAND.
    let picked = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        )
    };
    // SAFETY: we created the menu and nothing else can reference it.
    let _ = unsafe { DestroyMenu(menu) };

    if picked.0 as usize == ID_QUIT {
        Some(Event::Quit)
    } else {
        None
    }
}

/// UTF-16, NUL-terminated.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
