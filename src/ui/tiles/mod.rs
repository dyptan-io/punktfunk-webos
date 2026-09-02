//! Rasterized-once tile sources for the GPU compositor.
//!
//! Widgets are rasterized by tiny-skia into standalone padded tiles ONCE (keeping the AA/soft
//! shadow look), then composed per frame by the GPU — position, scroll, the focus pop's scale,
//! and fades are all texture-copy parameters, not re-rasterization. See
//! `platform::webos::compositor` and `app::render::prepare`.
//!
//! Every tile here is a [`TileWidget`](crate::ui::TileWidget): a value that says how big a
//! surface it wants and then draws itself into it, rasterized by
//! [`rasterize`](crate::ui::rasterize). That is the same [`Widget`] contract the rest of `ui`
//! draws through, so there is one idiom for "how do I draw a thing" rather than a trait for
//! widgets and a parallel family of free `render_*_tile` functions that each re-spelled the
//! measure/allocate/wrap-in-a-canvas preamble.
mod card;
mod cardmenu;
mod confirm;
mod overlay;
mod text;

pub use card::{
    CardOutlineTile, CardShadowTile, CardTile, CardTitleTile, FocusRingTile, CARD_OUTLINE_PAD, CARD_SHADOW_PAD,
    FOCUS_RING_PAD,
};
pub use cardmenu::{CardMenuBandTile, CardMenuRowsTile, CardMenuTile, CardMenuTitleTile};
pub use confirm::{
    confirm_button_at, confirm_dialog_card, confirm_dialog_layout, ConfirmDialogShellTile, ConfirmSurface,
};
pub use overlay::{LogOverlayTile, StatsOverlayTile, LOG_OVERLAY_LINES};
pub use text::{TextTile, WrappedTextTile};

/// Padding for row tile shadow + sidebar inflate. Settings rows use GPU scale.
pub const ROW_TILE_PAD: i32 = 28;

/// Size of a tile holding a `w`x`h` shape with `pad` of transparent margin all round — the
/// arithmetic every padded tile's [`TileWidget::size`](crate::ui::TileWidget::size) repeats.
/// The margin is what lets the compositor pop and dip a tile without clipping its shadow.
pub const fn padded_size(w: u32, h: u32, pad: i32) -> (u32, u32) {
    (w + 2 * pad as u32, h + 2 * pad as u32)
}
