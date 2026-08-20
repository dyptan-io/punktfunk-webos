//! `TileStore` — the rasterized-once tiles, and how staleness is decided.
//!
//! Every tile in this UI is built from some state and stays valid until that state moves:
//! the sidebar until its rows change, a settings row until the setting changes, a modal's
//! focused widget until focus or its value moves. That used to be sixteen hand-written
//! `Option<(Key, Painter)>` fields with sixteen hand-written `if key != stored` checks —
//! one per tile, each its own opportunity to forget a field of the key.
//!
//! Here it is one slot table and one rule: a tile is fresh while its [`version`] matches. The
//! version is a hash of whatever the app decides the tile depends on, so adding a
//! dependency is adding a field to a `#[derive(Hash)]` key rather than editing a
//! comparison. `ui` never sees the key itself, which is what keeps this app's `Settings`
//! and `Screen` out of the library.
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
///
/// A slot vector rather than a map: [`TileId`]s are handed out in dense bands (see
/// `app::render::tile`), so indexing by `id.0` turns every lookup into a bounds check where
/// a `HashMap<TileId, _>` paid a `SipHash` over a `u32` plus a probe — once per visible card
/// and once per draw command, every frame.
#[derive(Default)]
pub struct TileStore {
    slots: Vec<Option<Entry>>,
}

impl TileStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self, id: TileId) -> Option<&Entry> {
        self.slots.get(id.0 as usize)?.as_ref()
    }

    fn store(&mut self, id: TileId, entry: Entry) {
        let idx = id.0 as usize;
        if idx >= self.slots.len() {
            self.slots.resize_with(idx + 1, || None);
        }
        self.slots[idx] = Some(entry);
    }

    /// The rasterized pixels for `id`, if it has been built.
    pub fn get(&self, id: TileId) -> Option<&Painter> {
        self.entry(id).map(|e| &e.painter)
    }

    pub fn contains(&self, id: TileId) -> bool {
        self.entry(id).is_some()
    }

    /// Whether `id` is present and was built at `version`.
    pub fn is_fresh(&self, id: TileId, version: u64) -> bool {
        self.entry(id).is_some_and(|e| e.version == version)
    }

    /// Builds `id` if it is missing or was built at a different version, and reports
    /// whether it was (re)built — i.e. whether its GPU texture needs re-uploading.
    ///
    /// `build` is not called when the tile is already fresh, so the cost of a tile that
    /// does not change is one slot lookup.
    pub fn ensure(&mut self, id: TileId, version: u64, build: impl FnOnce() -> Result<Painter>) -> Result<bool> {
        if self.is_fresh(id, version) {
            return Ok(false);
        }
        let painter = build()?;
        self.store(id, Entry { version, painter });
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
    /// tile whose reallocation per rebuild is the dominant cost (a full-screen layer, or a
    /// modal card rebuilt on every keystroke). The painter is handed to `build` already
    /// sized; `build` is responsible for clearing whatever it does not overwrite.
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
        let mut painter = match self.take(id) {
            Some(p) => p,
            None => fresh(),
        };
        build(&mut painter)?;
        self.store(id, Entry { version, painter });
        Ok(true)
    }

    /// Stores an already-rasterized tile.
    pub fn put(&mut self, id: TileId, version: u64, painter: Painter) {
        self.store(id, Entry { version, painter });
    }

    /// How many tiles are resident — the tile store is pruned only by its callers
    /// (the grid's eviction window), so this is the only account of its size.
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Bytes of rasterized pixels resident, at 4 bytes a pixel. The one family that scales
    /// with anything the user controls is the grid's cards, and those are held to the scroll
    /// window — this is what makes that claim checkable rather than argued (see G5).
    pub fn bytes(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .map(|e| e.painter.width() as usize * e.painter.height() as usize * 4)
            .sum()
    }

    /// Drops `id`, reporting whether it was there. The GPU texture is the caller's to
    /// release (see `Compositor::drop_tile`).
    pub fn remove(&mut self, id: TileId) -> bool {
        self.take(id).is_some()
    }

    /// Drops `id` but hands back its pixmap, for a caller that will immediately need another
    /// of the same size — the grid, which evicts and builds card tiles in the same frame
    /// during a scroll (see `app::render::prepare::prepare_grid`). The GPU texture is still
    /// the caller's to release, exactly as with [`remove`](Self::remove).
    pub fn take(&mut self, id: TileId) -> Option<Painter> {
        self.slots.get_mut(id.0 as usize)?.take().map(|e| e.painter)
    }
}
