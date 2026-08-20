//! The per-host actions menu — presentation. Which actions exist, and their labels, is
//! logic and lives in `app::state::hostmenu`; this only lays them out and paints them.
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str, rows: usize) -> Rect {
    ui::widgets::list_modal_card_rect(screen_w, screen_h, fonts, subtitle, rows)
}

/// What the menu's card geometry is measured from: the subtitle it wraps and how many rows
/// it holds. Separate from [`Modal`] so the hit tests never build the row labels — see
/// [`ModalMetrics`].
pub(crate) struct Metrics<'a> {
    pub subtitle: &'a str,
    pub rows: usize,
}

impl ModalMetrics for Metrics<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts, self.subtitle, self.rows)
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        Some(ui::widgets::list_modal_content_rect(
            card,
            fonts,
            self.subtitle,
            self.rows,
        ))
    }
}

/// The per-host actions menu as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub title: &'a str,
    pub subtitle: String,
    pub rows: Vec<FocusRow>,
}

impl Modal<'_> {
    fn metrics(&self) -> Metrics<'_> {
        Metrics {
            subtitle: &self.subtitle,
            rows: self.rows.len(),
        }
    }
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        self.metrics().card_rect(screen_w, screen_h, fonts)
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        self.metrics().content_rect(card, fonts)
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.list_modal_screen(card, self.title, &self.subtitle, &self.rows, hover_close)
    }
}
