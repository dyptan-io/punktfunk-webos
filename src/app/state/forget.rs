//! The "Forget this host?" confirmation modal's logic. Rendering lives in `app::view::forget`.
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use std::time::Instant;

impl App {
    /// Open `ForgetHost` confirmation for sidebar row at long-press.
    pub fn open_forget_host(&mut self, idx: usize) {
        self.screens.host_menu_index = Some(idx);
        self.nav.enter(Screen::ForgetHost, 1);
    }

    /// Return to `HostMenu` or Home if host was removed.
    pub(crate) fn back_to_host_menu(&mut self) {
        if self
            .screens
            .host_menu_index
            .is_some_and(|i| i < self.hosts.entries.len())
        {
            self.nav.enter(Screen::HostMenu, 0);
        } else {
            self.screens.host_menu_index = None;
            self.nav.screen = Screen::Home;
        }
    }

    /// Handle menu event. Left/Right toggle focus; Confirm/Back act on focused button.
    pub fn handle_forget_host_event(&mut self, ev: MenuEvent) {
        match ev {
            MenuEvent::Left | MenuEvent::Right => {
                self.nav
                    .set_cursor(ScreenKey::ForgetHost, 1 - self.nav.cursor(ScreenKey::ForgetHost));
                self.render.modal.focus_anim = Some(Instant::now());
            }
            MenuEvent::Confirm => {
                if self.nav.cursor(ScreenKey::ForgetHost) == 0 {
                    if let Some(idx) = self.screens.host_menu_index {
                        self.forget_host(idx);
                    }
                    // Entry list changed; host_menu_index is now stale
                    self.screens.host_menu_index = None;
                    self.nav.screen = Screen::Home;
                } else {
                    self.back_to_host_menu();
                }
            }
            // Back returns to menu (not Home) to avoid closing the menu behind
            MenuEvent::Back => self.back_to_host_menu(),
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Secondary => {}
        }
    }
}
