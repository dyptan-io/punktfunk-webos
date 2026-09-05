//! Controller screen logic. Rendering lives in `app::view::controllersettings`.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::app::DropdownState;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;

impl App {
    /// Opens the Controller screen (Settings → `menu::SettingsRow::Controller`). Holds the pad
    /// kind and the two rows that decide which menus a pad drives. `scope` is the caller's,
    /// carried on the screen so the sub-screen keeps editing the same document.
    pub(crate) fn open_controller_settings(&mut self, scope: menu::SettingsScope) {
        self.nav.enter(Screen::ControllerSettings(scope), 0);
    }

    /// Two dropdown rows and one toggle, all `menu::SettingsRow`s — so every pick goes through
    /// the same mutators the main list uses. Back saves and returns to whichever settings
    /// screen opened it.
    pub(crate) fn handle_controller_settings_event(&mut self, ev: MenuEvent) {
        // An open dropdown is a modal over this list and takes every key until it closes —
        // the main list's rule, and the same close-then-apply order, because the pick has to
        // reach `self` beyond the borrow the fade needs.
        let open = self.settings_ui.dropdown.as_ref().map(|dd| dd.row);
        if let Some(row) = open {
            let logical = menu::CONTROLLER_ROWS.get(row).copied();
            let len = logical.map_or(1, |r| menu::dropdown_option_count(r, self.settings_target()).max(1));
            if self.dropdown_event(ev, row, len, move |app, choice| {
                if let Some(logical) = logical {
                    let detected = app.detected_gamepad_type;
                    menu::apply_dropdown_choice(app.settings_target_mut(), logical, choice, detected);
                    app.capture_game_override(logical);
                }
            }) {
                return;
            }
        }
        if self.list_nav_event(ev) {
            return;
        }
        let row = self.nav.cursor(ScreenKey::ControllerSettings);
        let logical = menu::CONTROLLER_ROWS.get(row).copied();
        match (logical, ev) {
            // A locked row never opens its dropdown — there is nothing to pick, which is what
            // the greyed control already says.
            (Some(logical @ (menu::SettingsRow::Gamepad | menu::SettingsRow::GamepadUiMode)), MenuEvent::Confirm)
                if menu::row_lock(logical, self.settings_target(), self.detected_gamepad_type).is_none() =>
            {
                let focused = menu::dropdown_current_index(self.settings_target(), logical);
                self.settings_ui.dropdown = Some(DropdownState { row, focused });
                self.settings_ui.dropdown_fade.reopen();
            }
            // Left/Right cycle in place, and the switch row toggles — `adjust_setting` is the
            // one mutator either way, so it enforces the lock rather than each site repeating it.
            (Some(logical), MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = menu::toggle_value(self.settings_target(), logical);
                let detected = self.detected_gamepad_type;
                let forward = ev != MenuEvent::Left;
                if menu::adjust_setting(self.settings_target_mut(), logical, forward, detected) {
                    self.capture_game_override(logical);
                    if let Some(from) = from {
                        self.arm_switch_anim(from);
                    }
                }
            }
            // Same clear gesture as the parent list: these rows are on it in every way but
            // which screen draws them.
            (_, MenuEvent::Secondary) => self.clear_focused_override(),
            (_, MenuEvent::Back) => {
                let scope = self.settings_scope();
                // The per-game copy is saved once, on the way out of its own screen — this is
                // a step back into it, not out of the flow.
                if scope == menu::SettingsScope::Global {
                    self.persist();
                }
                self.nav.resume(Screen::Settings(scope));
            }
            _ => {}
        }
    }
}
