//! The expanded dropdown a [`RowKind::Dropdown`](super::RowKind) row opens: the option
//! overlay, its option rows, and the popup chrome they sit on. The closed row's pill is the
//! row's own business (see [`Canvas::focus_row`](super::Canvas)).
use crate::ui::prelude::*;
use anyhow::Result;

/// Row height of one dropdown option — also `render_dropdown_option_tile`'s tile size.
pub const DROPDOWN_OPTION_H: u32 = 56;

/// The expanded dropdown: its options as an overlay list anchored below the opener row.
/// One panel background+shadow instead of per-row cards, to avoid shadow smearing.
/// Renders every option unfocused, like the row lists: the focused one composites over it
/// from [`render_dropdown_option_tile`].
pub struct DropdownOverlay<'a> {
    options: &'a [String],
}

impl<'a> DropdownOverlay<'a> {
    pub fn new(options: &'a [String]) -> Self {
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
        c.painter.popup_panel(bg_rect, Color::RGBA(0xff, 0xff, 0xff, 0x20));
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
pub fn render_dropdown_option_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    option: &str,
    width: u32,
) -> Result<Painter> {
    let mut p = Painter::new(width, DROPDOWN_OPTION_H);
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    c.dropdown_option(option, true, Rect::new(0, 0, width, DROPDOWN_OPTION_H))?;
    Ok(p)
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
                Color::RGBA(theme().accent.r, theme().accent.g, theme().accent.b, 0x50),
            );
        }
        let font = self.fonts.value;
        let y = row_rect.y() + (row_rect.height() as i32 - self.fonts.raster.height(font)) / 2;
        self.text(
            font,
            option,
            row_rect.x() + 20,
            y,
            if focused { theme().text } else { theme().muted },
        )?;
        Ok(())
    }
}

impl Painter {
    /// Common popup panel chrome: shadowed dark background with colored border.
    pub fn popup_panel(&mut self, rect: Rect, border_color: Color) {
        self.card_shadow(rect, CARD_RADIUS);
        self.fill_rounded_rect(rect, CARD_RADIUS, Color::RGBA(0x17, 0x11, 0x28, 0xf6));
        self.stroke_rounded_rect(rect, CARD_RADIUS, border_color, 1.5);
    }
}
