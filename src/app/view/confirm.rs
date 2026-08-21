//! The two-button confirm dialog, as a screen — the shell every one of them draws.
//!
//! What differs between Forget host, Send logs, a Wake prompt and a finished speed test is a
//! title and a [`Confirm`]; the card, the header and the button row are this. The screens with
//! a button-less state of their own (Wake without a MAC, a test still running) keep their own
//! `Modal` and take their buttons from the same descriptor.
use crate::app::screens::confirm::Confirm;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) struct Modal<'a> {
    pub title: &'a str,
    pub confirm: &'a Confirm,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        ui::tiles::confirm_dialog_card(screen_w, screen_h, fonts, &self.confirm.subtitle)
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let (card, content) = ui::tiles::confirm_dialog_layout(c.screen_w, c.screen_h, c.fonts, &self.confirm.subtitle);
        c.modal_shell(card, hover_close)?;
        c.modal_header(
            card,
            self.title,
            ui::theme::palette().text,
            &self.confirm.subtitle,
            ui::theme::palette().muted,
        )?;
        // Every button drawn unfocused: the focused one is composited from its own tile.
        c.render(ui::widgets::ConfirmButtons::new(&self.confirm.widgets()), content)
    }
}
