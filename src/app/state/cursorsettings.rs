//! Cursor screen logic. Rendering lives in `app::view::cursorsettings`.
use crate::app::App;
use crate::core::screen::Screen;
use crate::ui::{self, MenuEvent};
use std::time::Instant;

impl App {
    /// Opens the Cursor screen (Settings → `ui::ROW_CURSOR`). Holds the two pointer
    /// toggles: capture mode and the OK-button gestures.
    pub(crate) fn open_cursor_settings(&mut self) {
        self.cursor_settings_focused = 0;
        // Stash scroll so Back can restore it; this screen doesn't use it.
        self.settings_scroll = self.scroll;
        self.screen = Screen::CursorSettings;
    }

    /// All rows are plain Left/Right/Confirm toggles. Back saves and returns to Settings.
    pub(crate) fn handle_cursor_settings_event(&mut self, ev: MenuEvent) {
        if ui::list_nav(&mut self.cursor_settings_focused, ui::CURSOR_ROW_COUNT, ev) {
            self.modal_focus_anim = Some(Instant::now());
            return;
        }
        match (self.cursor_settings_focused, ev) {
            (ui::CURSOR_ROW_CAPTURE, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.cursor_capture;
                self.settings.cursor_capture = !from;
                self.switch_anim = Some((Instant::now(), from, self.cursor_settings_focused));
            }
            (ui::CURSOR_ROW_GESTURES, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.cursor_gestures;
                self.settings.cursor_gestures = !from;
                self.switch_anim = Some((Instant::now(), from, self.cursor_settings_focused));
            }
            (_, MenuEvent::Back) => {
                self.persist();
                self.screen = Screen::Settings;
                self.scroll = self.settings_scroll;
            }
            _ => {}
        }
    }
}
