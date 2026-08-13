//! Per-host Wake-on-LAN settings. Logic lives in `app::state::wakesettings`.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow, Fonts};
use anyhow::Result;

/// Spells out both halves of the behaviour, because the alternative to "On" is not
/// "never wake" — it's "ask first", which the switch alone can't say.
pub const SUBTITLE: &str = "On: an unreachable host is sent a wake signal straight away, retried every \
     minute until it answers. Off: it asks first.";
pub const ROW_COUNT: usize = 1;

pub fn title(host_name: &str) -> String {
    format!("Wake · {host_name}")
}

pub fn rows(auto_send: bool) -> Vec<FocusRow> {
    vec![FocusRow::toggle(ui::ICON_POWER, "Wake automatically", auto_send)]
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, ROW_COUNT)
}

pub fn render(c: &mut Canvas, host_name: &str, auto_send: bool, hover_close: bool) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts);
    ui::render_list_modal_screen(c, card, &title(host_name), SUBTITLE, &rows(auto_send), hover_close)
}
