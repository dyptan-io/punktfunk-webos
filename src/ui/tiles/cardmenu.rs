//! The submenu a held grid card raises, in four tiles.
//!
//! Baked as four pieces rather than one panel because each has to move differently while it
//! opens:
//!
//! - the glass ([`CardMenuTile`]) can only *wipe*: it ends on the card's own rounded bottom
//!   edge, so it has to stay flush to it while the window grows upward;
//! - the title ([`CardMenuTitleTile`]) and the rows ([`CardMenuRowsTile`]) have to *travel*,
//!   riding the top edge of the growing window — the title is already on screen at the card's
//!   bottom before the menu opens, so it continues upward from there rather than restarting;
//! - the band ([`CardMenuBandTile`]) is the focused row's raised surface, and it *slides*
//!   between rows while the labels stay put.
//!
//! See `app::render::compose`, which is the one place those four motions are reconciled.
use crate::ui::prelude::*;
use crate::ui::tiles::{padded_size, ROW_TILE_PAD};
use anyhow::Result;

/// The glass tint itself, and nothing else — no title, no row text. `card_h`/`rows` are here
/// for the *sizing* they imply (the panel's height), not to be drawn; the blur beneath is the
/// compositor's `DrawCmd::Frost`.
#[derive(Clone, Copy)]
pub struct CardMenuTile<'a> {
    pub card_w: u32,
    pub card_h: u32,
    /// `(icon glyph, label)` per row.
    pub rows: &'a [(&'a str, &'a str)],
}

impl Widget for CardMenuTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.poster_frost_panel(area, card_glass());
        Ok(())
    }
}

impl TileWidget for CardMenuTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        (
            self.card_w.max(1),
            card_menu_strip_h(fonts.raster, fonts.value, self.card_h, self.rows.len()),
        )
    }
}

/// The panel's title line alone, on a transparent tile — drawn at the same inset and baseline
/// [`CardTitleTile`](super::CardTitleTile) uses, so the moment the menu opens and this takes
/// over, the name does not shift by a pixel.
///
/// No override dot here, unlike the collapsed strip: with the panel open the mark moves down its
/// own right-hand column onto the Settings row that owns it (see [`Canvas::poster_menu_rows`]).
pub struct CardMenuTitleTile<'a> {
    pub card_w: u32,
    pub card_h: u32,
    pub title: &'a str,
}

impl Widget for CardMenuTitleTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.poster_strip_label(area, self.title, false, card_title_fg())
    }
}

impl TileWidget for CardMenuTitleTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        (
            self.card_w.max(1),
            title_strip_h(fonts.raster, fonts.value, self.card_h).max(1),
        )
    }
}

/// The submenu's icons and labels alone, on a transparent tile the width of the card — laid
/// over the selection band so the band darkens the glass and nothing else. The focused row is
/// drawn in the theme's focused text colour and the rest muted (see [`Canvas::poster_menu_rows`]),
/// so unlike its three siblings this tile *is* keyed by focus — a row move rebuilds two short
/// labels, which is what that costs now the panel carries no art.
pub struct CardMenuRowsTile<'a> {
    pub card_w: u32,
    pub card_h: u32,
    pub rows: &'a [(&'a str, &'a str)],
    /// Which row wears the "has overrides" dot, if any.
    pub marked: Option<usize>,
    /// The row drawn in the focused text colour.
    pub focused: usize,
}

impl Widget for CardMenuRowsTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.poster_menu_rows(area, self.rows, self.marked, self.focused)
    }
}

impl TileWidget for CardMenuRowsTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        // What is left of the panel under the title line.
        let (raster, font) = (fonts.raster, fonts.value);
        let rows_h = card_menu_strip_h(raster, font, self.card_h, self.rows.len()).saturating_sub(title_strip_h(
            raster,
            font,
            self.card_h,
        ));
        (self.card_w.max(1), rows_h.max(1))
    }
}

/// The selection band: one row, drawn through [`Painter::selectable_fixed`] — the same
/// shadowed, [`CARD_RADIUS`]-rounded [`Theme::surface`](crate::ui::style::Theme::surface) card
/// a focused settings row is. Padded by [`ROW_TILE_PAD`] like [`FocusRowTile`](super::FocusRowTile),
/// because a shadow needs somewhere to fall.
///
/// It used to be a square-cornered band with no shadow, in two halves — one square, one
/// bottom-rounded for the row that ends on the card's own edge. Same fill as a settings row and
/// yet visibly flatter, because the lift comes from the shadow and the corners, not the colour.
pub struct CardMenuBandTile {
    pub card_w: u32,
}

impl Widget for CardMenuBandTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.painter.selectable_fixed(area.inflate(-ROW_TILE_PAD), true);
        Ok(())
    }
}

impl TileWidget for CardMenuBandTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.card_w.max(1), CARD_MENU_ROW_H, ROW_TILE_PAD)
    }
}
