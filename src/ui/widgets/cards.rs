//! Focus rings, selectable cards, game-grid poster card.
use crate::ui::prelude::*;
use anyhow::Result;
use tiny_skia::Pixmap;

/// Card corner radius (softened from moonlight-tv's ~2px).
pub const CARD_RADIUS: i32 = 10;
pub const MODAL_RADIUS: i32 = 20;

/// Approximate moonlight-tv's 2% focus zoom by inflating rect from center.
fn focus_zoom(rect: Rect, focused: bool) -> Rect {
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
        let (dx, dy) = SHADOW_OFFSET;
        self.fill_shadow(rect, radius, dx as f32, dy as f32, SHADOW_BLUR, SHADOW_OPACITY);
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
        self.fill_glow(rect, CARD_RADIUS, palette().accent_bright, FOCUS_GLOW_BLUR);
    }

    /// A crisp lit edge right on the focused card's own outline, composited on top of the
    /// art (unlike the glow behind it). It gives the halo something to end against, so the
    /// glow reads as light coming off the card rather than fading into the art. Rounded to
    /// `CARD_RADIUS`, the same shape as the art, the placeholder poster and the glow.
    pub fn card_outline(&mut self, rect: Rect) {
        let accent = palette().accent_bright;
        let color = Color::RGBA(accent.r, accent.g, accent.b, 0xd0);
        self.stroke_rounded_rect(rect, CARD_RADIUS, color, 1.5);
    }

    /// Draw text-entry card (PIN/IP boxes); always visible, zoom when focused.
    pub fn card(&mut self, rect: Rect, focused: bool) -> Rect {
        let r = focus_zoom(rect, focused);
        self.card_shadow(r, CARD_RADIUS);
        self.fill_rounded_rect(r, CARD_RADIUS, palette().surface);
        r
    }

    /// Focus card that never inflates. Rows are rasterized once at their literal size;
    /// `app::App`'s draw-list animates the zoom by GPU-scaling the focused-row tile around its
    /// center (same technique as the grid's card focus-pop) — a CPU-baked inflate
    /// here would fight that, since the rasterized content would then need
    /// re-rendering every animation frame instead of just repositioning.
    pub fn selectable_fixed(&mut self, rect: Rect, focused: bool) {
        if focused {
            self.card_shadow(rect, CARD_RADIUS);
            self.fill_rounded_rect(rect, CARD_RADIUS, palette().surface);
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
pub const CARD_MENU_ROW_H: u32 = 54;

/// Breathing room above and below the submenu's block of rows, inside the glass. The panel
/// is a raised surface in its own right now, so its rows sit off its edges the way a modal's
/// do rather than running into the title above and the card's edge below.
pub const CARD_MENU_ROWS_PAD: i32 = 10;

/// How far the submenu's rows — and the selection band under them — are held off the card's
/// left and right edges.
///
/// Two jobs: it makes the band read as a pill inside the panel rather than a full-bleed
/// stripe, and it is the margin the focus pop grows into. The band zooms by
/// [`FOCUS_GROWTH`](crate::ui::animation::FOCUS_GROWTH) about its own centre, so it gains
/// `growth / 2` of its width on each side; anything less than that here and a focused row
/// would spill past the cover art it is drawn on.
pub const CARD_MENU_BAND_INSET: i32 = 10;

/// Inset of a submenu row's icon from the band's own left edge — on top of
/// [`CARD_MENU_BAND_INSET`], so the icon sits inside the selection pill rather than against
/// its corner.
const CARD_MENU_ICON_INSET: i32 = 14;

/// Gap kept between the "this game has settings overrides" dot and the label that would
/// otherwise run into it. The dot itself is [`super::rows::MARK_DOT_R`] — the same mark the
/// settings rows wear.
const TITLE_DOT_GAP: i32 = 8;

/// The fill both of a card's glass surfaces pass to [`Canvas::poster_frost_panel`]: the same
/// [`glass_fill`](crate::ui::theme::glass_fill) every other raised surface takes. A card's
/// glass was briefly thinner, to keep more of the cover art under it; at that strength it
/// stopped matching the modals, and looking like one material everywhere won.
///
/// On the flat look this goes opaque, which is what makes gating `App::card_frost`
/// on the switch safe: the title keeps a solid backing instead of a bare tint over cover art.
pub fn card_glass() -> Color {
    crate::ui::theme::glass_fill()
}

/// The colour both of a card's title surfaces pass to [`Canvas::poster_strip_label`]: the
/// focused row's, so the card's name reads at the same weight as the row you are on.
pub fn card_title_fg() -> Color {
    palette().text
}

/// Height of the whole frosted panel when `rows` submenu entries sit under the title.
/// Shared by the tile builder and the compose geometry, like [`title_strip_h`] — and it
/// deliberately ignores that function's third-of-a-card cap: the panel is meant to climb
/// the card, which is what says the menu belongs to it.
pub fn card_menu_strip_h(raster: &dyn TextRaster, font: FontId, card_h: u32, rows: usize) -> u32 {
    let panel = title_strip_h(raster, font, card_h) + card_menu_rows_h(rows);
    panel.min(card_h.max(1))
}

/// Height of the submenu's rows block: the rows themselves plus [`CARD_MENU_ROWS_PAD`] at
/// each end. One place, because the tile that draws it, the geometry that places the band and
/// the pointer hit-test all have to agree to the pixel.
pub fn card_menu_rows_h(rows: usize) -> u32 {
    rows as u32 * CARD_MENU_ROW_H + 2 * CARD_MENU_ROWS_PAD as u32
}

/// Left edge and width of anything that lines up with the submenu's selection band — the
/// rows, the band itself and the hairline above them — inset from `rect` by
/// [`CARD_MENU_BAND_INSET`]. One definition, so the three cannot drift apart.
pub fn card_menu_band_x(rect: Rect) -> i32 {
    rect.x() + CARD_MENU_BAND_INSET
}

/// Width of that band on a `card_w`-wide card. See [`card_menu_band_x`].
pub fn card_menu_band_w(card_w: u32) -> u32 {
    card_w.saturating_sub(2 * CARD_MENU_BAND_INSET as u32).max(1)
}

/// The selection band's rect for row `i`, given the rows block's own rect — the band the
/// compose path zooms, and the row the labels are drawn into, are the same rectangle.
pub fn card_menu_row_rect(band: Rect, i: usize) -> Rect {
    Rect::new(
        card_menu_band_x(band),
        band.y() + CARD_MENU_ROWS_PAD + i as i32 * CARD_MENU_ROW_H as i32,
        card_menu_band_w(band.width()),
        CARD_MENU_ROW_H,
    )
}

/// Vertical breathing room around the title strip's single line.
const TITLE_STRIP_PAD: i32 = 16;
/// Left/right inset of the strip's label, doubled when measuring the space it has.
const TITLE_STRIP_INSET: i32 = 8;
/// Inset of the override dot from the panel's right edge — deeper than the label's own, so
/// the mark reads as sitting inside the frosted window rather than against its corner.
const MARK_DOT_INSET: i32 = 16;
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

    /// The glass tint alone, at `fill`, ending on the same rounded edge as the card's art.
    /// Both of a card's glass surfaces draw through here — the collapsed title strip and the
    /// submenu grown out of it — so passing the strength in is what lets them differ from the
    /// modals without forking the drawing.
    ///
    /// The blur itself is a `DrawCmd::Frost` the compose path pushes under this tile, of the
    /// card as it is actually on screen. It used to be baked in: the whole cover re-rasterized
    /// into the tile translated up, then a CPU box blur over it, on every focus move and every
    /// menu open. That is the most expensive thing this app ever did per keypress, and it
    /// bought a blur that could only ever *wipe* — a fragment of the cover was baked in, so
    /// translating the tile read as the card sliding under the glass. Blurring what the GPU
    /// has already composed costs a texture copy and removes both problems.
    pub fn poster_frost_panel(&mut self, strip: Rect, fill: Color) {
        self.painter.fill_rounded_rect(bottom_rounded(strip), CARD_RADIUS, fill);
    }

    /// The title line itself, centred in `band`.
    ///
    /// `color` is the caller's, like the glass under it — both of a card's title surfaces pass
    /// [`card_title_fg`], and they must pass the *same* value or the handoff from strip to
    /// panel flickers.
    ///
    /// `overridden` puts an amber dot at the band's right edge ([`mark_dot_x`]) — the same
    /// mark an overridden settings row wears, so "this game does not use the global settings"
    /// reads the same on the grid as inside the screen that made it true.
    pub fn poster_strip_label(&mut self, band: Rect, title: &str, overridden: bool, color: Color) -> Result<()> {
        let font = self.fonts.value;
        let x = band.x() + TITLE_STRIP_INSET;
        let y = band.y() + (band.height() as i32 - self.fonts.raster.height(font)) / 2;
        if overridden {
            self.mark_dot(
                mark_dot_x(band),
                y + self.fonts.raster.height(font) / 2,
                palette().warning,
            );
        }
        let avail = band
            .width()
            .saturating_sub((TITLE_STRIP_INSET + label_right_pad(overridden)) as u32);
        self.text_faded(font, title, x, y, avail, color)?;
        Ok(())
    }

    /// The submenu rows under a held card's title, inside the glass
    /// [`Self::poster_frost_panel`] has already laid down (which is why this takes the rows'
    /// band rather than drawing its own background).
    ///
    /// The *unfocused* rows only, in [`Theme::muted`](crate::ui::theme::Theme::muted). The
    /// focused one — its surface, its icon and its label together — belongs to
    /// [`CardMenuBandTile`](super::super::tiles::CardMenuBandTile), exactly as a modal's
    /// focused row belongs to [`FocusRowTile`](super::FocusRowTile) rather than to the list
    /// under it. That is what lets the compose path zoom the row's *text* with its surface
    /// instead of popping a bare band under a label that stays put; drawing it here as well
    /// would leave that fixed copy showing from under the zoom.
    ///
    /// `marked` is the row that wears the override dot ([`mark_dot_x`], measured off the row's
    /// own right edge rather than the card's), so raising the panel moves the mark down onto
    /// the Settings row that owns it, stepping in with the rest of the band.
    pub fn poster_menu_rows(
        &mut self,
        band: Rect,
        rows: &[(&str, &str)],
        marked: Option<usize>,
        focused: usize,
    ) -> Result<()> {
        for (i, (glyph, label)) in rows.iter().enumerate() {
            if i == focused {
                continue;
            }
            self.poster_menu_row(
                card_menu_row_rect(band, i),
                glyph,
                label,
                marked == Some(i),
                palette().muted,
            )?;
        }
        Ok(())
    }

    /// One submenu row's icon, label and (optional) override mark, drawn into `row`. Shared
    /// by the muted list and by the focused row's own tile, so the two only ever differ by
    /// the colour passed in and the surface drawn under them.
    pub fn poster_menu_row(&mut self, row: Rect, glyph: &str, label: &str, marked: bool, fg: Color) -> Result<()> {
        let font = self.fonts.value;
        let icon = 22u32;
        // Inside the selection pill, not against the panel's edge — the title above keeps
        // its own shallower inset, because it has no band under it to sit within.
        let icon_x = row.x() + CARD_MENU_ICON_INSET;
        self.icon(
            Rect::new(icon_x, row.y() + (row.height() as i32 - icon as i32) / 2, icon, icon),
            glyph,
            fg,
        )?;
        let text_x = icon_x + icon as i32 + ICON_LABEL_GAP;
        let avail = row.right().saturating_sub(text_x + label_right_pad(marked)).max(0) as u32;
        let y = row.y() + (row.height() as i32 - self.fonts.raster.height(font)) / 2;
        self.text_faded(font, label, text_x, y, avail, fg)?;
        if marked {
            self.mark_dot(mark_dot_x(row), row.y() + row.height() as i32 / 2, palette().warning);
        }
        Ok(())
    }
}

/// A focused card tile with centered text — a padded transparent tile holding
/// one `Painter::card(.., false)` box (no CPU inflate; the zoom is a GPU animation
/// in `app::App`'s draw-list building) with `text` centered in it. Backs the pairing screen's
/// focused digit and button tiles.
pub struct CardTextTile<'a> {
    pub font: FontId,
    pub text: &'a str,
    pub w: u32,
    pub h: u32,
}

impl Widget for CardTextTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let drawn = c.painter.card(area.inflate(-ROW_TILE_PAD), false);
        let text_y = drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(self.font)) / 2;
        c.text_centered(self.font, self.text, drawn, text_y, palette().text)?;
        Ok(())
    }
}

impl TileWidget for CardTextTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.w, self.h, ROW_TILE_PAD)
    }
}
