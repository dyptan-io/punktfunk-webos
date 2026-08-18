//! Activating the focused widget: the press dip, and the one table that routes a
//! `MenuEvent` to whichever screen is up.
//!
//! Every button presses the same way — its focus tile dips while its action runs — so
//! nothing here asks which modal is open, only whether what is focused is a button
//! (`pressable`).
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
        // Anything but a confirm cancels an in-flight dip: focus (or the screen) has moved,
        // so the tile it rides is no longer the pressed widget.
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

    /// Confirms the focused widget, dipping it if it is pressable.
    ///
    /// The action runs on the same frame the dip is armed, not after it: waiting put the
    /// dip's whole 120ms in front of every screen transition. A closing modal's dip is then
    /// never seen (its focus tile is already baked into the closing snapshot), which costs
    /// nothing — there is nothing left on screen to push in.
    pub(crate) fn press(&mut self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<ConnectTarget> {
        if self.pressable() {
            self.press.arm();
            self.press_screen = self.screen;
        }
        self.handle_menu_event(MenuEvent::Confirm, screen_w, screen_h, fonts)
    }

    /// The dip, but only for the screen whose button armed it. Every focus tile composites
    /// through `App::press`, so a modal's confirm would otherwise dip the sidebar row behind
    /// its card too.
    pub(crate) fn press_dip(&self, owner: Screen) -> ui::animation::Press {
        if self.press_screen == owner {
            self.press
        } else {
            ui::animation::Press::default()
        }
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

    /// Retires a dip that has played all the way out; called every frame. `true` on the
    /// frame that must be redrawn without it.
    pub(crate) fn poll_press(&mut self) -> bool {
        self.press.landed() && self.press.take()
    }
}
