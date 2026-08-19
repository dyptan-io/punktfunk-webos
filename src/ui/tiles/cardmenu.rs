//! The submenu a held grid card raises, in four tiles.
//!
//! Baked as four pieces rather than one panel because each has to move differently while it
//! opens:
//!
//! - the frost ([`CardMenuTile`]) can only *wipe*: it carries a fragment of the card's cover
//!   baked in for the blur to work on, and translating that reads as the card sliding under the
//!   glass;
//! - the title ([`CardMenuTitleTile`]) and the rows ([`CardMenuRowsTile`]) have to *travel*,
//!   riding the top edge of the growing window — the title is already on screen at the card's
//!   bottom before the menu opens, so it continues upward from there rather than restarting;
//! - the band ([`CardMenuBandTile`]) is a translucent darkening, so text baked under it would
//!   dim with the frost.
//!
//! See `app::render::compose`, which is the one place those four motions are reconciled.
use crate::ui::prelude::*;
use anyhow::Result;
use tiny_skia::Pixmap;

/// The frosted glass itself, and nothing else — no title, no row text. `title` and `rows` are
/// here for the *sizing* they imply (the placeholder poster's text, the panel's height), not to
/// be drawn.
#[derive(Clone, Copy)]
pub struct CardMenuTile<'a> {
    pub card_w: u32,
    pub card_h: u32,
    pub title: &'a str,
    pub art: Option<&'a Pixmap>,
    /// `(icon glyph, label)` per row.
    pub rows: &'a [(&'a str, &'a str)],
}

impl Widget for CardMenuTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let panel_h = area.height();
        // Same trick as the title strip: the card's art re-drawn translated up by everything
        // above the panel, so the frost blurs the cover it actually sits on.
        c.poster_art(
            Rect::new(
                0,
                -((self.card_h.saturating_sub(panel_h)) as i32),
                self.card_w,
                self.card_h,
            ),
            self.title,
            self.art,
        );
        c.poster_frost_panel(area);
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
        c.poster_strip_label(area, self.title, false)
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
/// over the selection band so the band darkens the frost and nothing else. Every row draws
/// identically (see [`Canvas::poster_menu_rows`]), so which row is focused is in no tile's cache
/// key and moving between them rebuilds nothing.
pub struct CardMenuRowsTile<'a> {
    pub card_w: u32,
    pub card_h: u32,
    pub rows: &'a [(&'a str, &'a str)],
    /// Which row wears the "has overrides" dot, if any.
    pub marked: Option<usize>,
}

impl Widget for CardMenuRowsTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.poster_menu_rows(area, self.rows, self.marked)
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

/// The selection band, the width of the card and *two* rows tall: a square-cornered row on top,
/// a bottom-rounded one under it.
///
/// A tile rather than a plain fill because the bottom row's band reaches the card's bottom edge,
/// where square corners jut out past the card's rounded art ([`bottom_rounded`]). Both shapes on
/// one tile so the compose path draws either from a single command — picking which half to crop,
/// rather than branching between a texture and a fill that would have to keep their alpha in
/// step (see `app::render::compose`).
pub struct CardMenuBandTile {
    pub card_w: u32,
}

impl Widget for CardMenuBandTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let row = Rect::new(0, 0, area.width(), CARD_MENU_ROW_H);
        c.painter.fill_rect(row, CARD_MENU_ROW_FOCUS);
        c.painter.fill_rounded_rect(
            bottom_rounded(row.offset(0, CARD_MENU_ROW_H as i32)),
            CARD_RADIUS,
            CARD_MENU_ROW_FOCUS,
        );
        Ok(())
    }
}

impl TileWidget for CardMenuBandTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.card_w.max(1), 2 * CARD_MENU_ROW_H)
    }
}
