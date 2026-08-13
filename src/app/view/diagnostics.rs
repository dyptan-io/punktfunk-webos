//! Diagnostics: debug aids, one list modal. Logic lives in `app::state::diagnostics`.
use crate::app::menu;
use crate::services::store::Settings;
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow, Fonts};
use anyhow::Result;

pub const TITLE: &str = "Diagnostics";
pub const SUBTITLE: &str = "Debug aids for on-device investigation.";

/// Log level (dropdown), stats overlay + show logs (toggles), send logs (action).
/// Order must match `menu::DIAG_ROW_*`.
pub fn rows(settings: &Settings) -> Vec<FocusRow> {
    vec![
        FocusRow {
            icon: ui::ICON_BUG,
            label: "Log level".into(),
            value: menu::log_level_label(settings.log_level_override).into(),
            kind: ui::RowKind::Dropdown,
            fraction: 0.0,
            danger: false,
            menu: None,
            subtext: None,
        },
        FocusRow {
            icon: ui::ICON_CHART,
            label: "Stats overlay".into(),
            value: if settings.stats_overlay {
                "On".into()
            } else {
                "Off".into()
            },
            kind: ui::RowKind::Toggle,
            fraction: 0.0,
            danger: false,
            menu: None,
            subtext: settings
                .stats_overlay
                .then(|| ui::RowSubtext::hint("Or use the Green button")),
        },
        FocusRow {
            icon: ui::ICON_VISIBILITY,
            label: "Show logs".into(),
            value: if settings.show_logs { "On".into() } else { "Off".into() },
            kind: ui::RowKind::Toggle,
            fraction: 0.0,
            danger: false,
            menu: None,
            subtext: settings
                .show_logs
                .then(|| ui::RowSubtext::hint("Or use the Yellow button")),
        },
        FocusRow::action(ui::ICON_SEND, "Send logs to developer")
            .with_subtext(ui::RowSubtext::hint("If a developer asked you to")),
    ]
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, menu::DIAGNOSTICS_ROW_COUNT)
}

pub fn render(c: &mut Canvas, settings: &Settings, hover_close: bool) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts);
    ui::render_list_modal_screen(c, card, TITLE, SUBTITLE, &rows(settings), hover_close)
}
