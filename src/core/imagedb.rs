//! Raw image registry keyed by content hash.
//!
//! `ui` stays pure and platform-free: it interns artwork bytes here and gets an
//! [`ImageHandle`]; the renderer decodes on demand and caches the GPU bitmap
//! under the same handle. Handle 0 stays "nothing decoded" by contract.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::scene::ImageHandle;

fn store() -> &'static Mutex<HashMap<u64, Vec<u8>>> {
    static STORE: OnceLock<Mutex<HashMap<u64, Vec<u8>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    // Content-addressed: re-interning the same artwork returns the same handle,
    // so one frame's decode cache serves every later frame.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish() | 1 // never 0; 0 means "nothing decoded yet"
}

/// Intern raw encoded image bytes, returning the handle to draw with.
#[must_use]
pub fn intern(bytes: &[u8]) -> ImageHandle {
    if bytes.is_empty() {
        return ImageHandle(0);
    }
    let key = hash(bytes);
    let mut map = store().lock().expect("image registry poisoned");
    map.entry(key).or_insert_with(|| bytes.to_vec());
    ImageHandle(key)
}

/// Look up the raw bytes a handle was interned from.
#[must_use]
pub fn bytes(handle: ImageHandle) -> Option<Vec<u8>> {
    if handle.0 == 0 {
        return None;
    }
    store().lock().expect("image registry poisoned").get(&handle.0).cloned()
}

/// Registry entry count, for tests.
#[cfg(test)]
pub fn len() -> usize {
    store().lock().expect("image registry poisoned").len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytes_give_the_null_handle() {
        assert_eq!(intern(&[]), ImageHandle(0));
        assert!(bytes(ImageHandle(0)).is_none());
    }

    #[test]
    fn same_content_same_handle() {
        let a = intern(b"png-bytes-here");
        let b = intern(b"png-bytes-here");
        assert_eq!(a, b);
        assert_ne!(a, ImageHandle(0));
        assert_eq!(bytes(a).as_deref(), Some(&b"png-bytes-here"[..]));
    }

    #[test]
    fn different_content_different_handle() {
        assert_ne!(intern(b"art-a"), intern(b"art-b"));
    }

    #[test]
    fn unknown_handle_reads_none() {
        assert!(bytes(ImageHandle(0xdead_beef)).is_none());
    }
}
