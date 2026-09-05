//! This app's tile numbering: which [`TileId`] means what.
//!
//! `ui` treats a tile id as an opaque number (see [`TileId`]'s docs) — the enum that used
//! to name these lived in the library and made it unusable by anything else. The numbering
//! is here, dense and `Copy`, so a draw command carries four bytes instead of a `String`.
//!
//! Ids are assigned in three bands: the fixed singletons below, one slot per spinner frame,
//! and an interned band for grid cards (whose count is the library's, not a constant).
use crate::ui::render::TileId;

/// In-stream stats overlay.
pub const STATS_OVERLAY: TileId = TileId(16);
/// Transient toast.
pub const NOTIFICATION: TileId = TileId(17);
/// Log-tail overlay (menu and stream).
pub const LOG_OVERLAY: TileId = TileId(18);
/// Disconnect confirm dialog and focused button.
pub const DISCONNECT_DIALOG: TileId = TileId(19);
pub const DISCONNECT_FOCUS_BUTTON: TileId = TileId(20);
/// Modal card drop shadow (nine-sliceable atlas, not baked).
pub const MODAL_SHADOW: TileId = TileId(29);
