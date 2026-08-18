//! Add-host modal logic. Rendering lives in `app::view::addhost`.
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::{HomeFocus, Screen};
use crate::services::store::{self, KnownHost};

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
        let name = if port == FIXED_HOST_PORT {
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
                games: store::pinned_only(store::DESKTOP_PIN_ID),
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

/// punktfunk's conventional host port — what a bare address means, so the add-host
/// screen only *has* to ask for an IP; an explicit `:port` suffix overrides it.
pub const FIXED_HOST_PORT: u16 = 9777;

/// Manual "add host" entry state: one free-form field holding `ip[:port]`
/// exactly as typed — no fixed-width mask, no `_` placeholders, no separate
/// port box. Separators are inserted automatically (a `.` once an octet is
/// complete — three digits, or a fourth that would push it past 255 — and a `:`
/// once a fourth octet is), so the Magic Remote's number pad (`digit_key_value`)
/// is enough on its own, with Left/Right (see `app::state::addhost`) standing in
/// for the backspace and separator keys it lacks.
#[derive(Default)]
pub struct AddHostState {
    text: String,
}

impl AddHostState {
    /// Pre-fills from an existing address for `Screen::EditHost`, keeping a
    /// non-default port visible so re-saving can't silently move the host back
    /// to [`FIXED_HOST_PORT`]. An address that isn't four numeric octets comes
    /// back empty rather than partially parsed — better to retype than to
    /// silently edit a mangled address.
    pub fn from_host_port(ip: &str, port: u16) -> Self {
        let mut s = Self { text: ip.to_string() };
        if !s.is_complete() {
            return Self::default();
        }
        if port != FIXED_HOST_PORT {
            s.text.push(':');
            s.text.push_str(&port.to_string());
        }
        s
    }

    /// Types one character from the webOS on-screen keyboard (`Event::TextInput`,
    /// see `main.rs`) — digits behave exactly as the remote's number pad does, and
    /// `.` / `:` finish the current field, since a real keyboard *does* have the
    /// separator keys the Magic Remote lacks. Anything else is ignored: this field
    /// only ever holds an IPv4 address and an optional port.
    pub fn enter_char(&mut self, c: char) {
        if let Some(d) = c.to_digit(10) {
            self.enter_digit(d as u8);
        } else if c == '.' || c == ':' {
            self.advance_field();
        }
    }

    /// Whether the address part names a real, connectable host — the point at
    /// which `host_and_port()` is meaningful. The port part is optional and, when
    /// absent or unparseable, falls back to [`FIXED_HOST_PORT`].
    pub fn is_complete(&self) -> bool {
        let addr = self.text.split(':').next().unwrap_or_default();
        let mut octets = addr.split('.');
        let ok = (&mut octets).take(4).filter(|o| o.parse::<u8>().is_ok()).count() == 4;
        ok && octets.next().is_none()
    }

    pub fn host_and_port(&self) -> (String, u16) {
        let (addr, port) = self.text.split_once(':').unwrap_or((&self.text, ""));
        let port = port.parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(FIXED_HOST_PORT);
        (addr.to_string(), port)
    }

    /// What's actually been typed so far, exactly as typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Types one digit (0-9), inserting the separator the remote can't type
    /// whenever the current field is full: an octet finishes at three digits or
    /// when a fourth would push its value past 255, and after the fourth octet
    /// further digits can only mean a port, so a `:` goes in by itself.
    pub fn enter_digit(&mut self, digit: u8) {
        let c = (b'0' + digit) as char;
        if let Some((_, port)) = self.text.split_once(':') {
            let mut candidate = port.to_string();
            candidate.push(c);
            if candidate.parse::<u16>().is_ok() {
                self.text.push(c);
            }
            return;
        }
        let seg = self.text.rsplit('.').next().unwrap_or_default();
        let full = seg.len() == 3 || format!("{seg}{c}").parse::<u16>().unwrap_or(0) > 255;
        if full {
            self.advance_field();
        }
        self.text.push(c);
    }

    /// Deletes the last typed character, separators included — Left on the d-pad.
    pub fn backspace(&mut self) {
        self.text.pop();
    }

    /// Manually finishes the field in progress — so e.g. "8" can become "8.8.8.8"
    /// without waiting for three digits or an overflow, and a complete address can
    /// grow a port. Right on the d-pad, standing in for the "." and ":" keys a real
    /// keyboard would have.
    pub fn advance_field(&mut self) {
        if self.text.is_empty() || self.text.ends_with(['.', ':']) || self.text.contains(':') {
            return;
        }
        self.text.push(if self.is_complete() { ':' } else { '.' });
    }
}
