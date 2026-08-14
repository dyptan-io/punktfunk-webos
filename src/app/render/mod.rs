//! The render path's own vocabulary: which tile is which ([`tile`]), and what each one's
//! pixels depend on ([`key`]).
//!
//! Both were in `ui` — the tile enum in `ui::render`, the staleness keys in `ui::tiles` —
//! where they named this app's screens and its `Settings`. `ui` now sees an opaque
//! [`TileId`](crate::ui::render::TileId) and an opaque `u64` version, and everything that
//! gives those meaning lives here.

pub(crate) mod key;
pub(crate) mod tile;
