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

/// Height of one row of the submenu a held card raises over its title strip.
pub const CARD_MENU_ROW_H: u32 = 46;

/// Gap kept between the "this game has settings overrides" dot and the label that would
/// otherwise run into it. The dot itself is [`super::rows::MARK_DOT_R`] — the same mark the
/// settings rows wear.
const TITLE_DOT_GAP: i32 = 8;

/// The focused submenu row's band: a darkening of the frost rather than a fill over it, and
/// edge to edge so it reads as a band across the panel rather than a pill floating on the
/// card's art. Composited by `app::render::compose`, not baked — see
/// [`Canvas::poster_menu_rows`]. Square-cornered except on the bottom row, which ends on the
/// card's own rounded edge — both shapes come off
/// [`super::super::tiles::render_card_menu_band_tile`].
pub const CARD_MENU_ROW_FOCUS: Color = Color::RGBA(0x00, 0x00, 0x00, 0x9a);

/// Height of the whole frosted panel when `rows` submenu entries sit under the title.
/// Shared by the tile builder and the compose geometry, like [`title_strip_h`] — and it
/// deliberately ignores that function's third-of-a-card cap: the panel is meant to climb
/// the card, which is what says the menu belongs to it.
pub fn card_menu_strip_h(raster: &dyn TextRaster, font: FontId, card_h: u32, rows: usize) -> u32 {
    let panel = title_strip_h(raster, font, card_h) + rows as u32 * CARD_MENU_ROW_H;
    panel.min(card_h.max(1))
}

/// Vertical breathing room around the title strip's single line.
const TITLE_STRIP_PAD: i32 = 16;
/// Left/right inset of the strip's label, doubled when measuring the space it has.
const TITLE_STRIP_INSET: i32 = 8;
/// Inset of the override dot from the panel's right edge — deeper than the label's own, so
/// the mark reads as sitting inside the frosted window rather than against its corner.
const MARK_DOT_INSET: i32 = 16;
/// Blur radius of the frost under the strip.
const TITLE_STRIP_BLUR: usize = 6;
/// Gap between a submenu row's icon and its label.
const ICON_LABEL_GAP: i32 = 10;
/// Inset of the generated placeholder poster's title block.
const PLACEHOLDER_PAD: i32 = 18;
/// Gap between wrapped lines of that title.
const PLACEHOLDER_LINE_GAP: i32 = 4;

/// `rect` reshaped so that filling it at [`CARD_RADIUS`] rounds its bottom corners only —
/// how anything laid over a card's bottom edge (the frost, the submenu's selection band) sits
/// inside the card's own corners rather than jutting out square past the art. One rounded rect
/// grown upward by its radius does it: the top pair of corners then falls above `rect`, off
/// the tile canvas entirely, where tiny-skia clips it.
pub fn bottom_rounded(rect: Rect) -> Rect {
    Rect::new(
        rect.x(),
        rect.y() - CARD_RADIUS,
        rect.width(),
        rect.height() + CARD_RADIUS as u32,
    )
}

/// Left of the override dot within `band`: against the band's right edge, one inset in.
///
/// On the right rather than in front of the label so every line down the card — the title,
/// then Pin and Settings once the panel is up — keeps one left edge whether the mark is
/// there or not, and so raising the panel never shifts the title sideways.
fn mark_dot_x(band: Rect) -> i32 {
    band.right() - MARK_DOT_INSET - 2 * super::rows::MARK_DOT_R
}

/// Right-hand room a label must leave inside a strip or row: the plain inset, plus the
/// override dot's column when one is drawn.
fn label_right_pad(marked: bool) -> i32 {
    if marked {
        MARK_DOT_INSET + 2 * super::rows::MARK_DOT_R + TITLE_DOT_GAP
    } else {
        TITLE_STRIP_INSET
    }
}

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
        let (raster, gap) = (self.fonts.raster, PLACEHOLDER_LINE_GAP);
        let max_w = r.width().saturating_sub(2 * PLACEHOLDER_PAD as u32);
        let font = fitting_font(raster, title, max_w);
        let line_h = raster.height(font) + gap;
        let mut lines = wrap_text(raster, font, title, max_w);
        // Keep the block inside the card even for a long title; the strip carries the whole
        // title anyway, so dropping lines here loses nothing.
        let max_lines = ((r.height() as i32 - 2 * PLACEHOLDER_PAD) / line_h.max(1)).max(1) as usize;
        lines.truncate(max_lines);

        let block_h = lines.len() as i32 * line_h - gap;
        let mut y = r.y() + (r.height() as i32 - block_h) / 2;
        for line in &lines {
            // Centred on the width it actually gets: a word too long even for the smallest
            // font still overflows its line, and must fade at the padding rather than spill
            // out over the card's shadow.
            let w = raster.measure(font, line).0.min(max_w);
            let x = r.x() + (r.width() as i32 - w as i32) / 2;
            // Infallible in practice (glyphs come from the bundled font); a placeholder
            // that can't measure its own title just draws the tint.
            let _ = self.text_faded(font, line, x, y, max_w, Color::RGBA(0xff, 0xff, 0xff, 0xd8));
            y += line_h;
        }
    }

    /// The frosted title strip overlaying a focused card's bottom edge, drawn over a copy
    /// of the art beneath it so the blur is real frost rather than a flat scrim.
    pub fn poster_title_strip(&mut self, strip: Rect, title: &str, overridden: bool) -> Result<()> {
        self.poster_frost_panel(strip);
        self.poster_strip_label(strip, title, overridden)
    }

    /// The frosted glass alone: blur, tint, and the bottom-rounded clip. Split out because
    /// the held-card submenu frosts a much taller panel than the one line its title
    /// occupies, and both must end on the same rounded edge as the card's art.
    pub fn poster_frost_panel(&mut self, strip: Rect) {
        self.painter.blur_rect(strip, TITLE_STRIP_BLUR);
        let shaped = bottom_rounded(strip);
        self.painter
            .fill_rounded_rect(shaped, CARD_RADIUS, Color::RGBA(0x00, 0x00, 0x00, 0x68));
        // The art under the tint was rounded to the same shape, but the blur above smeared
        // it back out into those corners — trim everything to the shape once, so art and
        // frost end on the same rounded edge.
        self.painter.clip_to_rounded_rect(shaped, CARD_RADIUS);
    }

    /// The title line itself, centred in `band`.
    ///
    /// `overridden` puts an amber dot at the band's right edge ([`mark_dot_x`]) — the same
    /// mark an overridden settings row wears, so "this game does not use the global settings"
    /// reads the same on the grid as inside the screen that made it true.
    pub fn poster_strip_label(&mut self, band: Rect, title: &str, overridden: bool) -> Result<()> {
        let font = self.fonts.value;
        let x = band.x() + TITLE_STRIP_INSET;
        let y = band.y() + (band.height() as i32 - self.fonts.raster.height(font)) / 2;
        if overridden {
            self.mark_dot(
                mark_dot_x(band),
                y + self.fonts.raster.height(font) / 2,
                theme().warning,
            );
        }
        let avail = band
            .width()
            .saturating_sub((TITLE_STRIP_INSET + label_right_pad(overridden)) as u32);
        self.text_faded(font, title, x, y, avail, theme().text)?;
        Ok(())
    }

    /// The submenu rows under a held card's title, inside the frost [`poster_title_strip`]
    /// has already laid down (which is why this takes the rows' band rather than drawing
    /// its own background).
    ///
    /// Deliberately focus-free: every row is drawn identically, and the selection is a
    /// translucent darkening the compose path lays under this tile (see
    /// [`CARD_MENU_ROW_FOCUS`]). Baking it in instead would put this whole panel — a
    /// full-card art rescale plus a blur of it — on the rebuild path of every row move,
    /// which is exactly the work the tile cache exists to avoid repeating.
    /// `marked` is the row that wears the override dot, in the same column the collapsed
    /// title's sits in ([`mark_dot_x`]), so raising the panel moves the mark straight down onto
    /// the Settings row that owns it.
    pub fn poster_menu_rows(&mut self, band: Rect, rows: &[(&str, &str)], marked: Option<usize>) -> Result<()> {
        let font = self.fonts.value;
        let icon = 22u32;
        for (i, (glyph, label)) in rows.iter().enumerate() {
            let row = Rect::new(
                band.x(),
                band.y() + i as i32 * CARD_MENU_ROW_H as i32,
                band.width(),
                CARD_MENU_ROW_H,
            );
            let fg = theme().text;
            // One left edge with the title above (`poster_strip_label`'s inset): the mark now
            // lives on the right, so nothing has to be held clear of it.
            let icon_x = row.x() + TITLE_STRIP_INSET;
            self.icon(
                Rect::new(icon_x, row.y() + (row.height() as i32 - icon as i32) / 2, icon, icon),
                glyph,
                fg,
            )?;
            let text_x = icon_x + icon as i32 + ICON_LABEL_GAP;
            let marked = marked == Some(i);
            let avail = row.right().saturating_sub(text_x + label_right_pad(marked)).max(0) as u32;
            let y = row.y() + (row.height() as i32 - self.fonts.raster.height(font)) / 2;
            self.text_faded(font, label, text_x, y, avail, fg)?;
            if marked {
                self.mark_dot(mark_dot_x(row), row.y() + row.height() as i32 / 2, theme().warning);
            }
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
