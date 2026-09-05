//! Controller: the pad, and which menus it drives. Logic lives in
//! `app::state::controllersettings`.
//!
//! Rows come from `view::settings::rows_for` rather than being built here, so their labels,
//! locks and captions are the main list's — this screen is a grouping, not a second wording of
//! the same settings.
use crate::app::menu;
use crate::services::store::{GamepadType, Settings};
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub const TITLE: &str = "Controller";
pub const SUBTITLE: &str = "Your pad, and which menus it drives.";

/// Order must match [`menu::CONTROLLER_ROWS`].
pub fn rows(
    settings: &Settings,
    detected_gamepad_type: Option<GamepadType>,
    dualsense_limited: bool,
    webos_major: Option<u32>,
) -> Vec<FocusRow> {
    super::settings::rows_for(
        menu::CONTROLLER_ROWS.iter().copied(),
        settings,
        detected_gamepad_type,
        dualsense_limited,
        webos_major,
    )
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::widgets::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, menu::CONTROLLER_ROWS.len())
}

/// The controller settings list as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub settings: &'a Settings,
    pub detected_gamepad_type: Option<GamepadType>,
    pub dualsense_limited: bool,
    pub webos_major: Option<u32>,
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
            menu::CONTROLLER_ROWS.len(),
        ))
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        // Always unfocused, like every other list modal: the focused row is composited from
        // its own tile and `ModalShellKey` carries no focus to invalidate on.
        c.list_modal_screen(
            card,
            TITLE,
            SUBTITLE,
            &rows(
                self.settings,
                self.detected_gamepad_type,
                self.dualsense_limited,
                self.webos_major,
            ),
            hover_close,
        )
    }
}
