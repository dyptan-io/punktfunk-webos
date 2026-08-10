//! Cursor screen rendering. Logic lives in `app::state::cursorsettings`.
use crate::app::App;
use crate::ui::render::Rect;
use crate::ui::{self, FocusRow, Painter};
use anyhow::Result;

impl App {
    pub(crate) fn cursor_settings_rows(&self) -> Vec<FocusRow> {
        ui::cursor_rows(&self.settings)
    }

    pub(crate) fn cursor_settings_subtitle(&self) -> String {
        "How the pointer behaves in a stream.".to_string()
    }

    pub(crate) fn cursor_settings_card_rect(screen_w: u32, screen_h: u32, fonts: &ui::Fonts, subtitle: &str) -> Rect {
        ui::list_modal_card_rect(screen_w, screen_h, fonts, subtitle, ui::CURSOR_ROW_COUNT)
    }

    pub(crate) fn render_cursor_settings(
        &self,
        painter: &mut Painter,
        text_cache: &mut crate::ui::TextCache,
        fonts: &ui::Fonts,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<()> {
        let subtitle = self.cursor_settings_subtitle();
        let rows = self.cursor_settings_rows();
        let card = Self::cursor_settings_card_rect(screen_w, screen_h, fonts, &subtitle);
        self.draw_modal_shell(painter, text_cache, fonts.raster, fonts.icon, card)?;
        ui::render_list_modal(painter, text_cache, fonts, card, "Cursor", &subtitle, &rows)
    }
}
