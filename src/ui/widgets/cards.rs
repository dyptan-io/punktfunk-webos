//! Focus rings, selectable cards, game-grid poster card.
use crate::ui::prelude::*;
use anyhow::Result;
use tiny_skia::Pixmap;

/// Card corner radius (softened from moonlight-tv's ~2px).
pub const CARD_RADIUS: i32 = 10;
pub const MODAL_RADIUS: i32 = 20;

/// Approximate moonlight-tv's 2% focus zoom by inflating rect from center.
pub fn inflate(rect: Rect, focused: bool) -> Rect {
    if !focused {
        return rect;
    }
    let grow_w = ((rect.width() as f32) * 0.02).round() as i32;
    let grow_h = ((rect.height() as f32) * 0.02).round() as i32;
    Rect::new(
        rect.x() - grow_w,
        rect.y() - grow_h,
        rect.width() + 2 * grow_w as u32,
        rect.height() + 2 * grow_h as u32,
    )
}

/// How far the focused-card glow's blur extends past the card edge — the
/// pad `render_focus_ring_tile`'s canvas must leave for it not to clip.
pub const FOCUS_GLOW_BLUR: f32 = 16.0;

/// The themed card shapes, as `Painter` methods rather than free functions taking one:
/// they need nothing but the surface, so a tile builder with only a `Painter` (and no
/// fonts — see `render_focus_ring_tile`) can still draw them. Anything that also needs
/// the glyph cache or fonts is a [`Canvas`](crate::ui::Canvas) method instead.
impl Painter {
    /// Soft drop shadow matching moonlight-tv's card look.
    pub fn card_shadow(&mut self, rect: Rect, radius: i32) {
        self.fill_shadow(rect, radius, 3.0, 5.0, SHADOW_BLUR, 0x60);
    }

    /// Soft glow behind a focused card — a blurred halo in the accent color,
    /// replacing the old hard double-outline ring for a more pleasant look. Same
    /// cached-shape technique as a drop shadow (`Painter::fill_glow`), so it costs
    /// one shared texture, reused by every card, not a per-frame re-blur. Rounded
    /// noticeably less than the card itself (`radius`) — a smaller pre-blur radius
    /// leaves more straight edge for the blur to soften, which reads as hugging
    /// the card's actual corners rather than blooming into a big round blob.
    pub fn focus_ring(&mut self, rect: Rect, radius: i32) {
        self.fill_glow(rect, radius / 2, theme().accent_bright, FOCUS_GLOW_BLUR);
    }

    /// A crisp thin outline right at the card's own edge — composited on top of
    /// the card art (unlike the soft glow behind it), so the transition from
    /// glow to art reads as a clean rectangle rather than a smudge. Square, not
    /// `CARD_RADIUS`-rounded: the art itself is a plain blit with square corners
    /// (see [`Canvas::poster_card`](crate::ui::Canvas::poster_card)), so a rounded
    /// outline would float visibly outside the actual art edge whenever a cover is
    /// loaded.
    pub fn card_outline(&mut self, rect: Rect) {
        let color = Color::RGBA(
            theme().accent_bright.r,
            theme().accent_bright.g,
            theme().accent_bright.b,
            0xd0,
        );
        self.stroke_rounded_rect(rect, 0, color, 1.5);
    }

    /// Draw text-entry card (PIN/IP boxes); always visible, zoom when focused.
    pub fn card(&mut self, rect: Rect, focused: bool) -> Rect {
        let r = inflate(rect, focused);
        self.card_shadow(r, CARD_RADIUS);
        self.fill_rounded_rect(r, CARD_RADIUS, theme().surface);
        r
    }

    /// Card painted only when focused (no background for unfocused). Used by rows/buttons.
    pub fn selectable(&mut self, rect: Rect, focused: bool) -> Rect {
        let r = inflate(rect, focused);
        if focused {
            self.card_shadow(r, CARD_RADIUS);
            self.fill_rounded_rect(r, CARD_RADIUS, theme().surface);
        }
        r
    }

    /// Same as [`selectable`](Self::selectable) but never inflates: settings rows are
    /// rasterized once at their literal size, and `app::App`'s draw-list building animates
    /// the zoom-in itself by GPU-scaling the whole focused-row tile around its
    /// center (same technique as the grid's card focus-pop) — a CPU-baked inflate
    /// here would fight that, since the rasterized content would then need
    /// re-rendering every animation frame instead of just repositioning.
    pub fn selectable_fixed(&mut self, rect: Rect, focused: bool) {
        if focused {
            self.card_shadow(rect, CARD_RADIUS);
            self.fill_rounded_rect(rect, CARD_RADIUS, theme().surface);
        }
    }
}

/// A handful of muted hues for the poster-card placeholder tint (hash-selected per
/// title, not arbitrary RGB) — kept dark enough that white text stays legible.
pub const POSTER_TINTS: [Color; 6] = [
    Color::RGB(0x4a, 0x3a, 0x7d), // violet
    Color::RGB(0x35, 0x40, 0x6e), // indigo
    Color::RGB(0x6b, 0x3a, 0x68), // plum
    Color::RGB(0x57, 0x50, 0x93), // deep lavender
    Color::RGB(0x3a, 0x4a, 0x8c), // slate blue
    Color::RGB(0x7d, 0x4a, 0x5e), // mauve
];

pub fn tint_for(title: &str) -> Color {
    let hash = title
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(u32::from(b)));
    POSTER_TINTS[hash as usize % POSTER_TINTS.len()]
}

impl Canvas<'_, '_> {
    /// Draws one game/Desktop tile. `art`, when `Some` (a decoded cover, already
    /// downscaled and premultiplied by `art.rs`), fills the whole card, same as
    /// moonlight-tv's cover-image tiles; `None` falls back to a tinted placeholder +
    /// initial letter (no real art fetched yet, or the host has none for this title).
    /// Either way a bottom title strip overlays the art/tint, matching the reference's
    /// always-present (ellipsized) title label.
    pub fn poster_card(&mut self, rect: Rect, title: &str, art: Option<&Pixmap>, focused: bool) -> Result<()> {
        let r = inflate(rect, focused);
        self.painter.card_shadow(r, CARD_RADIUS);

        let (value_font, title_font) = (self.fonts.value, self.fonts.title);
        let strip_h = (self.fonts.raster.height(value_font) + 16).min(r.height() as i32 / 3);
        match art {
            // Already stretched to this card size by `art::ArtLoader` (see
            // `art::resize_pixmap`) — a plain blit, not `draw_pixmap_scaled`. Falls back
            // to scaling if a pixmap ever arrives at some other size.
            Some(pixmap) if pixmap.width() == r.width() && pixmap.height() == r.height() => {
                self.painter.draw_pixmap(r.x(), r.y(), pixmap);
            }
            Some(pixmap) => {
                self.painter.draw_pixmap_scaled(r, pixmap);
            }
            None => {
                self.painter.fill_rounded_rect(r, CARD_RADIUS, tint_for(title));
                let initial = title
                    .chars()
                    .find(|c| c.is_alphanumeric())
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string();
                let ih = self.fonts.raster.measure(title_font, &initial).1;
                let art_h = r.height() as i32 - strip_h;
                let text_y = r.y() + (art_h - ih as i32) / 2;
                self.text_centered(title_font, &initial, r, text_y, Color::RGBA(0xff, 0xff, 0xff, 0xa0))?;
            }
        }

        let strip = Rect::new(r.x(), r.bottom() - strip_h, r.width(), strip_h.max(0) as u32);
        self.painter
            .fill_frosted_rect(strip, 0, Color::RGBA(0x00, 0x00, 0x00, 0x68), 6);
        let label = ellipsize(self.fonts.raster, value_font, title, strip.width().saturating_sub(16));
        let label_y = strip.y() + (strip.height() as i32 - self.fonts.raster.height(value_font)) / 2;
        self.text(value_font, &label, strip.x() + 8, label_y, theme().text)?;

        if focused {
            self.painter.focus_ring(r, CARD_RADIUS);
        }
        Ok(())
    }
}

/// A focused card tile with centered text — a padded transparent tile holding
/// one `Painter::card(.., false)` box (no CPU inflate; the zoom is a GPU animation
/// in `app::App`'s draw-list building) with `text` centered in it. Backs the pairing screen's
/// focused digit and button tiles.
pub fn render_card_text_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    font: FontId,
    text: &str,
    w: u32,
    h: u32,
) -> Result<Painter> {
    let pad = ROW_TILE_PAD;
    let mut p = Painter::new(w + 2 * pad as u32, h + 2 * pad as u32);
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    let drawn = c.painter.card(Rect::new(pad, pad, w, h), false);
    let text_y = drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(font)) / 2;
    c.text_centered(font, text, drawn, text_y, theme().text)?;
    Ok(p)
}
