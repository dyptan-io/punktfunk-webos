//! The one editable text field, in the two shapes this app types into: a host address and a
//! collection name.
//!
//! Both are edited by the same three inputs — the Magic Remote's number pad, the webOS
//! on-screen keyboard, and Left/Right standing in for keys the remote lacks — so what differs
//! between them is only which characters are accepted and when the field is finished. That is
//! [`FieldKind`], and it is a value rather than a second state struct.

/// What a [`TextField`] holds, and therefore what it accepts.
pub(crate) enum FieldKind {
    /// `ip[:port]` exactly as typed — no fixed-width mask, no `_` placeholders, no separate
    /// port box. Separators are inserted automatically (a `.` once an octet is complete —
    /// three digits, or a fourth that would push it past 255 — and a `:` once a fourth octet
    /// is), so the remote's number pad (`digit_key_value`) is enough on its own, with
    /// Left/Right (see `app::state::addhost`) standing in for the backspace and separator
    /// keys it lacks.
    Ipv4Port,
    /// A free-form name, bounded at `max` characters. Uniqueness is not a property of the
    /// field — the document it will be written into decides that (`KnownHost::can_name`).
    Name { max: usize },
}

pub(crate) struct TextField {
    text: String,
    kind: FieldKind,
}

/// The add-host field is the one `ScreenSlots::default` builds, so the address shape is the
/// default one.
impl Default for TextField {
    fn default() -> Self {
        Self::ipv4()
    }
}

impl TextField {
    pub fn ipv4() -> Self {
        Self {
            text: String::new(),
            kind: FieldKind::Ipv4Port,
        }
    }

    /// A name field, pre-filled for a rename and empty when adding.
    pub fn name(max: usize, text: &str) -> Self {
        Self {
            text: text.to_string(),
            kind: FieldKind::Name { max },
        }
    }

    /// Pre-fills from an existing address for `Screen::EditHost`, keeping a
    /// non-default port visible so re-saving can't silently move the host back
    /// to [`FIXED_HOST_PORT`](super::addhost::FIXED_HOST_PORT). An address that isn't four
    /// numeric octets comes back empty rather than partially parsed — better to retype than
    /// to silently edit a mangled address.
    pub fn from_host_port(ip: &str, port: u16) -> Self {
        let mut s = Self {
            text: ip.to_string(),
            kind: FieldKind::Ipv4Port,
        };
        if !s.is_complete() {
            return Self::ipv4();
        }
        if port != super::addhost::FIXED_HOST_PORT {
            s.text.push(':');
            s.text.push_str(&port.to_string());
        }
        s
    }

    /// What's actually been typed so far, exactly as typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Types one character from the webOS on-screen keyboard (`Event::TextInput`, see
    /// `main.rs`). On an address, digits behave exactly as the remote's number pad does and
    /// `.` / `:` finish the current field, since a real keyboard *does* have the separator
    /// keys the Magic Remote lacks; anything else is ignored. A name takes any printable
    /// character up to its limit.
    pub fn enter_char(&mut self, c: char) {
        match self.kind {
            FieldKind::Ipv4Port => {
                if let Some(d) = c.to_digit(10) {
                    self.enter_digit(d as u8);
                } else if c == '.' || c == ':' {
                    self.advance_field();
                }
            }
            // Control characters included, so a stray newline from the OSK can't end up in a
            // name that is then drawn as a row label.
            FieldKind::Name { max } => {
                if !c.is_control() && self.text.chars().count() < max {
                    self.text.push(c);
                }
            }
        }
    }

    /// Whether the field holds something its screen can commit — an address that names a real,
    /// connectable host, or a name that isn't blank. What greys the confirm button, with the
    /// caller adding whatever only it can know (a name's uniqueness).
    pub fn is_complete(&self) -> bool {
        match self.kind {
            FieldKind::Ipv4Port => {
                let addr = self.text.split(':').next().unwrap_or_default();
                let mut octets = addr.split('.');
                let ok = (&mut octets).take(4).filter(|o| o.parse::<u8>().is_ok()).count() == 4;
                ok && octets.next().is_none()
            }
            FieldKind::Name { .. } => !self.text.trim().is_empty(),
        }
    }

    /// The address and port typed, the port falling back to
    /// [`FIXED_HOST_PORT`](super::addhost::FIXED_HOST_PORT) when absent or unparseable.
    pub fn host_and_port(&self) -> (String, u16) {
        let (addr, port) = self.text.split_once(':').unwrap_or((&self.text, ""));
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .unwrap_or(super::addhost::FIXED_HOST_PORT);
        (addr.to_string(), port)
    }

    /// Types one digit (0-9). On an address it inserts the separator the remote can't type
    /// whenever the current field is full: an octet finishes at three digits or when a fourth
    /// would push its value past 255, and after the fourth octet further digits can only mean
    /// a port, so a `:` goes in by itself.
    pub fn enter_digit(&mut self, digit: u8) {
        let c = (b'0' + digit) as char;
        if let FieldKind::Name { .. } = self.kind {
            self.enter_char(c);
            return;
        }
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

    /// Manually finishes the address field in progress — so e.g. "8" can become "8.8.8.8"
    /// without waiting for three digits or an overflow, and a complete address can grow a
    /// port. Right on the d-pad, standing in for the "." and ":" keys a real keyboard would
    /// have. A name has no fields, so nothing to advance.
    pub fn advance_field(&mut self) {
        if !matches!(self.kind, FieldKind::Ipv4Port)
            || self.text.is_empty()
            || self.text.ends_with(['.', ':'])
            || self.text.contains(':')
        {
            return;
        }
        self.text.push(if self.is_complete() { ':' } else { '.' });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_takes_printable_characters_up_to_its_limit() {
        let mut f = TextField::name(4, "");
        for c in "ab\ncd!".chars() {
            f.enter_char(c);
        }
        assert_eq!(f.text(), "abcd", "control characters dropped, then the limit holds");
        f.backspace();
        assert_eq!(f.text(), "abc");
        // No fields to advance, and blank is not committable.
        f.advance_field();
        assert_eq!(f.text(), "abc");
        assert!(f.is_complete());
        assert!(!TextField::name(4, "  ").is_complete());
    }

    #[test]
    fn an_address_still_separates_its_own_octets() {
        let mut f = TextField::ipv4();
        for d in [1, 9, 2, 1, 6, 8, 1, 5] {
            f.enter_digit(d);
        }
        assert_eq!(f.text(), "192.168.15");
        f.advance_field();
        f.enter_digit(5);
        assert_eq!(f.text(), "192.168.15.5");
        assert!(f.is_complete());
        assert_eq!(
            f.host_and_port(),
            ("192.168.15.5".to_string(), super::super::addhost::FIXED_HOST_PORT)
        );
    }
}
