//! The rasterized-text cache's key hash (see `ui::text`).
use std::hash::{Hash, Hasher};

/// A stable 64-bit identity for `key`, for the text cache's map.
pub fn identity(key: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}
