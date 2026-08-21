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
//! - the band ([`CardMenuBandTile`]) is the focused row entire — surface, icon and label —
//!   and it *pops* on whichever row has focus, zoomed as a unit.
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
        c.poster_strip_label(area, self.title, false, card_title_fg())?;
        // The same `Theme::rule` hairline a settings card draws under its own title, run to
        // the width of the rows below it rather than the panel's — so it lines up with the
        // selection pill's edges instead of the glass's. On this tile and not the collapsed
        // strip's: it separates the name from a list, and there is no list until the panel
        // is up.
        c.painter.rule(
            card_menu_band_x(area),
            area.bottom() - 1,
            card_menu_band_w(area.width()),
        );
        Ok(())
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

/// The *unfocused* rows' icons and labels, on a transparent tile the width of the card. The
/// focused one is skipped here and drawn by [`CardMenuBandTile`] instead (see
/// [`Canvas::poster_menu_rows`]), so unlike the glass and the title this tile *is* keyed by
/// focus — a row move rebuilds a short label, which is what that costs now the panel carries
/// no art.
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

/// The focused row — surface, icon and label in one tile the compose path zooms as a unit.
/// [`FocusRowTile`](crate::ui::widgets::FocusRowTile) at submenu size, down to the
/// [`ROW_TILE_PAD`] its shadow falls in, and drawn from the same
/// [`Painter::selectable_fixed`] and [`Canvas::poster_menu_row`] as the rows around it.
///
/// It carries the content rather than being a bare band so the label zooms *with* the
/// surface: [`CardMenuRowsTile`] skips whichever row this one owns, or an unzoomed copy of
/// the text would show from underneath. That costs one icon and one short label per focus
/// move, which is what the modals pay for the same effect.
///
/// Held off the card's side edges by [`CARD_MENU_BAND_INSET`] — the room the pop grows into.
pub struct CardMenuBandTile<'a> {
    pub card_w: u32,
    /// `(icon glyph, label)` of the focused row.
    pub row: (&'a str, &'a str),
    /// Whether this row is the one wearing the "has overrides" dot.
    pub marked: bool,
}

impl Widget for CardMenuBandTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let inner = area.inflate(-ROW_TILE_PAD);
        c.painter.selectable_fixed(inner, true);
        let (glyph, label) = self.row;
        c.poster_menu_row(inner, glyph, label, self.marked, card_title_fg())
    }
}

impl TileWidget for CardMenuBandTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(card_menu_band_w(self.card_w), CARD_MENU_ROW_H, ROW_TILE_PAD)
    }
}
