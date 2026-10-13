//! System-status probes: focus mode and display brightness.
//!
//! Both answer a question the OS already tracks, in one call each, so there is
//! no worker here — the app polls them alongside the network sample.
//!
//! *Focus* maps onto Windows' own notion of "do not interrupt me":
//! `SHQueryUserNotificationState` reports whether the shell is suppressing
//! notifications (a running full-screen app, a presentation, or the user's own
//! Do Not Disturb). DynamicNotch has a Focus live activity; this is the
//! Windows equivalent without inventing a parallel settings store.
//!
//! *Brightness* is the monitor's own value via DDC/CI (`GetMonitorBrightness`).
//! The `windows` binding for `GetPhysicalMonitorsFromHMONITOR` takes a slice
//! and derives the size from it, so the classic count-then-fill dance does not
//! exist here: a one-element slice is the overwhelmingly common case, and
//! anything else fails the call and yields `None`. Failing is the correct
//! outcome for displays without DDC/CI (TVs, some docks) — no HUD, never a
//! fake 0 %.

use windows::Win32::Devices::Display::{
    GetMonitorBrightness, GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, POINT};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_NOT_PRESENT, QUNS_QUIET_TIME,
};

/// Is the user in a do-not-disturb context right now?
pub fn focus_on() -> bool {
    // SAFETY: no pointers — the binding returns the state by value.
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) => {
            let s = state.0;
            s == QUNS_BUSY.0 || s == QUNS_QUIET_TIME.0 || s == QUNS_NOT_PRESENT.0
        }
        // No answer: assume the user *can* be interrupted, never the reverse.
        Err(_) => false,
    }
}

/// Current brightness of the primary monitor as 0..=100, `None` when no
/// physical monitor reports DD/CI brightness.
pub fn brightness() -> Option<u8> {
    // SAFETY: plain handle + caller-owned buffer; all reads follow success.
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut monitors = [PHYSICAL_MONITOR {
            hPhysicalMonitor: HANDLE::default(),
            szPhysicalMonitorDescription: [0; 128],
        }];
        if GetPhysicalMonitorsFromHMONITOR(hmon, &mut monitors).is_err() {
            return None;
        }
        let mut min = 0u32;
        let mut cur = 0u32;
        let mut max = 100u32;
        // The binding returns the raw BOOL as i32.
        let ok = GetMonitorBrightness(monitors[0].hPhysicalMonitor, &mut min, &mut cur, &mut max)
            != 0;
        let _ = CloseHandle(monitors[0].hPhysicalMonitor);
        if !ok || max <= min {
            return None;
        }
        let pct = (cur - min) as f32 / (max - min) as f32 * 100.0;
        Some(pct.round().clamp(0.0, 100.0) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_and_brightness_answer_without_panicking() {
        // Values depend on the machine; the contract is "never panics".
        let _ = focus_on();
        let _ = brightness();
    }

    #[test]
    fn brightness_is_a_percentage_when_present() {
        if let Some(b) = brightness() {
            assert!(b <= 100, "{b} must be a percentage");
        }
    }
}
