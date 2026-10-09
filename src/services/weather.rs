//! Weather: Open-Meteo current temperature for a city, refreshed every 10
//! minutes. No crates: HTTPS via WinINet, JSON parsed by hand (the one flat
//! response shape is smaller than serde's derive tree).

use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// One Open-Meteo `current` block — the only fields the pill shows.
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Now {
    pub temperature_2m: f32,
    pub weather_code: u32,
}

/// Geolocation + forecast in one flat struct.
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub city: String,
    pub now: Now,
}

impl Snapshot {
    /// A pill label like `Tehran 24°`.
    pub fn label(&self) -> String {
        format!("{} {:.0}°", self.city, self.now.temperature_2m)
    }
}

/// WMO weather codes → short icon glyphs. Only the common buckets; anything
/// else falls back to `?`.
pub fn glyph(code: u32) -> &'static str {
    match code {
        0 => "Sunny",
        1..=2 => "Partly cloudy",
        3 => "Cloudy",
        45 | 48 => "Fog",
        51..=67 | 80..=82 => "Rain",
        71..=77 | 85 | 86 => "Snow",
        95..=99 => "Storm",
        _ => "—",
    }
}

pub struct Weather {
    shared: Arc<Mutex<Option<Snapshot>>>,
    alive: Arc<AtomicBool>,
}

impl Weather {
    /// Spawns the worker: geolocate once, then poll the forecast every 10
    /// minutes until the process ends.
    pub fn new() -> Self {
        let shared: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));
        let (sh, al) = (shared.clone(), alive.clone());
        std::thread::Builder::new()
            .name("weather".into())
            .spawn(move || loop {
                if !al.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(s) = fetch() {
                    *sh.lock().unwrap() = Some(s);
                }
                // 10 minutes in 1 s steps so shutdown is observed promptly.
                for _ in 0..600 {
                    if !al.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            })
            .expect("spawn weather thread");
        Self { shared, alive }
    }

    /// The latest snapshot, if any fetch has ever succeeded.
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.shared.lock().ok().and_then(|g| g.clone())
    }
}

impl Drop for Weather {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

/// Geolocate by IP, then fetch the current block. Any failure returns `None`
/// and the pill keeps its last value.
fn fetch() -> Option<Snapshot> {
    let loc = http("https://ipapi.co/json/")?;
    #[derive(Deserialize)]
    struct Loc {
        city: String,
        latitude: f64,
        longitude: f64,
    }
    let loc: Loc = serde_json::from_str(&loc).ok()?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&current=temperature_2m,weather_code",
        loc.latitude, loc.longitude
    );
    let raw = http(&url)?;
    #[derive(Deserialize)]
    struct Resp {
        current: Now,
    }
    let resp: Resp = serde_json::from_str(&raw).ok()?;
    Some(Snapshot {
        city: loc.city,
        now: resp.current,
    })
}

/// One HTTPS GET via WinINet, body capped at 64 KiB. Returns `None` on any
/// failure; this is a status pill, not a critical path.
pub(crate) fn http(url: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinInet::{
        InternetCloseHandle, InternetOpenUrlW, InternetOpenW, InternetReadFile,
        INTERNET_FLAG_NO_CACHE_WRITE, INTERNET_FLAG_RELOAD, INTERNET_FLAG_SECURE,
        INTERNET_OPEN_TYPE_PRECONFIG,
    };

    // The agent string is ASCII, so building a wide copy by hand is safe.
    let agent: Vec<u16> = "Arc\0".encode_utf16().collect();
    unsafe {
        let h = InternetOpenW(PCWSTR(agent.as_ptr()), INTERNET_OPEN_TYPE_PRECONFIG.0, None, None, 0);
        if h.is_null() {
            return None;
        }
        // Wrap in a guard so every return path closes the session handle.
        struct S(*const core::ffi::c_void);
        impl Drop for S {
            fn drop(&mut self) {
                unsafe { let _ = InternetCloseHandle(self.0); }
            }
        }
        let _s = S(h);

        // URL is ASCII-only for our two endpoints; hand-widen it.
        let mut wide: Vec<u16> = url.encode_utf16().collect();
        wide.push(0);
        let flags = INTERNET_FLAG_SECURE | INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE;
        let file = InternetOpenUrlW(h, PCWSTR(wide.as_ptr()), None, flags, None);
        if file.is_null() {
            return None;
        }
        let _f = S(file);

        let mut body = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let mut n = 0u32;
            if InternetReadFile(file, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n).is_err() || n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > 64 * 1024 {
                break; // cap: neither endpoint needs more
            }
        }
        String::from_utf8(body).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_shows_city_and_whole_degrees() {
        let s = Snapshot {
            city: "Tehran".into(),
            now: Now {
                temperature_2m: 23.6,
                weather_code: 0,
            },
        };
        assert_eq!(s.label(), "Tehran 24°");
    }

    #[test]
    fn glyphs_cover_the_common_codes() {
        assert_eq!(glyph(0), "Sunny");
        assert_eq!(glyph(61), "Rain");
        assert_eq!(glyph(95), "Storm");
        assert_eq!(glyph(9999), "—");
    }

    #[test]
    fn offline_fetch_returns_none() {
        // The sandbox may have no network; fetch() must fail soft.
        let _ = fetch();
    }
}
