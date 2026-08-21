//! The one-field text form — presentation, shared by Add host, Edit address and naming a
//! collection. Logic lives in `app::state::addhost` / `app::state::edithost` /
//! `app::state::collections`.
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) const ADD_TITLE: &str = "Add host";
pub(crate) const EDIT_TITLE: &str = "Edit address";
pub(crate) const ADD_SUBTITLE: &str = "Enter the host's IP address. Right adds an optional port.";

/// Edit gets its own subtitle rather than reusing the Add one, which would overflow the
/// card once the host's name is in it.
pub(crate) fn edit_subtitle(host_name: &str) -> String {
    format!("New IP address for {host_name}. Its pairing is kept.")
}

/// The caution line under the field, when the form has one — a refused name says why rather
/// than greying its confirm with no explanation.
const HINT_H: i32 = 34;

/// Lifted clear of the on-screen keyboard when it's up.
pub(crate) fn card_rect(
    screen_w: u32,
    screen_h: u32,
    fonts: &Fonts,
    subtitle: &str,
    hint: bool,
    keyboard_shown: bool,
) -> Rect {
    ui::widgets::simple_modal_card_above_keyboard(screen_w, screen_h, keyboard_shown, |probe| {
        let header_end = ui::text::modal_header_end_y(fonts, probe, subtitle);
        (header_end + 20 + 80 + 32 + if hint { HINT_H } else { 0 }) as u32 // field + hint + bottom margin
    })
}

/// The text field, also handed to `SDL_SetTextInputRect` (which the webOS OSK ignores).
pub(crate) fn field_rect(
    screen_w: u32,
    screen_h: u32,
    fonts: &Fonts,
    subtitle: &str,
    hint: bool,
    keyboard_shown: bool,
) -> Rect {
    let card = card_rect(screen_w, screen_h, fonts, subtitle, hint, keyboard_shown);
    let after_subtitle_y = ui::text::modal_header_end_y(fonts, card, subtitle);
    Rect::new(
        card.x() + 32,
        after_subtitle_y + 20,
        card.width().saturating_sub(64),
        80,
    )
}

/// The text form as a [`ModalScreen`]. Every screen that types into one field shares it; the
/// caller passes the copy that tells them apart, and the hint that says why what is typed
/// cannot be committed yet.
pub(crate) struct Modal<'a> {
    pub title: &'static str,
    pub subtitle: String,
    pub typed: &'a str,
    pub hint: Option<&'a str>,
    pub keyboard_shown: bool,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(
            screen_w,
            screen_h,
            fonts,
            &self.subtitle,
            self.hint.is_some(),
            self.keyboard_shown,
        )
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        let (title, subtitle, typed) = (self.title, self.subtitle.as_str(), self.typed);
        c.modal_shell(card, hover_close)?;
        let after_subtitle_y = c.modal_header(
            card,
            title,
            ui::theme::palette().text,
            subtitle,
            ui::theme::palette().muted,
        )?;
        let field = Rect::new(
            card.x() + 32,
            after_subtitle_y + 20,
            card.width().saturating_sub(64),
            80,
        );
        let drawn = c.painter.card(field, true);
        let text_x = drawn.x() + 24;
        let text_w = c.fonts.raster.measure(c.fonts.title, typed).0;
        c.text(
            c.fonts.title,
            typed,
            text_x,
            drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(c.fonts.title)) / 2,
            ui::theme::palette().text,
        )?;
        // A blinkless text-cursor bar right after what's typed so far — there's no fixed-width
        // mask anymore to show *where* editing happens, so this stands in for it.
        let caret = Rect::new(
            text_x + text_w as i32 + 6,
            drawn.y() + 16,
            3,
            drawn.height().saturating_sub(32),
        );
        c.painter.fill_rect(caret, ui::theme::palette().accent_bright);
        if let Some(hint) = self.hint {
            c.text(
                c.fonts.value,
                hint,
                text_x,
                drawn.bottom() + 10,
                ui::theme::palette().error,
            )?;
        }
        Ok(())
    }
}
