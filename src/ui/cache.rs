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

/// The version of a tile that depends on nothing but the palette — built once and valid until
/// the style changes under it (a fade ramp, a focus ring at a fixed size). Distinct from any
/// [`version`] output by the nudge there, which is what reserves this band.
pub fn static_version() -> u64 {
    crate::ui::theme::epoch()
}

/// Hashes a tile's dependencies into the value [`TileStore`] compares against.
///
/// Pass a tuple or a `#[derive(Hash)]` struct of everything the tile's pixels depend on.
/// Floats and `Instant`s are not `Hash` on purpose: a tile keyed on a clock would rebuild
/// every frame, which is the one thing this cache exists to prevent — animate by
/// compositing the tile differently instead.
pub fn version(key: &impl Hash) -> u64 {
    let mut h = FxHasher::default();
    // Every tile depends on the style whether it names it or not, so the epoch is mixed in
    // here rather than left to each key to remember (see `ui::theme::EPOCH`).
    crate::ui::theme::epoch().hash(&mut h);
    key.hash(&mut h);
    // `static_version` is reserved for "depends on nothing but the palette"; nudge the one key
    // that would collide with it so a real dependency is never mistaken for a build-once tile.
    match h.finish() {
        v if v == static_version() => v.wrapping_add(1),
        v => v,
    }
}

/// Hashes a key that is used as an *identity* rather than as a change detector — where two
/// different values colliding shows the wrong pixels rather than merely a stale tile.
///
/// `SipHash`, deliberately: [`version`]'s hasher trades collision resistance for speed, which is
/// the right trade when the answer only ever decides "rebuild or not" and the wrong answer costs
/// one redundant raster. [`crate::ui::text::TextCache`] is the one caller that cannot take that
/// trade — its key names the glyph run to draw.
pub fn identity(key: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

/// `FxHash`: multiply-xor, one 64-bit word at a time.
///
/// [`version`] runs dozens of times a frame, over whole `Settings` and `SettingsOverride`
/// structs in the modal keys — and `SipHash`'s per-call setup and finalization dominate at
/// those key sizes, on a CPU with no 64-bit multiplier to spare. Nothing here is persisted or
/// compared across processes, so the hash is free to change with the build.
#[derive(Default)]
struct FxHasher(u64);

/// The 64-bit fractional constant `FxHash` uses — an odd multiplier, so the map is a bijection.
const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Length first, and not for free: the tail of a partial word is zero-padded, and
        // `Hash for str` writes only a `0xff` terminator after the bytes — so without this,
        // "a" and "a\0" pad to the same word and hash identically. A tile keyed on a title
        // would then be reused for a different one.
        self.add(bytes.len() as u64);
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_ne_bytes(c.try_into().expect("chunks_exact(8)")));
        }
        let tail = chunks.remainder();
        if !tail.is_empty() {
            let mut buf = [0u8; 8];
            buf[..tail.len()].copy_from_slice(tail);
            self.add(u64::from_ne_bytes(buf));
        }
    }

    fn write_u8(&mut self, v: u8) {
        self.add(u64::from(v));
    }

    fn write_u16(&mut self, v: u16) {
        self.add(u64::from(v));
    }

    fn write_u32(&mut self, v: u32) {
        self.add(u64::from(v));
    }

    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }

    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
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

    /// [`ensure`](Self::ensure) for a tile with no dependencies of its own — rebuilt only when
    /// the style epoch moves (see [`static_version`]).
    pub fn ensure_static(&mut self, id: TileId, build: impl FnOnce() -> Result<Painter>) -> Result<bool> {
        self.ensure(id, static_version(), build)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_hashes_the_same_every_time() {
        let key = ("settings", 3u32, true, Some(7u64));
        assert_eq!(version(&key), version(&key));
    }

    #[test]
    fn keys_that_differ_in_one_field_get_different_versions() {
        assert_ne!(version(&("row", 1u32)), version(&("row", 2u32)));
        assert_ne!(version(&("row", 1u32)), version(&("rov", 1u32)));
        assert_ne!(version(&(1u8, 2u8)), version(&(2u8, 1u8)));
    }

    #[test]
    fn a_string_key_is_not_aliased_by_its_zero_padding() {
        // The tail of a partial word is zero-filled, so "a" and "a\0" would collide on the
        // word alone — `Hash for str` writing the length is what separates them.
        assert_ne!(version(&"a"), version(&"a\0"));
        assert_ne!(version(&"abcdefgh"), version(&"abcdefgh\0"));
    }

    #[test]
    fn no_real_key_is_mistaken_for_a_build_once_tile() {
        // `version` remaps the one input that would hash to `static_version`; nothing else may
        // return it. Hashes exactly as `version` does, style epoch included.
        let target = static_version();
        let colliding = (0..5000u32).find(|i| {
            let mut h = FxHasher::default();
            crate::ui::theme::epoch().hash(&mut h);
            i.hash(&mut h);
            h.finish() == target
        });
        assert!(colliding.is_none_or(|i| version(&i) != target));
        assert!((0..5000u32).all(|i| version(&i) != target));
    }
}
