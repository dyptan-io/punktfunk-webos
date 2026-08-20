//! Diagnostics screen logic. Rendering lives in `app::view::diagnostics`.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::app::DropdownState;
use crate::core::event::MenuEvent;
use crate::core::screen::{Screen, SettingsScope};
use crate::services::store;

impl App {
    /// Opens the Diagnostics screen — reached from the "Diagnostics" row at the
    /// bottom of Settings (`menu::ROW_DIAGNOSTICS`), not a hidden/remote-button menu.
    pub(crate) fn open_diagnostics(&mut self) {
        self.nav.enter(Screen::Diagnostics, 0);
    }

    /// `menu::DIAG_ROW_*` rows: Log level opens the same dropdown picker every
    /// `Settings` dropdown uses (its row `0` is disambiguated from `Settings`' row 0
    /// by `self.nav.screen`, see `dropdown_overlay_tile`'s docs); the rest are plain
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
        if self.list_nav_event(ev) {
            return;
        }
        match (self.nav.cursor(ScreenKey::Diagnostics), ev) {
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
                self.arm_switch_anim(from);
            }
            (menu::DIAG_ROW_SHOW_LOGS, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.show_logs;
                self.settings.show_logs = !from;
                crate::runtime::set_log_overlay_enabled(!from);
                self.arm_switch_anim(from);
            }
            (menu::DIAG_ROW_SEND_LOGS, MenuEvent::Confirm) => {
                // Persist any pending diagnostics changes before leaving the screen —
                // the confirmation modal's buttons return to Home, not back here.
                self.persist();
                self.open_send_logs();
            }
            (_, MenuEvent::Back) => {
                self.persist();
                self.nav.resume(Screen::Settings(SettingsScope::Global));
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
