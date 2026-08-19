//! Cursor screen logic. Rendering lives in `app::view::cursorsettings`.
use crate::app::menu;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::ui;
use std::time::Instant;

impl App {
    /// Opens the Cursor screen (Settings → `menu::ROW_CURSOR`). Holds the two pointer
    /// toggles: capture mode and the OK-button gestures. `scope` is the caller's, carried on
    /// the screen so the sub-screen keeps editing the same document.
    pub(crate) fn open_cursor_settings(&mut self, scope: menu::SettingsScope) {
        self.cursor_settings_focused = 0;
        self.screen = Screen::CursorSettings(scope);
    }

    /// All rows are plain Left/Right/Confirm toggles. Back saves and returns to whichever
    /// settings screen opened it — the per-game one keeps editing its own copy while here
    /// (see `App::settings_target`), so only where the save lands differs.
    pub(crate) fn handle_cursor_settings_event(&mut self, ev: MenuEvent) {
        if ui::widgets::list_nav(
            &mut self.cursor_settings_focused,
            menu::CURSOR_ROW_COUNT,
            menu::nav_dir(ev),
        ) {
            self.modal.focus_anim = Some(Instant::now());
            return;
        }
        let row = self.cursor_settings_focused;
        match (row, ev) {
            // Both rows are plain toggles, so they go through the same mutator every
            // settings row uses — `cursor_logical_row` is where the dense `CURSOR_ROW_*`
            // indices meet the logical `ROW_*` ids the override table is keyed by.
            (_, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let logical = menu::cursor_logical_row(row);
                let from = menu::toggle_value(self.settings_target(), logical);
                let detected = self.detected_gamepad_type;
                if menu::adjust_setting(self.settings_target_mut(), logical, true, detected) {
                    self.capture_game_override(logical);
                    if let Some(from) = from {
                        self.modal.switch_anim = Some((Instant::now(), from, row));
                    }
                }
            }
            // Same clear gesture as the parent list: these rows are on it in every way but
            // which screen draws them.
            (_, MenuEvent::Secondary) => self.clear_focused_override(),
            (_, MenuEvent::Back) => {
                let scope = self.settings_scope();
                // The per-game copy is saved once, on the way out of its own screen — this
                // is a step back into it, not out of the flow.
                if scope == menu::SettingsScope::Global {
                    self.persist();
                }
                self.screen = Screen::Settings(scope);
            }
            _ => {}
        }
    }
}
