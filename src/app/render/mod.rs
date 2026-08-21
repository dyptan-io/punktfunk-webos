//! The render path's own vocabulary: which tile is which ([`tile`]), and what each one's
//! pixels depend on ([`key`]).
//!
//! Both were in `ui` — the tile enum in `ui::render`, the staleness keys in `ui::tiles` —
//! where they named this app's screens and its `Settings`. `ui` now sees an opaque
//! [`TileId`](crate::ui::render::TileId) and an opaque `u64` version, and everything that
//! gives those meaning lives here.

use crate::ui::render::Rect;

/// Where a left modal's card was, and which slice of its scrolled body was showing. The
/// pixels are cloned into [`tile::MODAL_PREV`]/[`tile::MODAL_PREV_CONTENT`], leaving the
/// entering modal free to rebuild [`tile::MODAL`] in the same frame — hence the cross-fade.
#[derive(Clone, Copy)]
pub(crate) struct ModalSnapshot {
    pub region: Rect,
    /// The scrolled body under that card, frozen as it was drawn — `None` for a modal whose
    /// body is baked into its own shell (every list modal and confirm dialog).
    pub content: Option<SnapshotBody>,
}

impl ModalSnapshot {
    /// Whether this fade still needs the settings row band — the snapshot says what it wants
    /// kept, rather than the eviction rule reading its variant.
    pub fn holds_settings_rows(&self) -> bool {
        matches!(self.content, Some(SnapshotBody::Rows(..)))
    }
}

/// How a left modal's body is redrawn while it fades.
///
/// Two shapes because the live compose path has two, and the fading copy goes through those
/// same two rather than a form of its own — see `compose_modal`.
#[derive(Clone, Copy)]
pub(crate) enum SnapshotBody {
    /// `(src crop, dst rect)` of [`tile::MODAL_PREV_CONTENT`], a clone of the leaving
    /// screen's single body tile.
    Cropped(Rect, Rect),
    /// Settings' row count, viewport, and the scroll offset frozen at the frame it was left.
    /// Nothing is copied: the band outlives the screen (see `prepare_scroll`'s eviction), so
    /// the fade-out is the same handful of blits the live screen was.
    Rows(usize, Rect, i32),
}

pub(crate) mod compose;
pub(crate) mod ctx;
pub(crate) mod geometry;
pub(crate) mod key;
pub(crate) mod prepare;
pub(crate) mod prepare_grid;
pub(crate) mod state;
pub(crate) mod tile;
