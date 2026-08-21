//! The expanded dropdown a [`RowKind::Dropdown`](super::RowKind) row opens: the option
//! overlay, its option rows, and the popup chrome they sit on. The closed row's pill is the
//! row's own business (see [`Canvas::focus_row`](super::Canvas)).
use std::borrow::Cow;

use anyhow::Result;

use crate::ui::prelude::*;

/// Row height of one dropdown option — also `render_dropdown_option_tile`'s tile size.
pub const DROPDOWN_OPTION_H: u32 = 56;

/// Left/right inset of an option's label inside the popup.
const DROPDOWN_OPTION_INSET: i32 = 20;

/// The popup's own fill, darker than the shared glass: it hangs over the lit settings row it
/// opened from, and at the glass alpha the row's text reads straight through the options.
const DROPDOWN_FILL: Color = Color::RGBA(0x17, 0x11, 0x28, 0xf6);

/// The expanded dropdown: its options as an overlay list anchored below the opener row.
/// One panel background+shadow instead of per-row cards, to avoid shadow smearing.
/// Renders every option unfocused, like the row lists: the focused one composites over it
/// from [`render_dropdown_option_tile`].
pub struct DropdownOverlay<'a> {
    options: &'a [Cow<'a, str>],
}

impl<'a> DropdownOverlay<'a> {
    pub fn new(options: &'a [Cow<'a, str>]) -> Self {
        Self { options }
    }
}

impl Widget for DropdownOverlay<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let bg_rect = Rect::new(
            area.x(),
            area.y(),
            area.width(),
            self.options.len() as u32 * DROPDOWN_OPTION_H,
        );
        c.painter.panel_in(bg_rect, CARD_RADIUS, DROPDOWN_FILL);
        for (i, opt) in self.options.iter().enumerate() {
            c.dropdown_option(opt, false, dropdown_option_rect(area, i))?;
        }
        Ok(())
    }
}

/// Option `index`'s rect within a dropdown overlay.
pub fn dropdown_option_rect(rect: Rect, index: usize) -> Rect {
    Rect::new(
        rect.x(),
        rect.y() + index as i32 * DROPDOWN_OPTION_H as i32,
        rect.width(),
        DROPDOWN_OPTION_H,
    )
}

/// Renders one focused dropdown option as a tile, composited over the overlay.
/// Moving focus recomposites just this tile instead of re-rasterizing.
pub struct DropdownOptionTile<'a> {
    pub option: &'a str,
    pub width: u32,
}

impl Widget for DropdownOptionTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.dropdown_option(self.option, true, area)
    }
}

impl TileWidget for DropdownOptionTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.width, DROPDOWN_OPTION_H)
    }
}

impl Canvas<'_, '_> {
    /// Draws one dropdown option (highlighted if focused) at normal size.
    pub fn dropdown_option(&mut self, option: &str, focused: bool, row_rect: Rect) -> Result<()> {
        if focused {
            let highlight = Rect::new(
                row_rect.x() + 6,
                row_rect.y() + 4,
                row_rect.width().saturating_sub(12),
                row_rect.height().saturating_sub(8),
            );
            self.painter.fill_rounded_rect(
                highlight,
                8,
                Color::RGBA(palette().accent.r, palette().accent.g, palette().accent.b, 0x50),
            );
        }
        let font = self.fonts.value;
        let y = row_rect.y() + (row_rect.height() as i32 - self.fonts.raster.height(font)) / 2;
        let x = row_rect.x() + DROPDOWN_OPTION_INSET;
        // Faded, on the app's one edge ramp: an option is whatever the host named it, and a
        // long one used to run out over the popup's rounded edge and its shadow.
        self.text_faded(
            font,
            option,
            x,
            y,
            (row_rect.right() - x - DROPDOWN_OPTION_INSET).max(0) as u32,
            if focused { palette().text } else { palette().muted },
        )?;
        Ok(())
    }
}
