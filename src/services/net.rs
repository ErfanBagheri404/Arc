//! Network state: internet reachability and VPN adapter status.
//!
//! Both facts come from cheap, allocation-free calls on the app's own thread,
//! so there is no worker and no snapshot type. `InternetGetConnectedState` is
//! the OS's own answer to "do I have a link" (DynamicNotch's offline HUD),
//! and the interface table's descriptions tell us whether a VPN tunnel
//! adapter exists — every Windows VPN driver names itself `... VPN`
//! (WireGuard Tunnel, AnyConnect VPN, L2TP VPN…), so a case-insensitive
//! substring match classifies them without touching the registry or
//! spawning anything.
//!
//! Reachability is deliberately the OS's link answer, not a ping: no probe
//! traffic, no false "offline" behind a captive portal, and it is what the
//! Windows tray icon itself reports.
//!
//! `INTERNET_CONNECTION_*` flags live in `Win32::Networking::WinInet` and the
//! interface table in `Win32::NetworkManagement::IpHelper`.

use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::Networking::WinInet::{
    InternetGetConnectedState, INTERNET_CONNECTION_LAN, INTERNET_CONNECTION_MODEM,
};

/// Both facts at once, so the caller polls once per frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetState {
    /// The OS reports some network connection.
    pub online: bool,
    /// Name of a VPN/tunnel adapter that is present, e.g. `WireGuard Tunnel`.
    pub vpn: Option<String>,
}

/// Read both now. At the HUD's 1 Hz this is two syscall-cheap queries.
pub fn sample() -> NetState {
    NetState {
        online: online(),
        vpn: vpn_tunnel(),
    }
}

/// Whether the machine currently reports a network connection.
///
/// `INTERNET_CONNECTION` is the aggregate "we have a connection" bit; the
/// LAN and MODEM bits say how. Only the aggregate matters here, and a
/// failed query reads as offline: a HUD claiming "online" when we cannot tell
/// would be a lie, and this is the state the Windows tray icon also shows.
pub fn online() -> bool {
    let mut flags = windows::Win32::Networking::WinInet::INTERNET_CONNECTION(0);
    // SAFETY: `flags` is a live out-param for the call's duration.
    if unsafe { InternetGetConnectedState(&mut flags, None) }.is_err() {
        return false;
    }
    flags.0 & (INTERNET_CONNECTION_LAN.0 | INTERNET_CONNECTION_MODEM.0) != 0
}

/// The first VPN-looking adapter's description, or `None`.
///
/// `GetIfTable2` allocates the table, so it is freed here with
/// `FreeMibTable` the moment we are done with it: no drop-glue struct, and
/// `name` is an owned `String` so nothing borrows the table across the free.
pub fn vpn_tunnel() -> Option<String> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // SAFETY: `table` is a live out-param written only on success.
    if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
        return None;
    }
    let name = unsafe { vpn_name_in_table(table) };
    // SAFETY: `table` is an owned allocation from `GetIfTable2`; the `name`
    // we return is an owned `String`, so nothing still borrows it.
    unsafe { FreeMibTable(table as *mut _) };
    name
}

/// Borrow a VPN-looking description out of a live table.
///
/// # Safety
///
/// `table` must be a valid pointer from a successful `GetIfTable2`.
unsafe fn vpn_name_in_table(table: *mut MIB_IF_TABLE2) -> Option<String> {
    // SAFETY: `table` is valid and owned by the caller.
    let table_ref = unsafe { &*table };
    let rows = unsafe {
        std::slice::from_raw_parts(
            table_ref.Table.as_ptr(),
            table_ref.NumEntries as usize,
        )
    };
    rows.iter().find_map(|row| {
        let units: Vec<u16> = row
            .Description
            .iter()
            .copied()
            .take_while(|c| *c != 0)
            .collect();
        if units.is_empty() {
            return None;
        }
        let name = String::from_utf16(&units).ok()?;
        looks_like_vpn(&name).then_some(name)
    })
}

/// Every VPN driver Windows ships describes its adapter with `VPN`,
/// `Tunnel` or `WireGuard` in the name. Case-insensitive, so a user's custom
/// description ("home VPN") classifies too.
fn looks_like_vpn(description: &str) -> bool {
    let d = description.to_ascii_uppercase();
    d.contains("VPN") || d.contains("TUNNEL") || d.contains("WIREGUARD")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vpn_classification_covers_every_shipped_driver() {
        for name in [
            "WireGuard Tunnel",
            "Cisco AnyConnect VPN",
            "VPN Adapter",
            "IKEv2 VPN",
            "Ethernet 2",
        ] {
            assert_eq!(looks_like_vpn(name), !name.contains("Ethernet"), "{name}");
        }
    }

    #[test]
    fn sampling_never_panics_and_is_consistent_with_its_parts() {
        let s = sample();
        assert_eq!(s.online, online());
        assert_eq!(s.vpn, vpn_tunnel());
    }
}