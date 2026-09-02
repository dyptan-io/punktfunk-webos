//! Diagnostics: debug aids, one list modal. Logic lives in `app::state::diagnostics`.
use crate::app::menu;
use crate::services::store::Settings;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub const TITLE: &str = "Diagnostics";
pub const SUBTITLE: &str = "Debug aids for on-device investigation.";

/// Log level (dropdown), stats overlay + show logs (toggles), send logs (action).
/// Order must match `menu::DIAG_ROW_*`.
pub fn rows(settings: &Settings, to_host: bool) -> Vec<FocusRow> {
    vec![
        FocusRow::dropdown(
            crate::app::view::icons::ICON_BUG,
            "Log level",
            menu::log_level_label(settings.log_level_override),
        ),
        FocusRow::toggle(
            crate::app::view::icons::ICON_CHART,
            "Stats overlay",
            settings.stats_overlay,
        )
        .with_subtext_opt(
            settings
                .stats_overlay
                .then(|| ui::widgets::RowSubtext::hint("Or use the Green button")),
        ),
        FocusRow::toggle(
            crate::app::view::icons::ICON_VISIBILITY,
            "Show logs",
            settings.show_logs,
        )
        .with_subtext_opt(
            settings
                .show_logs
                .then(|| ui::widgets::RowSubtext::hint("Or use the Yellow button")),
        ),
        FocusRow::action(crate::app::view::icons::ICON_SEND, "Send logs").with_subtext(if to_host {
            ui::widgets::RowSubtext::hint("Will be sent to the host")
        } else {
            ui::widgets::RowSubtext::caution("Host is unavailable — send to developer")
        }),
    ]
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::widgets::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, menu::DIAGNOSTICS_ROW_COUNT)
}

/// The diagnostics list as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub settings: &'a Settings,
    pub to_host: bool,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts)
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        Some(ui::widgets::list_modal_content_rect(
            card,
            fonts,
            SUBTITLE,
            menu::DIAGNOSTICS_ROW_COUNT,
        ))
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.list_modal_screen(card, TITLE, SUBTITLE, &rows(self.settings, self.to_host), hover_close)
    }
}
