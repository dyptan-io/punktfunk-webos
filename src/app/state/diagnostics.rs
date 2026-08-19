//! Diagnostics screen logic. Rendering lives in `app::view::diagnostics`.
use crate::app::menu;
use crate::app::App;
use crate::app::DropdownState;
use crate::core::event::MenuEvent;
use crate::core::screen::{Screen, SettingsScope};
use crate::services::store;
use crate::ui;
use std::time::Instant;

impl App {
    /// Opens the Diagnostics screen — reached from the "Diagnostics" row at the
    /// bottom of Settings (`menu::ROW_DIAGNOSTICS`), not a hidden/remote-button menu.
    pub(crate) fn open_diagnostics(&mut self) {
        self.diagnostics_focused = 0;
        self.screen = Screen::Diagnostics;
    }

    /// `menu::DIAG_ROW_*` rows: Log level opens the same dropdown picker every
    /// `Settings` dropdown uses (its row `0` is disambiguated from `Settings`' row 0
    /// by `self.screen`, see `dropdown_overlay_tile`'s docs); the rest are plain
    /// Left/Right/Confirm toggles. Back saves and returns to Settings.
    pub(crate) fn handle_diagnostics_event(&mut self, ev: MenuEvent) {
        if let Some(dd) = self.dropdown.as_mut() {
            let len = menu::LOG_LEVEL_OPTIONS.len();
            match ev {
                MenuEvent::Up | MenuEvent::Down => {
                    crate::ui::widgets::list_nav(&mut dd.focused, len, menu::nav_dir(ev));
                }
                MenuEvent::Confirm => {
                    let choice = dd.focused;
                    self.dropdown_fade.close((menu::DIAG_ROW_LOG_LEVEL, choice));
                    self.dropdown = None;
                    self.set_log_level(menu::LOG_LEVEL_OPTIONS[choice]);
                }
                MenuEvent::Back => {
                    self.dropdown_fade.close((menu::DIAG_ROW_LOG_LEVEL, dd.focused));
                    self.dropdown = None;
                }
                MenuEvent::Left | MenuEvent::Right | MenuEvent::Secondary => {}
            }
            return;
        }
        let len = crate::app::view::diagnostics::rows(&self.settings).len();
        if ui::widgets::list_nav(&mut self.diagnostics_focused, len, menu::nav_dir(ev)) {
            self.modal.focus_anim = Some(Instant::now());
            return;
        }
        match (self.diagnostics_focused, ev) {
            (menu::DIAG_ROW_LOG_LEVEL, MenuEvent::Left | MenuEvent::Right) => self.cycle_log_level(),
            (menu::DIAG_ROW_LOG_LEVEL, MenuEvent::Confirm) => {
                self.dropdown = Some(DropdownState {
                    row: menu::DIAG_ROW_LOG_LEVEL,
                    focused: menu::log_level_dropdown_current_index(self.settings.log_level_override),
                });
                self.dropdown_fade.reopen();
            }
            (menu::DIAG_ROW_STATS_OVERLAY, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.stats_overlay;
                self.settings.stats_overlay = !from;
                self.modal.switch_anim = Some((Instant::now(), from, self.diagnostics_focused));
            }
            (menu::DIAG_ROW_SHOW_LOGS, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.show_logs;
                self.settings.show_logs = !from;
                crate::runtime::set_log_overlay_enabled(!from);
                self.modal.switch_anim = Some((Instant::now(), from, self.diagnostics_focused));
            }
            (menu::DIAG_ROW_SEND_LOGS, MenuEvent::Confirm) => {
                // Persist any pending diagnostics changes before leaving the screen —
                // the confirmation modal's buttons return to Home, not back here.
                self.persist();
                self.open_send_logs();
            }
            (_, MenuEvent::Back) => {
                self.persist();
                self.screen = Screen::Settings(SettingsScope::Global);
            }
            _ => {}
        }
    }

    fn set_log_level(&mut self, level: store::LogLevelOverride) {
        self.settings.log_level_override = level;
        crate::logger::set_level_override(level);
    }

    fn cycle_log_level(&mut self) {
        let idx = menu::log_level_dropdown_current_index(self.settings.log_level_override);
        let next = menu::cycle_index(idx, menu::LOG_LEVEL_OPTIONS.len(), true);
        self.set_log_level(menu::LOG_LEVEL_OPTIONS[next]);
    }
}
