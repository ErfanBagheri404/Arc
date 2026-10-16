//! Pet: the idle-notch mascot, in the Hermes `petdex` spritesheet format.
//!
//! A pet is a spritesheet plus a `pet.json` manifest. Native frames are
//! 192x208 cells, row 0 is the idle row, and animation steps through the first
//! six cells on an 1100 ms loop. This module only *resolves* the pet and times
//! its frames; the renderer interns the sheet bytes and crops the current
//! frame with `DrawBitmap`'s source rect, so nothing is re-encoded here and
//! nothing is decoded per frame.
//!
//! Degrade, never fail: no pet installed, a broken manifest, or a missing
//! sheet means `pet()` returns `None` and the pill keeps its resting glyph.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;

use crate::core::scene::ImageHandle;

/// Native cell size of a petdex spritesheet.
pub const CELL_W: f32 = 192.0;
pub const CELL_H: f32 = 208.0;
/// Frames the idle row steps through (petdex CSS `steps(6)`).
pub const IDLE_FRAMES: u32 = 6;
/// Full idle-loop duration, ms (the petdex default).
pub const LOOP_MS: u64 = 1100;
/// Idle row index in both the legacy and the Codex atlas taxonomy.
pub const IDLE_ROW: u32 = 0;

/// What the pill draws: the interned sheet, the frame to crop, and the slug
/// it came from (Settings mirrors the selection from it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PetFrame {
    pub handle: ImageHandle,
    pub frame: u32,
}

/// A resolved pet: display name, slug, and the interned sheet handle.
#[derive(Debug, Clone)]
pub struct Pet {
    pub name: String,
    pub slug: String,
    pub handle: ImageHandle,
}

/// The loaded pet plus the clock its frames run against.
#[derive(Debug)]
pub struct PetState {
    pet: Option<Pet>,
    epoch: Instant,
}

impl Default for PetState {
    fn default() -> Self {
        Self {
            pet: None,
            epoch: Instant::now(),
        }
    }
}

impl PetState {
    /// Load the configured pet (empty slug = first installed). A reload of the
    /// same slug is a no-op; a different one re-reads and re-interns. Failure
    /// clears the pet — the pill falls back to its resting glyph rather than
    /// showing a stale mascot.
    pub fn reload(&mut self, slug: &str) {
        if self.pet.as_ref().is_some_and(|p| p.slug == slug) {
            return;
        }
        self.pet = load(slug);
        self.epoch = Instant::now();
    }

    /// The slug currently shown, or `None` when no pet is loaded.
    #[must_use]
    pub fn slug(&self) -> Option<&str> {
        self.pet.as_ref().map(|p| p.slug.as_str())
    }

    /// The display name of the loaded pet, for the Settings picker row.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.pet.as_ref().map(|p| p.name.as_str())
    }

    /// The frame to draw at `now`, or `None` when no pet is loaded.
    #[must_use]
    pub fn frame_at(&self, now: Instant) -> Option<PetFrame> {
        let pet = self.pet.as_ref()?;
        Some(PetFrame {
            handle: pet.handle,
            frame: frame_at(now, self.epoch),
        })
    }
}

/// Which idle frame (0..6) is up at `now`, stepping on the petdex 1100 ms loop.
#[must_use]
pub fn frame_at(now: Instant, epoch: Instant) -> u32 {
    let ms = now.saturating_duration_since(epoch).as_millis() as u64;
    ((ms % LOOP_MS) * IDLE_FRAMES as u64 / LOOP_MS) as u32 % IDLE_FRAMES
}

/// How long one frame is held — the app paces its idle repaints by this
/// instead of spinning at full rate while the pet animates.
#[allow(dead_code)]
#[must_use]
pub fn frame_period() -> Duration {
    Duration::from_millis(LOOP_MS / IDLE_FRAMES as u64)
}

/// The sub-rect of the sheet that holds idle frame `index`, as
/// `(x, y, w, h)` in native pixels.
#[must_use]
pub fn frame_rect(index: u32) -> (f32, f32, f32, f32) {
    (
        index.min(IDLE_FRAMES - 1) as f32 * CELL_W,
        IDLE_ROW as f32 * CELL_H,
        CELL_W,
        CELL_H,
    )
}

/// Hermes pet installs, profile-aware: `<HERMES_HOME>/pets/<slug>/`.
fn pets_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HERMES_HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home).join("pets"));
        }
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| {
            PathBuf::from(h)
                .join("AppData")
                .join("Local")
                .join("hermes")
                .join("pets")
        })
}

/// Installed pet slugs, sorted — for the Settings surface.
#[must_use]
pub fn installed() -> Vec<String> {
    let Some(dir) = pets_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("pet.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    out.sort();
    out
}

/// Gallery sponsor: the public petdex manifest URL.
pub const MANIFEST_URL: &str = "https://petdex.dev/api/manifest";

#[derive(Debug, Clone, Deserialize)]
struct CatalogEntryRaw {
    #[serde(default)]
    slug: Option<String>,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(rename = "spritesheetUrl", default)]
    spritesheet_url: Option<String>,
    #[serde(rename = "petJsonUrl", default)]
    pet_json_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestRaw {
    #[serde(default)]
    pets: Option<Vec<CatalogEntryRaw>>,
}

#[derive(Debug, Clone, Default)]
pub struct Tile {
    pub slug: String,
    pub name: String,
    pub installed: bool,
    pub thumb: Option<ImageHandle>,
    pub busy: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GallerySnapshot {
    pub loading: bool,
    pub tiles: Vec<Tile>,
    pub equipped: String,
    pub version: u64,
}

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub slug: String,
    pub name: String,
    pub spritesheet_url: String,
    pub pet_json_url: String,
}

#[derive(Clone)]
enum Cmd {
    Thumb(String),
    Install(String),
    Uninstall(String),
}

#[derive(Default)]
struct GalleryState {
    catalog: Vec<CatalogEntry>,
    thumbs: std::collections::HashMap<String, Option<ImageHandle>>,
    tiles: Vec<Tile>,
    equipped: String,
    refresh_at: Option<Instant>,
    last_attempt: Option<Instant>,
    busy: Option<String>,
    version: u64,
}

impl GalleryState {
    fn rebuild(&mut self) {
        use std::collections::{BTreeMap, HashSet};
        let installed_slugs = installed();
        let installed_set: HashSet<String> = installed_slugs.iter().cloned().collect();
        let mut map: BTreeMap<String, Tile> = BTreeMap::new();
        for slug in &installed_slugs {
            let name = display_name(slug).unwrap_or_else(|| slug.clone());
            let thumb = self.thumbs.get(slug).copied().flatten().or_else(|| {
                let dir = pets_dir()?.join(slug);
                let manifest = std::fs::read_to_string(dir.join("pet.json")).ok()?;
                let sp = field(&manifest, "spritesheetPath").unwrap_or_else(|| "spritesheet.webp".into());
                let bytes = std::fs::read(dir.join(sp)).ok()?;
                let h = crate::core::imagedb::intern(&bytes);
                if h == ImageHandle(0) { None } else { Some(h) }
            });
            if thumb.is_some() {
                self.thumbs.insert(slug.clone(), thumb);
            }
            map.insert(
                slug.clone(),
                Tile { slug: slug.clone(), name, installed: true, thumb, busy: false, failed: false },
            );
        }
        for entry in &self.catalog {
            let installed = installed_set.contains(&entry.slug);
            if let Some(tile) = map.get_mut(&entry.slug) {
                if (tile.name.is_empty() || tile.name == tile.slug) && !entry.name.is_empty() {
                    tile.name = entry.name.clone();
                }
            } else {
                let name = if entry.name.is_empty() {
                    entry.slug.clone()
                } else {
                    entry.name.clone()
                };
                map.insert(
                    entry.slug.clone(),
                    Tile {
                        slug: entry.slug.clone(),
                        name,
                        installed,
                        thumb: self.thumbs.get(&entry.slug).copied().flatten(),
                        busy: false,
                        failed: false,
                    },
                );
            }
        }
        let mut tiles: Vec<Tile> = map.into_values().collect();
        let equipped = self.equipped.clone();
        tiles.sort_by(|a, b| {
            let rank = |t: &Tile| -> u8 {
                if t.slug == equipped {
                    0
                } else if t.installed {
                    1
                } else {
                    2
                }
            };
            match rank(a).cmp(&rank(b)) {
                std::cmp::Ordering::Equal => a.name.cmp(&b.name),
                o => o,
            }
        });
        self.tiles = tiles;
    }
}

fn is_petdex_host(url: &str) -> bool {
    // Host-only comparison: parse the host part, allow petdex.dev + subdomains.
    let path_less = url.split("//").nth(1).unwrap_or("");
    let host = path_less.split('/').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or(host);
    let host = host.to_ascii_lowercase();
    host == "petdex.dev" || host.ends_with(".petdex.dev")
}

pub struct Gallery {
    inner: std::sync::Arc<Mutex<GalleryState>>,
    queue: std::sync::Arc<Mutex<Vec<Cmd>>>,
}

static GALLERY: Mutex<Option<std::sync::Arc<Gallery>>> =
    Mutex::new(None);

pub fn gallery() -> std::sync::Arc<Gallery> {
    let mut g = GALLERY.lock();
    if g.is_some() {
        return g.as_ref().unwrap().clone();
    }
    let inner = std::sync::Arc::new(Mutex::new(GalleryState::default()));
    let queue = std::sync::Arc::new(Mutex::new(Vec::new()));
    let gal = std::sync::Arc::new(Gallery { inner: inner.clone(), queue: queue.clone() });
    // start worker
    let (i, q) = (inner.clone(), queue.clone());
    std::thread::Builder::new()
        .name("pet-gallery".into())
        .spawn(move || worker(i, q))
        .ok();
    *g = Some(gal.clone());
    gal
}

fn worker(inner: std::sync::Arc<Mutex<GalleryState>>, queue: std::sync::Arc<Mutex<Vec<Cmd>>>) {
    loop {
        let cmds: Vec<Cmd> = {
            let mut q = queue.lock();
            std::mem::take(&mut *q)
        };
        let mut worked = false;
        // Refresh if stale (never fetched, or >5 min).
        let want_refresh = {
            let s = inner.lock();
            s.refresh_at.map(|t| t.elapsed().as_secs() >= 300).unwrap_or(true)
                && s.last_attempt.map(|t| t.elapsed().as_secs() >= 60).unwrap_or(true)
        };
        if want_refresh {
            let url = MANIFEST_URL;
            let now = Instant::now();
            inner.lock().last_attempt = Some(now);
            if let Some(bytes) = crate::services::net::get_bytes(url, 2 * 1024 * 1024) {
                if let Some(text) = String::from_utf8(bytes).ok() {
                    if let Ok(doc) = serde_json::from_str::<ManifestRaw>(&text) {
                        let mut catalog: Vec<CatalogEntry> = Vec::new();
                        for e in doc.pets.unwrap_or_default() {
                            let Some(slug) = e.slug else { continue; };
                            let slug = slug.trim().to_string();
                            if slug.is_empty() {
                                continue;
                            }
                            catalog.push(CatalogEntry {
                                slug,
                                name: e.display_name.unwrap_or_default(),
                                spritesheet_url: e.spritesheet_url.unwrap_or_default(),
                                pet_json_url: e.pet_json_url.unwrap_or_default(),
                            });
                        }
                        let mut s = inner.lock();
                        s.catalog = catalog;
                        s.refresh_at = Some(Instant::now());
                        s.rebuild();
                        s.version += 1;
                        worked = true;
                    }
                }
            }
        }
        let was_empty = cmds.is_empty();
        for cmd in cmds {
            worked = true;
            match cmd {
                Cmd::Thumb(slug) => {
                    if inner.lock().thumbs.contains_key(&slug) {
                        continue;
                    }
                    let url = inner
                        .lock()
                        .catalog
                        .iter()
                        .find(|e| e.slug == slug)
                        .map(|e| e.spritesheet_url.clone())
                        .unwrap_or_default();
                    if !url.is_empty() && is_petdex_host(&url) {
                        if let Some(bytes) = crate::services::net::get_bytes(&url, 8 * 1024 * 1024) {
                            let h = crate::core::imagedb::intern(&bytes);
                            let thumb = if h == ImageHandle(0) { None } else { Some(h) };
                            let mut s = inner.lock();
                            s.thumbs.insert(slug.clone(), thumb);
                            s.rebuild();
                            s.version += 1;
                            continue;
                        }
                    }
                    let mut s = inner.lock();
                    s.thumbs.insert(slug.clone(), None);
                    s.rebuild();
                    s.version += 1;
                }
                Cmd::Install(slug) => {
                    let entry = inner.lock().catalog.iter().find(|e| e.slug == slug).cloned();
                    let Some(entry) = entry else {
                        let mut s = inner.lock();
                        s.busy = None;
                        s.version += 1;
                        continue;
                    };
                    if !is_petdex_host(&entry.spritesheet_url) {
                        let mut s = inner.lock();
                        s.busy = None;
                        s.version += 1;
                        continue;
                    }
                    if let Some(bytes) = crate::services::net::get_bytes(&entry.spritesheet_url, 16 * 1024 * 1024) {
                        let ext = if entry.spritesheet_url.split('?').next().unwrap_or("").ends_with(".png") {
                            ".png"
                        } else {
                            ".webp"
                        };
                        let sheet_name = format!("spritesheet{ext}");
                        let dir = match pets_dir() {
                            Some(d) => d.join(&slug),
                            None => {
                                inner.lock().busy = None;
                                continue;
                            }
                        };
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::fs::write(dir.join(&sheet_name), &bytes);
                        // pet.json
                        let meta = if entry.pet_json_url.is_empty() || !is_petdex_host(&entry.pet_json_url) {
                            serde_json::json!({
                                "id": slug,
                                "displayName": entry.name,
                                "spritesheetPath": sheet_name,
                            })
                            .to_string()
                        } else if let Some(pb) = crate::services::net::get_bytes(&entry.pet_json_url, 64 * 1024) {
                            let s = String::from_utf8_lossy(&pb).to_string();
                            if serde_json::from_str::<serde_json::Value>(&s).is_ok() {
                                s
                            } else {
                                serde_json::json!({
                                    "id": slug,
                                    "displayName": entry.name,
                                    "spritesheetPath": sheet_name,
                                })
                                .to_string()
                            }
                        } else {
                            serde_json::json!({
                                "id": slug,
                                "displayName": entry.name,
                                "spritesheetPath": sheet_name,
                            })
                            .to_string()
                        };
                        let _ = std::fs::write(dir.join("pet.json"), meta.as_bytes());
                        let h = crate::core::imagedb::intern(&bytes);
                        let thumb = if h == ImageHandle(0) { None } else { Some(h) };
                        let mut s = inner.lock();
                        s.thumbs.insert(slug.clone(), thumb);
                        s.busy = None;
                        s.rebuild();
                        s.version += 1;
                    } else {
                        let mut s = inner.lock();
                        s.busy = None;
                        s.version += 1;
                    }
                }
                Cmd::Uninstall(slug) => {
                    if let Some(dir) = pets_dir() {
                        let _ = std::fs::remove_dir_all(dir.join(&slug));
                    }
                    let mut s = inner.lock();
                    s.thumbs.remove(&slug);
                    s.busy = None;
                    s.rebuild();
                    s.version += 1;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(if was_empty && !worked { 1000 } else { 100 }));
    }
}

impl Gallery {
    pub fn snapshot(&self) -> GallerySnapshot {
        let s = self.inner.lock();
        GallerySnapshot {
            loading: s.refresh_at.is_none(),
            tiles: s.tiles.clone(),
            equipped: s.equipped.clone(),
            version: s.version,
        }
    }
    pub fn set_equipped(&self, slug: &str) {
        let mut s = self.inner.lock();
        if s.equipped != slug {
            s.equipped = slug.to_string();
            s.rebuild();
            s.version += 1;
        }
    }
    pub fn request_thumbs(&self, slugs: &[String]) {
        let mut q = self.queue.lock();
        for s in slugs {
            if !self.inner.lock().thumbs.contains_key(s)
                && !q.iter().any(|c| matches!(c, Cmd::Thumb(x) if x == s))
            {
                q.push(Cmd::Thumb(s.clone()));
            }
        }
    }
    pub fn request_install(&self, slug: &str) {
        self.queue.lock().push(Cmd::Install(slug.to_string()));
        let mut s = self.inner.lock();
        if let Some(t) = s.tiles.iter_mut().find(|t| t.slug == slug) {
            t.busy = true;
        }
        s.busy = Some(slug.to_string());
        s.version += 1;
    }
    pub fn request_uninstall(&self, slug: &str) {
        self.queue.lock().push(Cmd::Uninstall(slug.to_string()));
    }
}

pub fn snapshot() -> GallerySnapshot {
    gallery().snapshot()
}
pub fn set_equipped(slug: &str) {
    gallery().set_equipped(slug);
}
pub fn request_thumbs(slugs: &[String]) {
    gallery().request_thumbs(slugs);
}
pub fn request_install(slug: &str) {
    gallery().request_install(slug);
}
pub fn request_uninstall(slug: &str) {
    gallery().request_uninstall(slug);
}
pub fn display_name(slug: &str) -> Option<String> {
    let dir = pets_dir()?;
    let manifest = std::fs::read_to_string(dir.join(slug).join("pet.json")).ok()?;
    field(&manifest, "displayName").or_else(|| field(&manifest, "id"))
}

fn load(slug: &str) -> Option<Pet> {
    let dir = pets_dir()?;
    let pet_dir = if slug.is_empty() {
        dir.join(installed().into_iter().next()?)
    } else {
        dir.join(slug)
    };
    let manifest = std::fs::read_to_string(pet_dir.join("pet.json")).ok()?;
    let sheet_path = pet_dir
        .join(field(&manifest, "spritesheetPath").unwrap_or_else(|| "spritesheet.webp".into()));
    let bytes = std::fs::read(&sheet_path).ok()?;
    let handle = crate::core::imagedb::intern(&bytes);
    if handle == ImageHandle(0) {
        return None;
    }
    let name = field(&manifest, "displayName")
        .or_else(|| field(&manifest, "id"))
        .unwrap_or_else(|| {
            pet_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("pet")
                .to_string()
        });
    let slug = pet_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("pet")
        .to_string();
    Some(Pet { name, slug, handle })
}

/// Pull one `"key": "value"` string out of a `pet.json` blob. A hand-rolled
/// reader keeps `serde_json` out of the hot path for two fields.
fn field(manifest: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = manifest.find(&needle)?;
    let rest = manifest[at + needle.len()..]
        .split_once(':')?
        .1
        .trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
fn idle_loop_steps_six_frames_evenly() {
        let e = Instant::now();
        let at = |ms: u64| frame_at(e + Duration::from_millis(ms), e);
        assert_eq!(at(0), 0);
        assert_eq!(at(LOOP_MS / 2), 3);
        assert_eq!(at(LOOP_MS - 1), 5);
        assert_eq!(at(LOOP_MS), 0, "the loop wraps");
    }

    #[test]
    fn frame_period_is_sub_frame_length() {
        assert!(frame_period() <= Duration::from_millis(LOOP_MS));
        assert!(frame_period() >= Duration::from_millis(1));
    }

    #[test]
    fn frame_rects_tile_the_idle_row() {
        assert_eq!(frame_rect(0), (0.0, 0.0, CELL_W, CELL_H));
        assert_eq!(frame_rect(3), (3.0 * CELL_W, 0.0, CELL_W, CELL_H));
        // Out-of-range clamps to the last real frame rather than leaving the row.
        assert_eq!(frame_rect(99), frame_rect(IDLE_FRAMES - 1));
    }

    #[test]
    fn manifest_fields_parse_without_json() {
        let m = r#"{"id": "nukey", "displayName": "Nukey", "spritesheetPath": "s.webp"}"#;
        assert_eq!(field(m, "displayName").as_deref(), Some("Nukey"));
        assert_eq!(field(m, "spritesheetPath").as_deref(), Some("s.webp"));
        assert_eq!(field(m, "missing"), None);
        assert_eq!(field(r#"{"id":"x"}"#, "id").as_deref(), Some("x"));
    }

    #[test]
    fn a_missing_home_resolves_no_pet() {
        std::env::set_var("HERMES_HOME", "C:\\nonexistent-hermes-home-xyz");
        let mut s = PetState::default();
        s.reload("anything");
        assert!(s.frame_at(Instant::now()).is_none());
        assert!(installed().is_empty());
        std::env::remove_var("HERMES_HOME");
    }
}
