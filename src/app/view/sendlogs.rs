//! "Send logs to developer?" confirmation. Logic lives in `app::state::sendlogs`.
use crate::ui::{self, Canvas, ConfirmButton};
use anyhow::Result;

pub const TITLE: &str = "Send logs to developer?";
pub const SUBTITLE: &str = "This uploads this session's log file to the app developer to help diagnose problems. \
     Logs can include host names, IP addresses, and game titles. Only send them if you're \
     comfortable sharing that.";

/// Order matches the screen's focus index (0 = Send, 1 = Cancel); Send is drawn in the
/// same red as Forget, since both are consequential.
pub fn buttons() -> [ConfirmButton<'static>; 2] {
    ui::confirm_buttons(Some(ui::ICON_SEND), "Send", ui::ERROR_RED)
}

pub fn render(c: &mut Canvas, hover_close: bool) -> Result<()> {
    let (card, content) = ui::confirm_dialog_layout(c.screen_w, c.screen_h, c.fonts, SUBTITLE);
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
    ui::draw_confirm_buttons(c.painter, c.text_cache, c.fonts, content, &buttons(), usize::MAX)
}
