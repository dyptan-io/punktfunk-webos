//! "Send logs to developer?" confirmation. Logic lives in `app::state::sendlogs`.
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::widgets::ConfirmButton;
use crate::ui::Canvas;
use crate::ui::ModalScreen;
use anyhow::Result;

pub const TITLE: &str = "Send logs to developer?";
pub const SUBTITLE: &str = "This uploads this session's log file to the app developer to help diagnose problems. \
     Logs can include host names, IP addresses, and game titles. Only send them if you're \
     comfortable sharing that.";

/// Order matches the screen's focus index (0 = Send, 1 = Cancel); Send is drawn in the
/// same red as Forget, since both are consequential.
pub fn buttons() -> [ConfirmButton<'static>; 2] {
    ui::widgets::confirm_buttons(
        Some(crate::app::view::icons::ICON_SEND),
        "Send",
        ui::style::theme().error,
    )
}

/// The send-logs confirmation as a [`ModalScreen`].
pub(crate) struct Modal;

impl ModalScreen for Modal {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Rect {
        ui::tiles::confirm_dialog_card(screen_w, screen_h, fonts, SUBTITLE)
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let (card, content) = ui::tiles::confirm_dialog_layout(c.screen_w, c.screen_h, c.fonts, SUBTITLE);
        c.modal_shell(card, hover_close)?;
        c.modal_header(card, TITLE, ui::style::theme().text, SUBTITLE, ui::style::theme().muted)?;
        c.render(ui::widgets::ConfirmButtons::new(&buttons()), content)
    }
}
