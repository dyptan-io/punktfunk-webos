//! Cursor: how the pointer behaves in a stream. Logic lives in `app::state::cursorsettings`.
use crate::app::menu;
use crate::services::store::Settings;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalScreen;
use anyhow::Result;

pub const TITLE: &str = "Cursor";
pub const SUBTITLE: &str = "How the pointer behaves in a stream.";

/// Order must match `menu::CURSOR_ROW_*`.
pub fn rows(settings: &Settings) -> Vec<FocusRow> {
    vec![
        FocusRow::toggle(crate::app::view::icons::ICON_MOUSE, "Capture", settings.cursor_capture).with_subtext(
            ui::widgets::RowSubtext::hint(if settings.cursor_capture {
                "Capture (games)"
            } else {
                "Desktop (absolute)"
            }),
        ),
        FocusRow::toggle(
            crate::app::view::icons::ICON_TOUCH,
            "Gestures",
            settings.cursor_gestures,
        )
        .with_subtext(ui::widgets::RowSubtext::hint(
            "Hold OK to right-click or red remote button",
        )),
    ]
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::widgets::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, menu::CURSOR_ROW_COUNT)
}

/// The cursor settings list as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub settings: &'a Settings,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts)
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        Some(ui::widgets::list_modal_content_rect(
            card,
            fonts,
            SUBTITLE,
            rows(self.settings).len(),
        ))
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.list_modal_screen(card, TITLE, SUBTITLE, &rows(self.settings), hover_close)
    }
}
