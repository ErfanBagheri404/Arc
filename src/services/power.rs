//! Power state: one `GetSystemPowerStatus` read, no thread and no COM.
//!
//! The call is a cheap in-process getter (unlike SMTC or PDH), so the app polls
//! it on its slow tick rather than paying for a worker thread and a channel.
//! `ACLineStatus == 255` means "unknown", which must not be misread as "on
//! battery" or the HUD would fire on every desktop.

pub use crate::app::hud::Power;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

/// Battery percentage at or below which the HUD turns amber (Atoll: 15 %).
pub const LOW_PERCENT: u8 = 15;

/// Read the current power state. `None` when the call fails outright.
pub fn read() -> Option<Power> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: out-pointer to a correctly sized, zeroed struct we own.
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;

    // 255 = unknown (a desktop with no battery, or a VM).
    let percent = match s.BatteryLifePercent {
        255 => None,
        p => Some(p),
    };
    Some(Power {
        percent,
        // 1 = AC online, 0 = on battery, 255 = unknown.
        charging: s.ACLineStatus == 1,
    })
}

/// True when a battery reading is low enough to warn about.
pub fn is_low(percent: Option<u8>) -> bool {
    percent.is_some_and(|p| p <= LOW_PERCENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_threshold_is_inclusive_at_15() {
        assert!(is_low(Some(15)));
        assert!(is_low(Some(1)));
        assert!(!is_low(Some(16)));
        // No battery must never read as low: a desktop is not "0 % and dying".
        assert!(!is_low(None));
    }

    #[test]
    fn read_returns_something_on_a_real_machine() {
        // Never asserts on the values: CI may have no battery at all. The call
        // must succeed and, if a percentage exists, be a sane one.
        let p = read().expect("GetSystemPowerStatus");
        if let Some(pct) = p.percent {
            assert!(pct <= 100, "battery percent out of range: {pct}");
        }
    }
}
