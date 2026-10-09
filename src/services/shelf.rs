//! Shelf: pinned file shortcuts, dropped onto the island and persisted.
//!
//! A shelf item is a path. Pinning one persists its path in the shared
//! settings store; double-click opens it with `ShellExecuteW`. There is no
//! copy of the file and no reference to a window — the path is the whole
//! item, so a file the user moves breaks the pin, and that is the honest
//! behaviour for a shortcut.
//!
//! CUT: `IDropTarget` / OLE drag-drop. `WM_DROPFILES` from `DragAcceptFiles`
//! delivers the same paths with zero COM interfaces and no vtable boilerplate.
//! The trade is that it is legacy: no drag-out (an app cannot accept its own
//! drags either way here) and no rich formats. Neither is on the Phase 6
//! done-check, so the smaller code wins.
//!
//! CUT: `SHGetFileInfo` thumbnails for non-image files. Getting an icon out
//! of one means an HICON → PNG encode path that nothing else in Arc needs;
//! WIC already decodes image *bytes*, so images thumb from their own file and
//! everything else gets a glyph. Add the icon path when a pinned folder or
//! document needs to be recognizable at a glance.

use std::path::Path;

use crate::core::scene::ImageHandle;
use crate::services::settings;

/// Extensions WIC can decode. Anything else draws a glyph instead of a
/// thumbnail, which is the whole point of not calling `SHGetFileInfo`.
const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"];

/// One pinned item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    /// Image handle for the thumbnail, or [`ImageHandle::default`] when the
    /// file is not an image WIC can read.
    pub thumb: ImageHandle,
}

impl Item {
    /// The file's own name, for the label under a thumbnail.
    pub fn label(&self) -> String {
        Path::new(&self.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
    }

    /// True when the pinned file is still where it was.
    pub fn exists(&self) -> bool {
        Path::new(&self.path).exists()
    }
}

/// Does this path look like something WIC can decode?
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            IMAGE_EXT.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Build one item: intern the file's bytes for a thumbnail when it is an
/// image, otherwise leave the handle null and draw a glyph.
pub fn item_for(path: &Path) -> Item {
    let thumb = if is_image(path) {
        std::fs::read(path)
            .map(|b| crate::core::imagedb::intern(&b))
            .unwrap_or_default()
    } else {
        ImageHandle::default()
    };
    Item {
        path: path.to_string_lossy().to_string(),
        thumb,
    }
}

/// Open a path with the user's default handler. Best effort: ShellExecuteW
/// returning anything under 32 is failure.
pub fn open(path: &Path) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use std::os::windows::ffi::OsStrExt;
    // ShellExecuteW wants null-terminated wide strings.
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both strings are null-terminated and live for the call.
    let r = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    r.0 as isize > 32
}

/// The pinned shelf, loaded from settings and persisted on every change.
pub struct Shelf {
    items: Vec<Item>,
}

impl Shelf {
    pub fn load() -> Self {
        let subs = settings::load();
        let items = subs
            .get("shelf")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|p| item_for(Path::new(p)))
                    .collect()
            })
            .unwrap_or_default();
        Self { items }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Pin a path. Duplicate paths are ignored so a repeated drop of the same
    /// file cannot grow the shelf.
    pub fn pin(&mut self, path: &Path) -> bool {
        let s = path.to_string_lossy().to_string();
        if self.items.iter().any(|i| i.path == s) {
            return false;
        }
        self.items.push(item_for(path));
        self.save();
        true
    }

    /// Unpin by path.
    pub fn unpin(&mut self, path: &str) {
        self.items.retain(|i| i.path != path);
        self.save();
    }

    fn save(&self) {
        let paths: Vec<&str> = self.items.iter().map(|i| i.path.as_str()).collect();
        // Read-modify-write: the same file holds the clipboard history and the
        // calendar subscription list, so the load/save pair is load-then-save
        // rather than a whole-file constructor.
        let mut t = settings::load();
        t.insert(
            "shelf".into(),
            toml::Value::Array(paths.into_iter().map(|p| p.into()).collect()),
        );
        settings::save(&t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions_are_recognised_case_insensitively() {
        assert!(is_image(Path::new("a/b.PNG")));
        assert!(is_image(Path::new("x.jpeg")));
        assert!(!is_image(Path::new("notes.txt")));
        assert!(!is_image(Path::new("noext")));
    }

    #[test]
    fn label_is_the_file_name_only() {
        let it = Item {
            path: r"C:\Users\x\Downloads\report.pdf".into(),
            thumb: ImageHandle::default(),
        };
        assert_eq!(it.label(), "report.pdf");
    }

    #[test]
    fn a_missing_file_reports_gone() {
        let it = Item {
            path: r"Q:\definitely\not\here.zzz".into(),
            thumb: ImageHandle::default(),
        };
        assert!(!it.exists());
    }

    #[test]
    fn non_image_items_get_no_thumbnail() {
        let it = item_for(Path::new(r"Q:\nope.txt"));
        assert_eq!(it.thumb, ImageHandle::default());
    }

    #[test]
    fn opening_a_missing_file_fails_rather_than_panics() {
        assert!(!open(Path::new(r"Q:\no\such\file.zzz")));
    }
}