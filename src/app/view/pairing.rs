//! Pairing modal rendering. Logic lives in `app::state::pairing`.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FontId, Fonts, Painter, TextCache, TextRaster};
use anyhow::Result;

pub(crate) const TITLE: &str = "Pair with host";
pub(crate) const SUBTITLE: &str = "Two ways to pair with this host — either one works.";

/// All y-positions on pairing card, computed once (keeps renderer, hit-test, and tile prep in sync).
pub(crate) struct PairingLayout {
    pub(crate) button: Rect,
    pub(crate) button_caption_y: i32,
    pub(crate) or_y: i32,
    pub(crate) pin_caption_y: i32,
    pub(crate) pin_y: i32,
    pub(crate) status_y: i32,
    /// The card's inner column, for full-width rules and centred captions.
    pub(crate) content: Rect,
}

/// Request access button height and card side inset.
const PAIRING_BUTTON_H: u32 = 64;
const PAIRING_MARGIN: i32 = 40;

/// The captions between the card's rows. Constants because the layout has to measure them: they
/// wrap to the card's inner column like every other modal's body text, so how many lines each
/// takes decides where the row below it starts.
const PAIRING_BUTTON_CAPTION: &str = "Then approve this TV on the host.";
const PAIRING_PIN_CAPTION: &str = "Enter the PIN shown on the host.";

/// Line spacing within a wrapped caption, matching the status line's.
const CAPTION_LINE_GAP: i32 = 6;

pub(crate) fn layout(card: Rect, fonts: &Fonts) -> PairingLayout {
    let content = Rect::new(
        card.x() + PAIRING_MARGIN,
        card.y(),
        card.width().saturating_sub(PAIRING_MARGIN as u32 * 2),
        0,
    );
    let header_end = ui::modal_header_end_y(fonts.raster, fonts.label, fonts.value, card, SUBTITLE);
    let button = Rect::new(content.x(), header_end + 26, content.width(), PAIRING_BUTTON_H);
    let button_caption_y = button.y() + button.height() as i32 + 12;
    let or_y = button_caption_y + caption_h(fonts, content.width(), PAIRING_BUTTON_CAPTION) + 20;
    let pin_caption_y = or_y + fonts.raster.height(fonts.value) + 20;
    let pin_y = pin_caption_y + caption_h(fonts, content.width(), PAIRING_PIN_CAPTION) + 14;
    let status_y = pin_y + DIGIT_H as i32 + 22;
    PairingLayout {
        button,
        button_caption_y,
        or_y,
        pin_caption_y,
        pin_y,
        status_y,
        content,
    }
}

/// The pairing card, sized from the layout plus room for an up-to-two-line status. Every
/// geometry caller goes through this, so the sizing is done in exactly one place.
pub(crate) fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::simple_modal_card(screen_w, screen_h, |probe| {
        let status_room = 2 * (fonts.raster.height(fonts.value) + 6);
        let status_y = layout(probe, fonts).status_y;
        (status_y + status_room + 26) as u32
    })
}

/// Request access button rect.
pub(crate) fn request_button_rect(card: Rect, fonts: &Fonts) -> Rect {
    layout(card, fonts).button
}

/// PIN row top y-position.
pub(crate) fn pin_row_y(card: Rect, fonts: &Fonts) -> i32 {
    layout(card, fonts).pin_y
}

pub(crate) fn render(
    c: &mut Canvas,
    pin_digits: &[u8; 4],
    status: Option<&String>,
    busy: bool,
    hover_close: bool,
) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts);
    let l = layout(card, c.fonts);
    ui::draw_modal_shell(c.painter, c.text_cache, c.fonts, card, hover_close)?;

    ui::draw_modal_header(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.label,
        c.fonts.value,
        card,
        TITLE,
        ui::WHITE,
        SUBTITLE,
        ui::MUTED,
    )?;

    // Primary first, and visually primary: approving on the host is the path that
    // always works, whereas the PIN needs the host's pairing page open and armed.
    // The shell draws it unfocused-but-filled; the focused copy is a separate
    // `Tile::ModalFocusElement` (see `prepare_tiles`).
    ui::draw_primary_button(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.label,
        l.button,
        REQUEST_LABEL,
    )?;
    draw_centred_caption(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.value,
        l.content,
        l.button_caption_y,
        PAIRING_BUTTON_CAPTION,
    )?;

    ui::draw_or_divider(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.value,
        l.content,
        l.or_y,
        "or",
    )?;

    draw_centred_caption(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.value,
        l.content,
        l.pin_caption_y,
        PAIRING_PIN_CAPTION,
    )?;
    for (i, digit) in pin_digits.iter().enumerate() {
        let rect = digit_rect(card, l.pin_y, i);
        let drawn = ui::draw_card(c.painter, rect, false);
        let text = digit.to_string();
        let tw = c.fonts.raster.measure(c.fonts.title, &text).0;
        ui::draw_text(
            c.painter,
            c.text_cache,
            c.fonts.raster,
            c.fonts.title,
            &text,
            drawn.x() + (drawn.width() as i32 - tw as i32) / 2,
            drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(c.fonts.title)) / 2,
            ui::WHITE,
        )?;
    }

    if let Some(status) = status {
        let color = if busy { ui::MUTED } else { ui::ERROR_RED };
        ui::draw_text_wrapped(
            c.painter,
            c.text_cache,
            c.fonts.raster,
            c.fonts.value,
            status,
            l.content.x(),
            l.status_y,
            l.content.width(),
            color,
            6,
        )?;
    }
    Ok(())
}

/// Height one caption occupies, wrapped to `content_w` — what the row below it is placed from.
fn caption_h(fonts: &Fonts, content_w: u32, text: &str) -> i32 {
    let lines = ui::wrap_text(fonts.raster, fonts.value, text, content_w).len().max(1) as i32;
    lines * fonts.raster.height(fonts.value) + (lines - 1) * CAPTION_LINE_GAP
}

/// Centred caption (the option labels either side of the "or" rule). Wrapped to the card's
/// inner column like every other modal's body text —
/// unwrapped, the longest of them ran past the card edge on a narrower card. Each line is
/// centred individually, so the block stays symmetric.
fn draw_centred_caption(
    painter: &mut Painter,
    text_cache: &mut TextCache,
    raster: &dyn TextRaster,
    font: FontId,
    content: Rect,
    y: i32,
    text: &str,
) -> Result<()> {
    let mut cursor_y = y;
    for line in ui::wrap_text(raster, font, text, content.width()) {
        let w = raster.measure(font, &line).0 as i32;
        ui::draw_text(
            painter,
            text_cache,
            raster,
            font,
            &line,
            content.x() + (content.width() as i32 - w) / 2,
            cursor_y,
            ui::MUTED,
        )?;
        cursor_y += raster.height(font) + CAPTION_LINE_GAP;
    }
    Ok(())
}

/// PIN digit box size/gap — shared by `digit_rect` and the digit
/// tiles so they can never disagree.
pub const DIGIT_W: u32 = 64;
pub const DIGIT_H: u32 = 80;
pub const DIGIT_GAP: i32 = 14;

/// PIN digit `index`'s rect within `card`, given the row's top `y` (from
/// `modal_header_end_y` plus a fixed gap) — the one place this layout formula
/// lives, shared by [`render`] and `app.rs`'s `draw_list`.
pub fn digit_rect(card: Rect, digit_y: i32, index: usize) -> Rect {
    let total_w = 4 * DIGIT_W as i32 + 3 * DIGIT_GAP;
    let start_x = card.x() + (card.width() as i32 - total_w) / 2;
    Rect::new(
        start_x + index as i32 * (DIGIT_W as i32 + DIGIT_GAP),
        digit_y,
        DIGIT_W,
        DIGIT_H,
    )
}

pub const REQUEST_LABEL: &str = "Request access";

/// One PIN digit, focused, as its own zoom-animated tile — composited by the
/// GPU over the shell's unfocused digit boxes, same pattern as
/// `render_focus_row_tile`.
pub fn render_digit_tile(
    text_cache: &mut ui::TextCache,
    raster: &dyn TextRaster,
    font_title: FontId,
    digit: u8,
) -> Result<ui::Painter> {
    ui::render_card_text_tile(text_cache, raster, font_title, &digit.to_string(), DIGIT_W, DIGIT_H)
}

/// The "Request access" button, focused, as its own zoom-animated tile — accent-filled
/// like the shell's copy (see `ui::draw_primary_button`), not the surface-card treatment
/// the digit tiles use, so the primary action keeps its emphasis while focused.
pub fn render_button_tile(
    text_cache: &mut ui::TextCache,
    raster: &dyn TextRaster,
    font_label: FontId,
    w: u32,
    h: u32,
) -> Result<ui::Painter> {
    let pad = ui::ROW_TILE_PAD;
    let mut p = ui::Painter::new(w + 2 * pad as u32, h + 2 * pad as u32);
    ui::draw_primary_button(
        &mut p,
        text_cache,
        raster,
        font_label,
        Rect::new(pad, pad, w, h),
        REQUEST_LABEL,
    )?;
    Ok(p)
}
