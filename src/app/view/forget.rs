//! "Forget this host?" confirmation. Logic lives in `app::state::forget`.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, ConfirmButton, ModalScreen};
use anyhow::Result;

pub const TITLE: &str = "Forget this host?";

pub fn subtitle(host_name: &str) -> String {
    format!("{host_name} will be removed from this TV. You can pair with it again later.")
}

/// The Forget/Cancel pair — shared by the shell and the focused-button tile, so their
/// `ConfirmButton` data can't drift apart.
pub fn buttons() -> [ConfirmButton<'static>; 2] {
    ui::confirm_buttons(Some(ui::ICON_DELETE), "Forget", ui::ERROR_RED)
}

/// The forget-host confirmation as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub host_name: &'a str,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &ui::Fonts) -> Rect {
        ui::confirm_dialog_card(screen_w, screen_h, fonts, &subtitle(self.host_name))
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let subtitle = subtitle(self.host_name);
        let (card, content) = ui::confirm_dialog_layout(c.screen_w, c.screen_h, c.fonts, &subtitle);
        c.modal_shell(card, hover_close)?;
        c.modal_header(card, TITLE, ui::WHITE, &subtitle, ui::MUTED)?;
        // `usize::MAX` = nothing focused here; the focused button is its own tile.
        c.confirm_buttons(content, &buttons(), usize::MAX)
    }
}
