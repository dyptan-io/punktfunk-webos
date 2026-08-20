//! Pairing modal rendering. Logic lives in `app::state::pairing`.
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
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
    let header_end = ui::text::modal_header_end_y(fonts, card, SUBTITLE);
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
    ui::widgets::simple_modal_card(screen_w, screen_h, |probe| {
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

/// Height one caption occupies, wrapped to `content_w` — what the row below it is placed from.
fn caption_h(fonts: &Fonts, content_w: u32, text: &str) -> i32 {
    let lines = ui::text::wrap_text(fonts.raster, fonts.value, text, content_w)
        .len()
        .max(1) as i32;
    lines * fonts.raster.height(fonts.value) + (lines - 1) * CAPTION_LINE_GAP
}

/// Centred caption (the option labels either side of the "or" rule). Wrapped to the card's
/// inner column like every other modal's body text —
/// unwrapped, the longest of them ran past the card edge on a narrower card. Each line is
/// centred individually, so the block stays symmetric.
fn draw_centred_caption(c: &mut Canvas, content: Rect, y: i32, text: &str) -> Result<()> {
    let (raster, font) = (c.fonts.raster, c.fonts.value);
    let mut cursor_y = y;
    for line in ui::text::wrap_text(raster, font, text, content.width()) {
        c.text_centered(font, &line, content, cursor_y, ui::style::theme().muted)?;
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
/// lives, shared by [`Modal::render`] and `app.rs`'s `draw_list`.
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

/// The "Request access" button, focused, as its own zoom-animated tile — accent-filled like
/// the shell's copy (see `ui::Canvas::primary_button`), not the surface-card treatment the
/// digit tiles use, so the primary action keeps its emphasis while focused.
pub struct RequestButtonTile {
    pub w: u32,
    pub h: u32,
}

impl ui::Widget for RequestButtonTile {
    fn render(self, area: ui::render::Rect, c: &mut ui::Canvas) -> Result<()> {
        c.primary_button(area.inflate(-ui::tiles::ROW_TILE_PAD), REQUEST_LABEL)
    }
}

impl ui::TileWidget for RequestButtonTile {
    fn size(&self, _fonts: &ui::text::Fonts) -> (u32, u32) {
        ui::tiles::padded_size(self.w, self.h, ui::tiles::ROW_TILE_PAD)
    }
}

/// The pairing modal as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub pin_digits: &'a [u8; 4],
    pub status: Option<&'a String>,
    pub busy: bool,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts)
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let (pin_digits, status, busy) = (self.pin_digits, self.status, self.busy);
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        let l = layout(card, c.fonts);
        c.modal_shell(card, hover_close)?;

        c.modal_header(card, TITLE, ui::style::theme().text, SUBTITLE, ui::style::theme().muted)?;

        // Primary first, and visually primary: approving on the host is the path that
        // always works, whereas the PIN needs the host's pairing page open and armed.
        // The shell draws it unfocused-but-filled; the focused copy is a separate
        // `tile::MODAL_FOCUS` (see `prepare_tiles`).
        c.primary_button(l.button, REQUEST_LABEL)?;
        draw_centred_caption(c, l.content, l.button_caption_y, PAIRING_BUTTON_CAPTION)?;

        c.or_divider(l.content, l.or_y, "or")?;

        draw_centred_caption(c, l.content, l.pin_caption_y, PAIRING_PIN_CAPTION)?;
        for (i, digit) in pin_digits.iter().enumerate() {
            let rect = digit_rect(card, l.pin_y, i);
            let drawn = c.painter.card(rect, false);
            let text_y = drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(c.fonts.title)) / 2;
            c.text_centered(
                c.fonts.title,
                &digit.to_string(),
                drawn,
                text_y,
                ui::style::theme().text,
            )?;
        }

        if let Some(status) = status {
            let color = if busy {
                ui::style::theme().muted
            } else {
                ui::style::theme().error
            };
            c.text_wrapped(
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
}
