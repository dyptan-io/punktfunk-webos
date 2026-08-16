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
    /// `(src crop, dst rect)` of the scrolled content, for Settings/About.
    pub content: Option<(Rect, Rect)>,
}

pub(crate) mod compose;
pub(crate) mod geometry;
pub(crate) mod key;
pub(crate) mod prepare;
pub(crate) mod tile;
