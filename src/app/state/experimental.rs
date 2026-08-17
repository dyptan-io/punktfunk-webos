//! Experimental screen logic. Rendering lives in `app::view::experimental`.
use crate::app::menu;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::ui;
use std::time::Instant;

impl App {
    /// Opens the Experimental screen (Settings → `menu::ROW_EXPERIMENTAL`). Holds unstable,
    /// off-by-default toggles (the frame pacer, the software-audio override).
    pub(crate) fn open_experimental(&mut self) {
        self.experimental_focused = 0;
        self.screen = Screen::Experimental;
    }

    /// All rows are plain Left/Right/Confirm toggles. Back saves and returns to Settings.
    pub(crate) fn handle_experimental_event(&mut self, ev: MenuEvent) {
        let len = crate::app::view::experimental::rows(&self.settings, Self::rooted()).len();
        if ui::widgets::list_nav(&mut self.experimental_focused, len, menu::nav_dir(ev)) {
            self.modal_focus_anim = Some(Instant::now());
            return;
        }
        match (self.experimental_focused, ev) {
            (menu::EXP_ROW_FRAME_PACER, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.video_pacing;
                self.settings.video_pacing = !from;
                self.switch_anim = Some((Instant::now(), from, self.experimental_focused));
            }
            (menu::EXP_ROW_SOFTWARE_AUDIO, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.force_software_audio;
                self.settings.force_software_audio = !from;
                self.switch_anim = Some((Instant::now(), from, self.experimental_focused));
            }
            (menu::EXP_ROW_GAME_MODE, MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.game_mode;
                self.settings.game_mode = !from;
                self.switch_anim = Some((Instant::now(), from, self.experimental_focused));
            }
            (_, MenuEvent::Back) => {
                self.persist();
                self.screen = Screen::Settings;
            }
            _ => {}
        }
    }
}
