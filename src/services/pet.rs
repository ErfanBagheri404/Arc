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
