//! Activating the focused widget: the press dip, and the one table that routes a
//! `MenuEvent` to whichever screen is up.
//!
//! Every button presses the same way — its focus tile dips, the action runs when the dip
//! lands — so nothing here asks which modal is open, only whether what is focused is a
//! button (`pressable`).
use crate::app::*;

impl App {
    /// Routes one `MenuEvent` to the open screen — the single dispatch table, which
    /// `press`, `back` and the runtime's event pump all come through. `Some` only when
    /// the event launched a stream.
    pub(crate) fn handle_menu_event(
        &mut self,
        ev: MenuEvent,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<ConnectTarget> {
        // Anything but a confirm cancels an in-flight dip: it moves focus (or closes the
        // screen), so landing the deferred action would confirm a widget nobody pressed.
        if ev != MenuEvent::Confirm {
            self.press.take();
        }
        match self.screen {
            Screen::Home => return self.handle_home_event(ev, screen_w, screen_h),
            Screen::Pairing => self.handle_pairing_event(ev),
            Screen::Settings => self.handle_settings_event(ev, screen_h),
            Screen::AddHost => self.handle_add_host_event(ev),
            Screen::Wake => self.handle_wake_event(ev),
            Screen::ForgetHost => self.handle_forget_host_event(ev),
            Screen::HostMenu => self.handle_host_menu_event(ev),
            Screen::WakeSettings => self.handle_wake_settings_event(ev),
            Screen::SpeedTest => self.handle_speed_test_event(ev),
            Screen::EditHost => self.handle_edit_host_event(ev),
            Screen::About => self.handle_about_event(ev, screen_w, screen_h, fonts),
            Screen::PinLimit => self.handle_pin_limit_event(ev),
            Screen::Diagnostics => self.handle_diagnostics_event(ev),
            Screen::Experimental => self.handle_experimental_event(ev),
            Screen::CursorSettings => self.handle_cursor_settings_event(ev),
            Screen::SendLogs => self.handle_send_logs_event(ev),
        }
        None
    }

    /// Confirms the focused widget, dipping it first if it is pressable.
    ///
    /// The action waits for the dip to land (`poll_press`) because most of them close the
    /// modal: acting now would swap the focus tile for the closing snapshot before a
    /// single frame of the dip was drawn.
    pub(crate) fn press(&mut self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<ConnectTarget> {
        // A press landing mid-dip runs that one now rather than dropping it.
        if let Some(target) = self.run_press(screen_w, screen_h, fonts) {
            return Some(target);
        }
        if !self.pressable() {
            return self.handle_menu_event(MenuEvent::Confirm, screen_w, screen_h, fonts);
        }
        self.press.arm();
        None
    }

    /// Whether the focused widget is a button — the only thing that dips. A row carries a
    /// value the press changes in place, and pushing a full-width row in for that reads as
    /// the list lurching; a button *is* its action.
    fn pressable(&self) -> bool {
        // Focus is on a dropdown option, which has its own tile — the row behind the
        // overlay is not what was pressed.
        if self.dropdown.is_some() {
            return false;
        }
        match self.screen {
            // Sidebar rows are buttons: pick a host, add one, open Settings. A grid card
            // isn't — launching one is already an animation of its own.
            Screen::Home => matches!(self.home_focus, HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_)),
            // The button only; the PIN digits above it are a field.
            Screen::Pairing => matches!(self.pairing_focus, PairingFocus::RequestAccess),
            Screen::Wake | Screen::ForgetHost | Screen::SpeedTest | Screen::SendLogs => true,
            // Rows, not buttons.
            Screen::Settings
            | Screen::AddHost
            | Screen::EditHost
            | Screen::About
            | Screen::HostMenu
            | Screen::WakeSettings
            | Screen::PinLimit
            | Screen::Diagnostics
            | Screen::Experimental
            | Screen::CursorSettings => false,
        }
    }

    /// Runs a deferred press whose dip has landed; called every frame. The outer `Some`
    /// means one fired (so the frame is dirty), the inner whatever its action produced.
    pub(crate) fn poll_press(
        &mut self,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<Option<ConnectTarget>> {
        self.press.landed().then(|| self.run_press(screen_w, screen_h, fonts))
    }

    /// Runs the deferred press now, however far its dip has got.
    fn run_press(&mut self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<ConnectTarget> {
        self.press
            .take()
            .then(|| self.handle_menu_event(MenuEvent::Confirm, screen_w, screen_h, fonts))?
    }
}
