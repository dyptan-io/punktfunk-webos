//! The "you can only pin N games" alert. Logic lives in `app::state::pinlimit`.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, ConfirmButton, Fonts, ModalScreen};
use anyhow::Result;

pub const TITLE: &str = "Pin limit reached";

/// The single OK button's fixed size.
const BUTTON_W: u32 = 200;
const BUTTON_H: u32 = 72;

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, message: &str) -> Rect {
    ui::simple_modal_card(screen_w, screen_h, |probe| {
        let header_end = ui::modal_header_end_y(fonts.raster, fonts.label, fonts.value, probe, message);
        (header_end + 32 + BUTTON_H as i32 + 32) as u32
    })
}

/// The "too many PIN attempts" notice as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub message: &'a str,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts, self.message)
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.modal_shell(card, hover_close)?;
        c.modal_header(card, TITLE, ui::WHITE, self.message, ui::MUTED)?;
        let after_subtitle_y = ui::modal_header_end_y(c.fonts.raster, c.fonts.label, c.fonts.value, card, self.message);
        // Single centred button, always focused (no separate focus tile).
        let button = Rect::new(
            card.x() + (card.width() as i32 - BUTTON_W as i32) / 2,
            after_subtitle_y + 32,
            BUTTON_W,
            BUTTON_H,
        );
        c.confirm_button(
            &ConfirmButton {
                icon: None,
                label: "OK",
                color: ui::WHITE,
            },
            true,
            button,
        )
    }
}
