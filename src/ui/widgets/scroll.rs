//! What a scrolling row list draws around its rows: the scrollbar, and the fades that
//! dissolve a partially-scrolled row into the viewport's edges. Both are their own tiles so
//! showing and hiding them is an alpha composite rather than a re-raster.
use crate::ui::prelude::*;
use anyhow::Result;

/// Scrollbar track+thumb. Rendered as own tile so fade-in/out is alpha
/// composite, not re-rasterization.
const SCROLLBAR_TRACK_W: u32 = 6;

pub struct ListScrollbarTile {
    pub w: u32,
    pub h: u32,
    pub total: usize,
    pub visible: usize,
    pub scroll: usize,
}

impl Widget for ListScrollbarTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        // A list that fits leaves the tile transparent rather than not existing: whether the
        // bar is *drawn* is an alpha the compose path picks, so the tile has to be uploadable
        // either way.
        if self.total <= self.visible {
            return Ok(());
        }
        let (tile_w, tile_h) = (area.width(), area.height());
        let track_w = SCROLLBAR_TRACK_W.min(tile_w);
        let track = Rect::new(tile_w as i32 - track_w as i32, 0, track_w, tile_h);
        let radius = track_w as i32 / 2;
        c.painter
            .fill_rounded_rect(track, radius, Color::RGBA(0xff, 0xff, 0xff, 0x14));

        let thumb_h = ((self.visible as f32 / self.total as f32) * track.height() as f32).round() as u32;
        let thumb_h = thumb_h.clamp(24, track.height());
        let max_thumb_y = track.height().saturating_sub(thumb_h) as f32;
        let max_scroll = (self.total - self.visible).max(1) as f32;
        let thumb_y = track.y() + ((self.scroll as f32 / max_scroll) * max_thumb_y).round() as i32;
        c.painter.fill_rounded_rect(
            Rect::new(track.x(), thumb_y, track_w, thumb_h),
            radius,
            Color::RGBA(0xff, 0xff, 0xff, 0x50),
        );
        Ok(())
    }
}

impl TileWidget for ListScrollbarTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.w, self.h.max(1))
    }
}

/// How tall an edge fade is: exactly one row.
///
/// Deliberately taller than the peek strip it dissolves (`view::settings::PEEK`), so the band
/// reaches past the partial row and into the full row beyond it. Sized to the peek instead,
/// the ramp only reached ~35% alpha by the time it crossed the partial row's text — enough to
/// render, not enough to read as a fade. Being taller also means the dense end lands on the
/// partial row while the row above it takes only the ramp's first, near-clear pixels.
pub const SCROLL_FADE_H: u32 = FOCUS_ROW_H;

/// Tile width for the scroll fade. The ramp is uniform horizontally, so the GPU stretches
/// this to whatever the list's width is — a fixed narrow tile means one static texture for
/// every modal instead of one per content width. Not 1px: under linear filtering a
/// single-column texture has no interior samples to stretch from.
const SCROLL_FADE_TILE_W: u32 = 8;

/// Which edge of the viewport a fade tile dissolves into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FadeEdge {
    /// Dense at the top, clear at the bottom — shown while content is scrolled off above.
    Top,
    /// Clear at the top, dense at the bottom — shown while content remains below.
    Bottom,
}

/// An edge fade that signals "the list continues this way".
///
/// Exists because the scrollbar alone doesn't answer the question on arrival: it's
/// hold-then-fade (see `SCROLL_INDICATOR_HOLD`), so a list that opens already overflowing
/// shows nothing at all once the hold lapses, and the last row looks like the final row.
///
/// Fades to the modal card's own background (`theme().panel`), not to black: the band has to
/// look like the card surface swallowing the row, and any other colour reads as a shadow
/// sitting on top of the list.
pub struct ScrollFadeTile {
    pub edge: FadeEdge,
}

impl Widget for ScrollFadeTile {
    fn render(self, _area: Rect, c: &mut Canvas) -> Result<()> {
        match self.edge {
            FadeEdge::Top => c.painter.fill_vertical_fade(theme().panel, 0xff, 0x00),
            FadeEdge::Bottom => c.painter.fill_vertical_fade(theme().panel, 0x00, 0xff),
        }
        Ok(())
    }
}

impl TileWidget for ScrollFadeTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (SCROLL_FADE_TILE_W, SCROLL_FADE_H)
    }
}
