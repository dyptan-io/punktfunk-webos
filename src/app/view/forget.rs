//! "Forget this host?" confirmation. Logic lives in `app::state::forget`.
use crate::ui::{self, Canvas, ConfirmButton};
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

pub fn render(c: &mut Canvas, host_name: &str, hover_close: bool) -> Result<()> {
    let subtitle = subtitle(host_name);
    let (card, content) = ui::confirm_dialog_layout(c.screen_w, c.screen_h, c.fonts, &subtitle);
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
        &subtitle,
        ui::MUTED,
    )?;
    // `usize::MAX` = nothing focused here; the focused button is its own tile.
    ui::draw_confirm_buttons(c.painter, c.text_cache, c.fonts, content, &buttons(), usize::MAX)
}
