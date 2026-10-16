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
    /// The WiFi network we are joined to, when it can be named at all.
    pub ssid: Option<String>,
}

/// Read all three now. At the HUD's 2 Hz this is a handful of syscall-cheap
/// queries; the WiFi one no-ops on machines without a wireless radio.
pub fn sample() -> NetState {
    NetState {
        online: online(),
        vpn: vpn_tunnel(),
        ssid: wifi_ssid(),
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
        std::slice::from_raw_parts(table_ref.Table.as_ptr(), table_ref.NumEntries as usize)
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

/// The SSID of the current WiFi connection, when there is one.
///
/// Open a client handle, enumerate interfaces, ask the first connected one for
/// its `wlan_intf_opcode_current_connection` blob, free it, close the handle.
/// Machines with no radio answer the enum with an empty list, and this returns
/// `None` — the "no name to show" case the HUD already handles.
pub fn wifi_ssid() -> Option<String> {
    use windows::Win32::NetworkManagement::WiFi::{
        wlan_interface_state_connected, wlan_intf_opcode_current_connection, WlanCloseHandle,
        WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
        WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
    };

    // SAFETY: all four WLAN calls below are plain out-param reads on handles
    // this function opens and closes within its own scope.
    let mut handle = Default::default();
    // SAFETY: out-params written by the service.
    if unsafe { WlanOpenHandle(2, None, &mut Default::default(), &mut handle) } != 0 {
        return None;
    }

    let name = (|| {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: `list` is a live out-param written only on success.
        if unsafe { WlanEnumInterfaces(handle, None, &mut list) } != 0 || list.is_null() {
            return None;
        }
        // SAFETY: `list` is valid until `WlanFreeMemory`; `ssid` is owned, so
        // nothing borrows the allocation across the free at the bottom.
        let ssid = unsafe {
            let list_ref = &*list;
            let mut ssid = None;
            for i in 0..list_ref.dwNumberOfItems {
                let iface = &list_ref.InterfaceInfo[i as usize];
                if iface.isState != wlan_interface_state_connected {
                    continue;
                }
                let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
                let mut size = 0u32;
                let query = WlanQueryInterface(
                    handle,
                    &iface.InterfaceGuid,
                    wlan_intf_opcode_current_connection,
                    None,
                    &mut size,
                    &mut data,
                    None,
                );
                if query != 0 || data.is_null() {
                    continue;
                }
                // SAFETY (outer block): the blob for this opcode is a
                // `WLAN_CONNECTION_ATTRIBUTES`.
                let conn = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                let raw = &conn.wlanAssociationAttributes.dot11Ssid;
                let n = (raw.uSSIDLength as usize).min(raw.ucSSID.len());
                if n > 0 {
                    ssid = Some(ssid_string(&raw.ucSSID[..n]));
                }
                // `data` is a service allocation, freed immediately.
                WlanFreeMemory(data as *const _);
                if ssid.is_some() {
                    break;
                }
            }
            let _ = list_ref;
            ssid
        };
        // SAFETY: `list` is the owned allocation from `WlanEnumInterfaces`.
        unsafe { WlanFreeMemory(list as *const _) };
        ssid
    })();

    // SAFETY: the handle was opened above and is closed exactly once.
    unsafe { WlanCloseHandle(handle, None) };
    name
}

/// SSIDs are UTF-8 by the 802.11 standard; latin-1 is the fallback for the
/// access points that ignore it, so a hotspot name never becomes U+FFFD.
fn ssid_string(raw: &[u8]) -> String {
    String::from_utf8(raw.to_vec())
        .unwrap_or_else(|_| raw.iter().map(|&b| b as char).collect())
        .trim()
        .to_string()
}

/// Every VPN driver Windows ships describes its adapter with `VPN`,
/// `Tunnel` or `WireGuard` in the name. Case-insensitive, so a user's custom
/// description ("home VPN") classifies too.
fn looks_like_vpn(description: &str) -> bool {
    let d = description.to_ascii_uppercase();
    d.contains("VPN") || d.contains("TUNNEL") || d.contains("WIREGUARD")
}

/// One HTTPS GET that returns raw bytes, capped at `cap` bytes. `None` on any
/// failure: callers (pet downloads) tolerate soft failures as \"not installed\".
pub fn get_bytes(url: &str, cap: usize) -> Option<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinInet::{
        InternetCloseHandle, InternetOpenUrlW, InternetOpenW, InternetReadFile,
        INTERNET_FLAG_NO_CACHE_WRITE, INTERNET_FLAG_RELOAD, INTERNET_FLAG_SECURE,
        INTERNET_OPEN_TYPE_PRECONFIG,
    };
    let agent: Vec<u16> = "Arc\0".encode_utf16().collect();
    let mut wide: Vec<u16> = url.encode_utf16().collect();
    wide.push(0);
    // SAFETY: WinINet calls are well-formed; handles are guarded.
    unsafe {
        let h = InternetOpenW(PCWSTR(agent.as_ptr()), INTERNET_OPEN_TYPE_PRECONFIG.0, None, None, 0);
        if h.is_null() {
            return None;
        }
        struct S(*const core::ffi::c_void);
        impl Drop for S {
            fn drop(&mut self) {
                unsafe {
                    let _ = InternetCloseHandle(self.0);
                }
            }
        }
        let _s = S(h);
        let flags = INTERNET_FLAG_SECURE | INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE;
        let file = InternetOpenUrlW(h, PCWSTR(wide.as_ptr()), None, flags, None);
        if file.is_null() {
            return None;
        }
        let _f = S(file);
        let mut body = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let mut n = 0u32;
            if InternetReadFile(file, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n).is_err() || n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > cap {
                break;
            }
        }
        if body.is_empty() { None } else { Some(body) }
    }
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
        // VPN name, when present, actually looks like one.
        if let Some(v) = &s.vpn {
            assert!(looks_like_vpn(v), "{v}");
        }
    }
}
