//! Cursor screen logic. Rendering lives in `app::view::cursorsettings`.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;

impl App {
    /// Opens the Cursor screen (Settings → `menu::SettingsRow::Cursor`). Holds the two pointer
    /// toggles: capture mode and the OK-button gestures. `scope` is the caller's, carried on
    /// the screen so the sub-screen keeps editing the same document.
    pub(crate) fn open_cursor_settings(&mut self, scope: menu::SettingsScope) {
        self.nav.enter(Screen::CursorSettings(scope), 0);
    }

    /// All rows are plain Left/Right/Confirm toggles. Back saves and returns to whichever
    /// settings screen opened it — the per-game one keeps editing its own copy while here
    /// (see `App::settings_target`), so only where the save lands differs.
    pub(crate) fn handle_cursor_settings_event(&mut self, ev: MenuEvent) {
        if self.list_nav_event(ev) {
            return;
        }
        let row = self.nav.cursor(ScreenKey::CursorSettings);
        match (menu::CURSOR_ROWS.get(row).copied(), ev) {
            // Both rows are plain toggles, so they go through the same mutator every other
            // settings row uses — they are `menu::SettingsRow`s like any other.
            (Some(logical), MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = menu::toggle_value(self.settings_target(), logical);
                let detected = self.detected_gamepad_type;
                if menu::adjust_setting(self.settings_target_mut(), logical, true, detected) {
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
                // The per-game copy is saved once, on the way out of its own screen — this
                // is a step back into it, not out of the flow.
                if scope == menu::SettingsScope::Global {
                    self.persist();
                }
                // Back into the list this was opened from: it keeps its place.
                self.nav.resume(Screen::Settings(scope));
            }
            _ => {}
        }
    }
}
