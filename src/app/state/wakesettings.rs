//! Per-host Wake-on-LAN settings — logic. Rendering lives in `app::view::wakesettings`.
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::services::store;

impl App {
    /// Open Wake settings for host menu's current host.
    pub(crate) fn open_wake_settings(&mut self) {
        self.nav.enter(Screen::WakeSettings, 0);
    }

    /// Host being edited (always from host menu).
    pub(crate) fn wake_settings_host(&self) -> Option<&store::KnownHost> {
        let entry = self.host_menu_index.and_then(|i| self.entries.get(i))?;
        let (host, port) = (entry.host(), entry.port());
        self.known_hosts.iter().find(|h| h.host == host && h.port == port)
    }

    /// Left/Right/Confirm flip toggle; Back returns to host menu.
    pub(crate) fn handle_wake_settings_event(&mut self, ev: MenuEvent) {
        if self.list_nav_event(ev) {
            return;
        }
        match ev {
            MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm => self.toggle_wol_auto(),
            MenuEvent::Back => self.nav.screen = Screen::HostMenu,
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Secondary => {}
        }
    }

    /// Flip auto-send flag and persist (discovered-only hosts have no record).
    fn toggle_wol_auto(&mut self) {
        let Some(entry) = self.host_menu_index.and_then(|i| self.entries.get(i)) else {
            return;
        };
        let (host, port) = (entry.host().to_string(), entry.port());
        let Some(known) = self.known_hosts.iter_mut().find(|h| h.host == host && h.port == port) else {
            return;
        };
        let from = known.wol_auto;
        known.wol_auto = !from;
        self.persist();
        self.arm_switch_anim(from);
    }
}
