//! The per-host actions menu — presentation. Which actions exist, and their labels, is
//! logic and lives in `app::state::hostmenu`; this only lays them out and paints them.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow, Fonts};
use anyhow::Result;

pub(crate) fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str, rows: usize) -> Rect {
    ui::list_modal_card_rect(screen_w, screen_h, fonts, subtitle, rows)
}

pub(crate) fn render(c: &mut Canvas, title: &str, subtitle: &str, rows: &[FocusRow], hover_close: bool) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts, subtitle, rows.len());
    ui::render_list_modal_screen(c, card, title, subtitle, rows, hover_close)
}
