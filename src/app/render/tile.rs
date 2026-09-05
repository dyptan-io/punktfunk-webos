//! This app's tile numbering: which [`TileId`] means what.
//!
//! `ui` treats a tile id as an opaque number (see [`TileId`]'s docs) — the enum that used
//! to name these lived in the library and made it unusable by anything else. The numbering
//! is here, dense and `Copy`, so a draw command carries four bytes instead of a `String`.
//!
//! Ids are assigned in three bands: the fixed singletons below, one slot per spinner frame,
//! and an interned band for grid cards (whose count is the library's, not a constant).
use crate::ui::render::TileId;

/// Open modal shell (unfocused).
pub const MODAL: TileId = TileId(5);
/// Open modal focused widget (zoom-animated).
pub const MODAL_FOCUS: TileId = TileId(6);
/// Scrollable modal scroll indicator.
pub const SCROLL_INDICATOR: TileId = TileId(11);
/// About document (baked full-height, scrolling inside doesn't invalidate).
pub const SCROLL_CONTENT: TileId = TileId(12);
/// In-stream stats overlay.
pub const STATS_OVERLAY: TileId = TileId(16);
/// Transient toast.
pub const NOTIFICATION: TileId = TileId(17);
/// Log-tail overlay (menu and stream).
pub const LOG_OVERLAY: TileId = TileId(18);
/// Disconnect confirm dialog and focused button.
pub const DISCONNECT_DIALOG: TileId = TileId(19);
pub const DISCONNECT_FOCUS_BUTTON: TileId = TileId(20);
/// Modal fading out (snapshot for cross-fade).
pub const MODAL_PREV: TileId = TileId(23);
/// Leaving modal's scrolled content (frozen).
pub const MODAL_PREV_CONTENT: TileId = TileId(24);
/// Modal card drop shadow (nine-sliceable atlas, not baked).
pub const MODAL_SHADOW: TileId = TileId(29);
/// Smaller atlas for non-modal panels (dropdown popup).
pub const PANEL_SHADOW: TileId = TileId(30);
/// Row band base (one slot per on-screen row, fixed not interned).
const LIST_ROW_BASE: u32 = 33;
pub const LIST_ROW_SLOTS: usize = 32;
/// Tile for on-screen list row (None past band).
pub fn list_row(index: usize) -> Option<TileId> {
    (index < LIST_ROW_SLOTS).then(|| TileId(LIST_ROW_BASE + index as u32))
}
