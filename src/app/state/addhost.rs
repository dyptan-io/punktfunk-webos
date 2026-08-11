//! Add-host modal logic. Rendering lives in `app::view::addhost`.
use crate::app::App;
use crate::core::screen::{HomeFocus, Screen};
use crate::services::store::{self, KnownHost};
use crate::ui::MenuEvent;

impl App {
    /// Handles menu event on add-host modal. Left/Right stand in for backspace and
    /// "next field" (no dot or colon key on the remote) — Right past the fourth octet
    /// opens the optional port. Confirm once the address is complete.
    pub fn handle_add_host_event(&mut self, ev: MenuEvent) {
        match ev {
            MenuEvent::Left => self.add_host.backspace(),
            MenuEvent::Right => self.add_host.advance_field(),
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Secondary => {}
            MenuEvent::Confirm => self.confirm_add_host(),
            MenuEvent::Back => self.screen = Screen::Home,
        }
    }

    /// Direct digit entry from Magic Remote number buttons.
    pub fn enter_add_host_digit(&mut self, digit: u8) {
        self.add_host.enter_digit(digit);
    }

    /// No-op until all four octets typed; prevents truncated connections.
    pub(crate) fn confirm_add_host(&mut self) {
        if !self.add_host.is_complete() {
            return;
        }
        let (host, port) = self.add_host.host_and_port();
        // Non-default port in the name, so two ports on one address stay tellable apart.
        let name = if port == crate::ui::FIXED_HOST_PORT {
            host.clone()
        } else {
            format!("{host}:{port}")
        };
        store::upsert_known_host(
            &mut self.known_hosts,
            KnownHost {
                name,
                host: host.clone(),
                port,
                // Only reaches a genuinely new host: `upsert_known_host` keeps an existing
                // record's pins, wol_auto and fingerprint, so re-adding a paired host neither
                // unpairs it nor resets its preferences.
                pinned: vec![store::DESKTOP_PIN_ID.to_string()],
                ..KnownHost::default()
            },
        );
        self.persist();
        self.rebuild_entries();
        self.home_focus = HomeFocus::Sidebar(
            self.entries
                .iter()
                .position(|e| e.host() == host && e.port() == port)
                .unwrap_or(0),
        );
        self.screen = Screen::Home;
    }
    /// Shared by `AddHost` and `EditHost`.
    pub(crate) fn enter_host_address_char(&mut self, c: char) {
        self.add_host.enter_char(c);
    }
}
