//! The per-host actions menu — presentation. Which actions exist, and their labels, is
//! logic and lives in `app::state::hostmenu`; this only lays them out and paints them.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow, Fonts, ModalScreen};
use anyhow::Result;

pub(crate) fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str, rows: usize) -> Rect {
    ui::list_modal_card_rect(screen_w, screen_h, fonts, subtitle, rows)
}

/// The per-host actions menu as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub title: &'a str,
    pub subtitle: String,
    pub rows: Vec<FocusRow>,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts, &self.subtitle, self.rows.len())
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        Some(ui::list_modal_content_rect(
            card,
            fonts,
            &self.subtitle,
            self.rows.len(),
        ))
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.list_modal_screen(card, self.title, &self.subtitle, &self.rows, hover_close)
    }
}
