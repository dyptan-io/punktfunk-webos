//! Focus rings, selectable cards, game-grid poster card.
use crate::ui::prelude::*;
use anyhow::Result;
use tiny_skia::Pixmap;

/// Card corner radius (softened from moonlight-tv's ~2px).
pub const CARD_RADIUS: i32 = 10;
pub const MODAL_RADIUS: i32 = 20;

/// Approximate moonlight-tv's 2% focus zoom by inflating rect from center.
pub fn focus_zoom(rect: Rect, focused: bool) -> Rect {
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

/// How far the focused-card glow's blur extends past the card edge — the pad
/// `render_focus_ring_tile`'s canvas must leave for it not to clip. The halo reads
/// tighter than this: `render_glow_shape` reshapes the blur's ramp into a collar on the
/// edge with a short tail, rather than the even spread a raw blur gives.
pub const FOCUS_GLOW_BLUR: f32 = 18.0;

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
    /// one shared texture, reused by every card, not a per-frame re-blur.
    ///
    /// The card's own rect and radius: `render_glow_shape` keeps the silhouette sharp and
    /// lets the blur supply only the tail beyond it, so the lit body is the card's shape
    /// rather than the rounder, larger figure a blurred-then-saturated shape becomes.
    pub fn focus_ring(&mut self, rect: Rect) {
        self.fill_glow(rect, CARD_RADIUS, theme().accent_bright, FOCUS_GLOW_BLUR);
    }

    /// A crisp lit edge right on the focused card's own outline, composited on top of the
    /// art (unlike the glow behind it). It gives the halo something to end against, so the
    /// glow reads as light coming off the card rather than fading into the art. Rounded to
    /// `CARD_RADIUS`, the same shape as the art, the placeholder poster and the glow.
    pub fn card_outline(&mut self, rect: Rect) {
        let accent = theme().accent_bright;
        let color = Color::RGBA(accent.r, accent.g, accent.b, 0xd0);
        self.stroke_rounded_rect(rect, CARD_RADIUS, color, 1.5);
    }

    /// Draw text-entry card (PIN/IP boxes); always visible, zoom when focused.
    pub fn card(&mut self, rect: Rect, focused: bool) -> Rect {
        let r = focus_zoom(rect, focused);
        self.card_shadow(r, CARD_RADIUS);
        self.fill_rounded_rect(r, CARD_RADIUS, theme().surface);
        r
    }

    /// Card painted only when focused (no background for unfocused). Used by rows/buttons.
    pub fn selectable(&mut self, rect: Rect, focused: bool) -> Rect {
        let r = focus_zoom(rect, focused);
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

/// Height of a card's title strip: one line of the value font plus breathing room,
/// never more than a third of the card. Shared by the strip's tile builder and by the
/// draw-list geometry that slides it, which must agree to the pixel.
pub fn title_strip_h(raster: &dyn TextRaster, font: FontId, card_h: u32) -> u32 {
    (raster.height(font) + TITLE_STRIP_PAD).min(card_h as i32 / 3).max(1) as u32
}

/// Vertical breathing room around the title strip's single line.
const TITLE_STRIP_PAD: i32 = 16;
/// Left/right inset of the strip's label, doubled when measuring the space it has.
const TITLE_STRIP_INSET: i32 = 8;
/// Blur radius of the frost under the strip.
const TITLE_STRIP_BLUR: usize = 6;
/// Inset of the generated placeholder poster's title block.
const PLACEHOLDER_PAD: i32 = 18;
/// Gap between wrapped lines of that title.
const PLACEHOLDER_LINE_GAP: i32 = 4;

impl Canvas<'_, '_> {
    /// Draws one game/Desktop card's art. `art`, when `Some` (a decoded cover, already
    /// downscaled and premultiplied by `art.rs`), fills the whole card, same as
    /// moonlight-tv's cover-image tiles; `None` draws a generated poster instead (no
    /// real art fetched yet, or the host has none for this title).
    ///
    /// The art layer alone: the shadow belongs to the card tile (`render_card_tile`), and
    /// the strip, the glow and the zoom are composited over that tile by
    /// `app::render::compose`, so an animating card is never rasterized twice.
    /// `render_card_title_tile` re-draws this layer translated, to have something real to
    /// frost over.
    pub fn poster_art(&mut self, r: Rect, title: &str, art: Option<&Pixmap>) {
        match art {
            // Rounded to `CARD_RADIUS`, same as the placeholder poster and the glow
            // behind it — a square-cornered cover was the one thing in the card stack
            // that didn't follow the card's shape. `art::ArtLoader` (`art::resize_pixmap`)
            // has already stretched it to card size; the draw rescales only if a pixmap
            // ever arrives at some other size.
            Some(pixmap) => self.painter.draw_pixmap_rounded(r, pixmap, CARD_RADIUS),
            None => self.placeholder_poster(r, title),
        }
    }

    /// The stand-in cover for a game the host has no art for: the tinted card with the
    /// title set into it like a poster, wrapped and centered. Its own artwork rather
    /// than a bare initial, so an art-less library still reads as a wall of covers —
    /// and so a card carries its title even before focus slides the strip up.
    ///
    /// Cards only; hero art keeps its own (art-or-nothing) treatment.
    fn placeholder_poster(&mut self, r: Rect, title: &str) {
        self.painter.fill_rounded_rect(r, CARD_RADIUS, tint_for(title));
        let font = self.fonts.title;
        let (raster, gap) = (self.fonts.raster, PLACEHOLDER_LINE_GAP);
        let line_h = raster.height(font) + gap;
        let max_w = r.width().saturating_sub(2 * PLACEHOLDER_PAD as u32);
        let mut lines = wrap_text(raster, font, title, max_w);
        // Keep the block inside the card even for a long title; the strip has the full
        // text (ellipsized) anyway, so truncating here loses nothing.
        let max_lines = ((r.height() as i32 - 2 * PLACEHOLDER_PAD) / line_h.max(1)).max(1) as usize;
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = ellipsize(raster, font, last, max_w);
        }

        let block_h = lines.len() as i32 * line_h - gap;
        let mut y = r.y() + (r.height() as i32 - block_h) / 2;
        for line in &lines {
            // Infallible in practice (glyphs come from the bundled font); a placeholder
            // that can't measure its own title just draws the tint.
            let _ = self.text_centered(font, line, r, y, Color::RGBA(0xff, 0xff, 0xff, 0xd8));
            y += line_h;
        }
    }

    /// The frosted title strip overlaying a focused card's bottom edge, drawn over a copy
    /// of the art beneath it so the blur is real frost rather than a flat scrim.
    pub fn poster_title_strip(&mut self, strip: Rect, title: &str) -> Result<()> {
        self.painter.blur_rect(strip, TITLE_STRIP_BLUR);
        // Rounded at the bottom only, to sit inside the card's own rounded corners rather
        // than jutting out square past the art. One rounded rect grown upward by its
        // radius does it: the top pair of corners then falls above the strip (off the
        // tile canvas entirely, where tiny-skia clips it), leaving only the bottom pair.
        let shaped = Rect::new(
            strip.x(),
            strip.y() - CARD_RADIUS,
            strip.width(),
            strip.height() + CARD_RADIUS as u32,
        );
        self.painter
            .fill_rounded_rect(shaped, CARD_RADIUS, Color::RGBA(0x00, 0x00, 0x00, 0x68));
        // The art under the tint was rounded to the same shape, but the blur above smeared
        // it back out into those corners — trim everything to the shape once, so art and
        // frost end on the same rounded edge.
        self.painter.clip_to_rounded_rect(shaped, CARD_RADIUS);
        let font = self.fonts.value;
        let avail = strip.width().saturating_sub(2 * TITLE_STRIP_INSET as u32);
        let label = ellipsize(self.fonts.raster, font, title, avail);
        let y = strip.y() + (strip.height() as i32 - self.fonts.raster.height(font)) / 2;
        self.text(font, &label, strip.x() + TITLE_STRIP_INSET, y, theme().text)?;
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
