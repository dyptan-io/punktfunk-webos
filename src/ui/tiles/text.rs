//! Standalone text tiles: one line, and one wrapped block.
use crate::ui::prelude::*;
use anyhow::Result;

/// A single line of text as its own tight transparent tile.
pub struct TextTile<'a> {
    pub font: FontId,
    pub text: &'a str,
    pub color: Color,
}

impl Widget for TextTile<'_> {
    fn render(self, _area: Rect, c: &mut Canvas) -> Result<()> {
        c.text(self.font, self.text, 0, 0, self.color)?;
        Ok(())
    }
}

impl TileWidget for TextTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        fonts.raster.measure(self.font, self.text)
    }
}

/// A wrapped text block as its own transparent tile: `max_w` wide, as tall as its wrapped line
/// count.
pub struct WrappedTextTile<'a> {
    pub font: FontId,
    pub text: &'a str,
    pub max_w: u32,
    pub color: Color,
    pub line_gap: i32,
}

impl Widget for WrappedTextTile<'_> {
    fn render(self, _area: Rect, c: &mut Canvas) -> Result<()> {
        c.text_wrapped(self.font, self.text, 0, 0, self.max_w, self.color, self.line_gap)?;
        Ok(())
    }
}

impl TileWidget for WrappedTextTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        let line_h = fonts.raster.height(self.font) + self.line_gap;
        let lines = wrap_text(fonts.raster, self.font, self.text, self.max_w).len().max(1) as u32;
        (self.max_w.max(1), lines * line_h.max(1) as u32)
    }
}
