//! Cursor: how the pointer behaves in a stream. Logic lives in `app::state::cursorsettings`.
use crate::app::menu;
use crate::services::store::Settings;
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow, Fonts};
use anyhow::Result;

pub const TITLE: &str = "Cursor";
pub const SUBTITLE: &str = "How the pointer behaves in a stream.";

/// Order must match `menu::CURSOR_ROW_*`.
pub fn rows(settings: &Settings) -> Vec<FocusRow> {
    vec![
        FocusRow::toggle(ui::ICON_MOUSE, "Capture", settings.cursor_capture).with_subtext(ui::RowSubtext::hint(
            if settings.cursor_capture {
                "Capture (games)"
            } else {
                "Desktop (absolute)"
            },
        )),
        FocusRow::toggle(ui::ICON_TOUCH, "Gestures", settings.cursor_gestures)
            .with_subtext(ui::RowSubtext::hint("Hold OK to right-click or red remote button")),
    ]
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, menu::CURSOR_ROW_COUNT)
}

pub fn render(c: &mut Canvas, settings: &Settings, hover_close: bool) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts);
    ui::render_list_modal_screen(c, card, TITLE, SUBTITLE, &rows(settings), hover_close)
}
