//! `TileStore` — the rasterized-once tiles, and how staleness is decided.
//!
//! Every tile in this UI is built from some state and stays valid until that state moves:
//! the sidebar until its rows change, a settings row until the setting changes, a modal's
//! focused widget until focus or its value moves. That used to be sixteen hand-written
//! `Option<(Key, Painter)>` fields with sixteen hand-written `if key != stored` checks —
//! one per tile, each its own opportunity to forget a field of the key.
//!
//! Here it is one map and one rule: a tile is fresh while its [`version`] matches. The
//! version is a hash of whatever the app decides the tile depends on, so adding a
//! dependency is adding a field to a `#[derive(Hash)]` key rather than editing a
//! comparison. `ui` never sees the key itself, which is what keeps this app's `Settings`
//! and `Screen` out of the library.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use anyhow::Result;

use crate::ui::render::TileId;
use crate::ui::Painter;

/// The version of a tile that depends on nothing — built once, valid forever (a fade ramp,
/// a focus ring at a fixed size). Distinct from any [`version`] output only by convention;
/// nothing breaks if a real key happens to hash to it.
pub const STATIC: u64 = 0;

/// Hashes a tile's dependencies into the value [`TileStore`] compares against.
///
/// Pass a tuple or a `#[derive(Hash)]` struct of everything the tile's pixels depend on.
/// Floats and `Instant`s are not `Hash` on purpose: a tile keyed on a clock would rebuild
/// every frame, which is the one thing this cache exists to prevent — animate by
/// compositing the tile differently instead.
pub fn version(key: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    // `STATIC` is reserved for "depends on nothing"; nudge the one key that would collide
    // with it so a real dependency can never be mistaken for a build-once tile.
    match h.finish() {
        STATIC => 1,
        v => v,
    }
}

struct Entry {
    version: u64,
    painter: Painter,
}

/// Every tile the compositor has a texture for, and the version each was built at.
#[derive(Default)]
pub struct TileStore {
    tiles: HashMap<TileId, Entry>,
}

impl TileStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rasterized pixels for `id`, if it has been built.
    pub fn get(&self, id: TileId) -> Option<&Painter> {
        self.tiles.get(&id).map(|e| &e.painter)
    }

    pub fn contains(&self, id: TileId) -> bool {
        self.tiles.contains_key(&id)
    }

    /// Whether `id` is present and was built at `version`.
    pub fn is_fresh(&self, id: TileId, version: u64) -> bool {
        self.tiles.get(&id).is_some_and(|e| e.version == version)
    }

    /// Builds `id` if it is missing or was built at a different version, and reports
    /// whether it was (re)built — i.e. whether its GPU texture needs re-uploading.
    ///
    /// `build` is not called when the tile is already fresh, so the cost of a tile that
    /// does not change is one hash lookup.
    pub fn ensure(&mut self, id: TileId, version: u64, build: impl FnOnce() -> Result<Painter>) -> Result<bool> {
        if self.is_fresh(id, version) {
            return Ok(false);
        }
        let painter = build()?;
        self.tiles.insert(id, Entry { version, painter });
        Ok(true)
    }

    /// [`ensure`](Self::ensure) for a tile that is built once and never invalidated.
    pub fn ensure_static(&mut self, id: TileId, build: impl FnOnce() -> Result<Painter>) -> Result<bool> {
        if self.contains(id) {
            return Ok(false);
        }
        self.ensure(id, STATIC, build)
    }

    /// Reuses `id`'s existing pixmap as the scratch surface for its own rebuild — for a
    /// full-screen tile, where reallocating several MB per rebuild is the dominant cost.
    /// The painter is handed to `build` already sized; `build` is responsible for clearing
    /// whatever it does not overwrite.
    pub fn ensure_in_place(
        &mut self,
        id: TileId,
        version: u64,
        fresh: impl FnOnce() -> Painter,
        build: impl FnOnce(&mut Painter) -> Result<()>,
    ) -> Result<bool> {
        if self.is_fresh(id, version) {
            return Ok(false);
        }
        let mut painter = match self.tiles.remove(&id) {
            Some(e) => e.painter,
            None => fresh(),
        };
        build(&mut painter)?;
        self.tiles.insert(id, Entry { version, painter });
        Ok(true)
    }

    /// Stores an already-rasterized tile.
    pub fn put(&mut self, id: TileId, version: u64, painter: Painter) {
        self.tiles.insert(id, Entry { version, painter });
    }

    /// How many tiles are resident — the tile store is pruned only by its callers
    /// (the grid's eviction window), so this is the only account of its size.
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Bytes of rasterized pixels resident, at 4 bytes a pixel. The one family that scales
    /// with anything the user controls is the grid's cards, and those are held to the scroll
    /// window — this is what makes that claim checkable rather than argued (see G5).
    pub fn bytes(&self) -> usize {
        self.tiles
            .values()
            .map(|e| e.painter.width() as usize * e.painter.height() as usize * 4)
            .sum()
    }

    /// Drops `id`, reporting whether it was there. The GPU texture is the caller's to
    /// release (see `Compositor::drop_tile`).
    pub fn remove(&mut self, id: TileId) -> bool {
        self.tiles.remove(&id).is_some()
    }
}
