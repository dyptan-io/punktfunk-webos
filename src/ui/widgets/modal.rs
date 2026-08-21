use crate::ui::prelude::*;
use anyhow::Result;

/// A centered glass card of `(width_frac * screen_w, height)` — a centring spacer either
/// side of the card on both axes, which is all "centred" is.
pub fn modal_card_rect(screen_w: u32, screen_h: u32, width_frac: f32, height: u32) -> Rect {
    center(Rect::new(0, 0, screen_w, screen_h), width_frac, height, 0)
}

/// The card `width_frac` wide and `height` tall, centred in `within`, with the top spacer
/// never shorter than `min_top`.
fn center(within: Rect, width_frac: f32, height: u32, min_top: u32) -> Rect {
    let column = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Percentage((width_frac * 100.0).round() as u16),
        Constraint::Fill(1),
    ])
    .split(within)[1];
    Layout::vertical([Constraint::Min(min_top), Constraint::Length(height), Constraint::Min(0)]).split(column)[1]
}

/// How much of the panel webOS's on-screen keyboard occupies, measured on-device: it
/// takes roughly the bottom half and its height barely varies between layouts.
///
/// This has to be a constant because SDL gives us no way to ask. `SDL_webOS.h` publishes
/// only cursor / panel-resolution / refresh-rate / exported-window calls,
/// `SDL_IsScreenKeyboardShown` is a bare bool, and the fork's internal `input_panel_rect`
/// isn't reachable from the public API. `SDL_SetTextInputRect` (which this app does set,
/// to the field) is the correct contract for "keep this region clear", but the webOS OSK
/// ignores it.
pub const KEYBOARD_PANEL_FRAC: f32 = 0.5;

/// Smallest gap left above a card that has been lifted clear of the keyboard, so a tall
/// one can't be pushed off the top of the panel.
const KEYBOARD_MIN_TOP: u32 = 24;

/// A modal card centred in whatever space the on-screen keyboard leaves.
///
/// With the keyboard down this is just [`modal_card_rect`] — the card sits where every
/// other modal sits. With it up, the card is centred in the band above the panel rather
/// than pinned to the very top: the point is to clear the keyboard, not to jam the card
/// against the screen edge.
pub fn modal_card_rect_above_keyboard(
    screen_w: u32,
    screen_h: u32,
    width_frac: f32,
    height: u32,
    keyboard_shown: bool,
) -> Rect {
    if !keyboard_shown {
        return modal_card_rect(screen_w, screen_h, width_frac, height);
    }
    let available = (screen_h as f32 * (1.0 - KEYBOARD_PANEL_FRAC)).round() as u32;
    center(
        Rect::new(0, 0, screen_w, available),
        width_frac,
        height,
        KEYBOARD_MIN_TOP,
    )
}

impl Painter {
    /// Draws a hairline (`Theme::rule`) `width` px wide at `(x, y)`.
    pub fn rule(&mut self, x: i32, y: i32, width: u32) {
        self.fill_rect(Rect::new(x, y, width, 1), theme().rule);
    }

    /// The modal card surface, opaque — for the in-stream dialogs, which have no frost
    /// beneath them to show through (the video is on a hardware plane below the SDL
    /// surface, so it is not in the framebuffer this composites into).
    pub fn modal_card(&mut self, rect: Rect) {
        self.panel_in(rect, MODAL_RADIUS, theme().panel);
    }

    /// The same card in [`Theme::panel_glass`](crate::ui::style::Theme::panel_glass) — for a
    /// menu modal, whose compositor draws a `DrawCmd::Frost` of the same rect and radius
    /// underneath it.
    pub fn modal_card_glass(&mut self, rect: Rect) {
        self.glass_panel(rect, MODAL_RADIUS);
    }

    /// Every raised glass surface in the menus: a shadow, the shared
    /// [`Theme::panel_glass`](crate::ui::style::Theme::panel_glass) fill and the shared
    /// [`Theme::glass_edge`](crate::ui::style::Theme::glass_edge) hairline, at `radius`.
    ///
    /// The modal card, a dropdown's popup and a toast are the same material at different
    /// sizes; each used to mix its own fill and its own white for the edge, which is only
    /// invisible until two of them are on screen together.
    pub fn glass_panel(&mut self, rect: Rect, radius: i32) {
        self.panel_in(rect, radius, crate::ui::style::glass_fill());
    }

    /// [`Self::glass_panel`] in a fill of its own — for a surface that has to sit darker than
    /// the shared glass, like the dropdown popup over a lit settings row.
    pub fn panel_in(&mut self, rect: Rect, radius: i32, fill: Color) {
        self.card_shadow(rect, radius);
        self.glass_face(rect, radius, fill);
    }

    /// [`Self::glass_panel`] without the drop shadow — for a surface drawn into a tile sized to
    /// the panel exactly, where every shadow pixel would fall outside the canvas or be
    /// overpainted by the fill, and the blur that produced it is pure waste.
    pub fn glass_face(&mut self, rect: Rect, radius: i32, fill: Color) {
        self.fill_rounded_rect(rect, radius, fill);
        self.stroke_rounded_rect(rect, radius, theme().glass_edge, 1.5);
    }
}

impl Canvas<'_, '_> {
    /// Shared modal chrome — the rounded card and its close (X) button — that every
    /// screen's renderer draws before its own content inside `card`.
    ///
    /// No backdrop: the scrim behind the modal is a GPU fill in the composed frame (it fades
    /// in with the modal), and this painter is the modal's own transparent tile. The card is
    /// glass — `compose_modal_card` pushes the matching `DrawCmd::Frost` under this tile.
    pub fn modal_shell(&mut self, card: Rect, hover_close: bool) -> Result<()> {
        self.painter.modal_card_glass(card);
        let color = if hover_close { theme().text } else { theme().muted };
        self.icon(modal_close_rect(card), icons().close, color)
    }
}

/// Width fraction shared by the confirm-style modals (forget host, send logs, stop
/// streaming, quit app) — narrower than the scrollable `ListModal` screens.
pub const SIMPLE_MODAL_WIDTH_FRAC: f32 = 0.40;

/// [`simple_modal_card`], lifted clear of the on-screen keyboard when it is up — for the
/// modals with a text field, which the panel would otherwise cover.
pub fn simple_modal_card_above_keyboard(
    screen_w: u32,
    screen_h: u32,
    keyboard_shown: bool,
    content_height: impl FnOnce(Rect) -> u32,
) -> Rect {
    let w = (screen_w as f32 * SIMPLE_MODAL_WIDTH_FRAC).round() as u32;
    let height = content_height(Rect::new(0, 0, w, 0));
    modal_card_rect_above_keyboard(screen_w, screen_h, SIMPLE_MODAL_WIDTH_FRAC, height, keyboard_shown)
}

/// A centered [`SIMPLE_MODAL_WIDTH_FRAC`]-wide card whose *height* is derived from its
/// own content: `content_height` receives a zero-y/height probe card at the final width
/// and returns the card's total height. Shared by every confirm modal so they size
/// identically whether they render through `App` (forget/send-logs) or `main.rs`'s
/// in-stream/quit dialog.
pub fn simple_modal_card(screen_w: u32, screen_h: u32, content_height: impl FnOnce(Rect) -> u32) -> Rect {
    let w = (screen_w as f32 * SIMPLE_MODAL_WIDTH_FRAC).round() as u32;
    let height = content_height(Rect::new(0, 0, w, 0));
    modal_card_rect(screen_w, screen_h, SIMPLE_MODAL_WIDTH_FRAC, height)
}

pub fn modal_close_rect(card_rect: Rect) -> Rect {
    const SIZE: u32 = 44;
    const MARGIN: i32 = 20;
    Rect::new(
        card_rect.right() - MARGIN - SIZE as i32,
        card_rect.y() + MARGIN,
        SIZE,
        SIZE,
    )
}

impl Canvas<'_, '_> {
    /// A horizontal rule broken by a centred word — the "or" between two mutually exclusive
    /// choices. Drawn in the value font, like the body copy it separates.
    ///
    /// The pairing card offers two independent ways to pair, and a sentence saying so is not
    /// enough: without a visual break the two blocks read as *steps*, i.e. "fill in the PIN,
    /// then press the button". The rule makes the exclusivity structural rather than something
    /// the user has to read and remember.
    pub fn or_divider(&mut self, content: Rect, y: i32, word: &str) -> Result<()> {
        let font = self.fonts.value;
        let word_w = self.fonts.raster.measure(font, word).0 as i32;
        let gap = 18i32;
        let line_y = y + self.fonts.raster.height(font) / 2;
        let half = (content.width() as i32 - word_w - 2 * gap) / 2;
        if half > 0 {
            self.painter.rule(content.x(), line_y, half as u32);
            self.painter.rule(content.right() - half, line_y, half as u32);
        }
        self.text_centered(font, word, content, y, theme().muted)?;
        Ok(())
    }

    /// The accent-filled primary action button. Labelled in the label font, like every
    /// other button.
    pub fn primary_button(&mut self, rect: Rect, label: &str) -> Result<()> {
        let font = self.fonts.label;
        self.painter.card_shadow(rect, CARD_RADIUS);
        self.painter.fill_rounded_rect(rect, CARD_RADIUS, theme().accent);
        let text_y = rect.y() + (rect.height() as i32 - self.fonts.raster.height(font)) / 2;
        self.text_centered(font, label, rect, text_y, theme().text)?;
        Ok(())
    }
}
