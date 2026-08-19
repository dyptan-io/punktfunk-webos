//! The grid card and the decorations composited around it.
//!
//! All cards are one size, so the shadow, focus ring, outline and pin badge are each a single
//! shared tile drawn at every card's position rather than baked into the cards themselves.
use crate::ui::prelude::*;
use crate::ui::tiles::padded_size;
use anyhow::Result;
use tiny_skia::Pixmap;

/// Transparent margin the card's drop shadow (dx 3 / dy 5 / blur 14) needs around the card
/// itself — the padding of [`CardShadowTile`]'s canvas, and how far past the viewport a card
/// can still be visible.
pub const CARD_SHADOW_PAD: i32 = 20;

/// Transparent padding around the focus-ring tile — must clear `FOCUS_GLOW_BLUR`'s blur radius
/// or the glow clips against the canvas edge.
pub const FOCUS_RING_PAD: i32 = 24;

/// Transparent padding around the card-outline tile — just enough for the stroke's own
/// width/AA, not a blur radius like [`FOCUS_RING_PAD`].
pub const CARD_OUTLINE_PAD: i32 = 4;

/// Diameter of the pinned badge composited over the focused grid/pinned card's top-right
/// corner (see `tile::PIN_BADGE`).
pub const PIN_BADGE_SIZE: u32 = 28;

/// The card drop shadow, composited *behind* each card rather than baked into it. Every card's
/// shadow is identical, so baking it in bought nothing and cost every card tile a 20px margin a
/// side — ~35% more pixels rasterized, uploaded and blended per card.
pub struct CardShadowTile {
    pub w: u32,
    pub h: u32,
}

impl Widget for CardShadowTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.painter.card_shadow(area.inflate(-CARD_SHADOW_PAD), CARD_RADIUS);
        Ok(())
    }
}

impl TileWidget for CardShadowTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.w, self.h, CARD_SHADOW_PAD)
    }
}

/// Focus-ring glow, shared across cards; the GPU scales and fades it.
pub struct FocusRingTile {
    pub w: u32,
    pub h: u32,
}

impl Widget for FocusRingTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.painter.focus_ring(area.inflate(-FOCUS_RING_PAD));
        Ok(())
    }
}

impl TileWidget for FocusRingTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.w, self.h, FOCUS_RING_PAD)
    }
}

/// The focused card's crisp lit edge, composited on top of the card art — see
/// [`Painter::card_outline`].
pub struct CardOutlineTile {
    pub w: u32,
    pub h: u32,
}

impl Widget for CardOutlineTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.painter.card_outline(area.inflate(-CARD_OUTLINE_PAD));
        Ok(())
    }
}

impl TileWidget for CardOutlineTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.w, self.h, CARD_OUTLINE_PAD)
    }
}

/// Grid card (unfocused), exactly card-sized. The GPU scales it and composites the shadow,
/// focus ring, title strip and outline around it.
pub struct CardTile<'a> {
    pub w: u32,
    pub h: u32,
    pub title: &'a str,
    pub art: Option<&'a Pixmap>,
}

impl Widget for CardTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.poster_art(area, self.title, self.art);
        Ok(())
    }
}

impl TileWidget for CardTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.w, self.h)
    }
}

/// The focused card's title strip, exactly card-wide.
///
/// Frost needs something to blur, so the card's own art is re-drawn here translated up by
/// everything above the strip: the strip's slice of the cover lands at y 0 and the rest falls
/// off the canvas, where tiny-skia clips it. One small blur per focus move — a fraction of the
/// card build happening at that same rate — and nothing per frame, since the wipe is a crop of
/// this tile (see `app::render::compose`).
pub struct CardTitleTile<'a> {
    pub card_w: u32,
    pub card_h: u32,
    pub title: &'a str,
    pub art: Option<&'a Pixmap>,
    pub overridden: bool,
}

impl CardTitleTile<'_> {
    fn strip_h(&self, fonts: &Fonts) -> u32 {
        title_strip_h(fonts.raster, fonts.value, self.card_h)
    }
}

impl Widget for CardTitleTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let strip_h = area.height();
        c.poster_art(
            Rect::new(0, -((self.card_h - strip_h) as i32), self.card_w, self.card_h),
            self.title,
            self.art,
        );
        c.poster_title_strip(area, self.title, self.overridden)
    }
}

impl TileWidget for CardTitleTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        (self.card_w.max(1), self.strip_h(fonts))
    }
}

/// Pinned badge: dark disc with a PIN icon. One shared tile, composited over the focused card
/// in the draw list rather than baked into individual card tiles.
pub struct PinBadgeTile;

impl Widget for PinBadgeTile {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let d = area.width();
        let mid = d as f32 / 2.0;
        c.painter
            .fill_circle(mid, mid, mid, Color::RGBA(0x00, 0x00, 0x00, 0x70));
        let icon = (d as f32 * 0.6) as u32;
        let inset = ((d - icon) / 2) as i32;
        c.icon(Rect::new(inset, inset, icon, icon), icons().pin, theme().muted)
    }
}

impl TileWidget for PinBadgeTile {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (PIN_BADGE_SIZE, PIN_BADGE_SIZE)
    }
}
