//! Where the UI is, and where the cursor was left on each screen it can be.
//!
//! The cursors used to be nine `usize` fields on `App` — one per screen that has a focusable
//! list — plus four `match self.screen` tables mapping the current screen onto the right one.
//! Every table had to name the same field as the other three; one of them already didn't (see
//! `WakeSettings` in `docs/APP-REWORK-PLAN.md` §1, P3). Here the mapping is the array index,
//! so there is nothing left to keep in step.
use crate::core::screen::Screen;

/// A [`Screen`] without its payload — what a cursor is filed under, so the two settings
/// scopes (and the two cursor-settings scopes) share one cursor exactly as they did when each
/// was a named field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScreenKey {
    Home,
    Pairing,
    Settings,
    AddHost,
    Wake,
    ForgetHost,
    HostMenu,
    EditHost,
    About,
    SpeedTest,
    WakeSettings,
    Diagnostics,
    Experimental,
    CursorSettings,
    SendLogs,
    Collections,
    RenameCollection,
}

impl ScreenKey {
    pub const COUNT: usize = Self::RenameCollection as usize + 1;

    pub const fn of(screen: Screen) -> Self {
        match screen {
            Screen::Home => Self::Home,
            Screen::Pairing => Self::Pairing,
            Screen::Settings(_) => Self::Settings,
            Screen::AddHost => Self::AddHost,
            Screen::Wake => Self::Wake,
            Screen::ForgetHost => Self::ForgetHost,
            Screen::HostMenu => Self::HostMenu,
            Screen::EditHost => Self::EditHost,
            Screen::About => Self::About,
            Screen::SpeedTest => Self::SpeedTest,
            Screen::WakeSettings => Self::WakeSettings,
            Screen::Diagnostics => Self::Diagnostics,
            Screen::Experimental => Self::Experimental,
            Screen::CursorSettings(_) => Self::CursorSettings,
            Screen::SendLogs => Self::SendLogs,
            Screen::Collections => Self::Collections,
            Screen::RenameCollection => Self::RenameCollection,
        }
    }
}

/// The current screen, the one before it, and one focus cursor per screen.
///
/// A cursor survives leaving its screen on purpose: a nested menu (host menu → wake settings →
/// Back) has to come back to the row it was opened from. What resets a cursor is
/// [`enter`](Self::enter), which every `open_*` goes through.
pub(crate) struct Nav {
    pub screen: Screen,
    /// Last screen `prepare_tiles` saw — a change triggers the modal-open animation and a
    /// modal re-rasterize without every transition site needing to remember to.
    pub last_screen: Screen,
    cursors: [usize; ScreenKey::COUNT],
}

impl Default for Nav {
    fn default() -> Self {
        Self {
            screen: Screen::Home,
            last_screen: Screen::Home,
            cursors: [0; ScreenKey::COUNT],
        }
    }
}

impl Nav {
    /// Where focus sits on `key`'s screen.
    pub fn cursor(&self, key: ScreenKey) -> usize {
        self.cursors[key as usize]
    }

    pub fn cursor_mut(&mut self, key: ScreenKey) -> &mut usize {
        &mut self.cursors[key as usize]
    }

    pub fn set_cursor(&mut self, key: ScreenKey, at: usize) {
        self.cursors[key as usize] = at;
    }

    /// Goes to `screen` with its cursor at `at` — what an `open_*` wants, since a screen
    /// opened afresh starts where its caller says rather than where it was last left.
    pub fn enter(&mut self, screen: Screen, at: usize) {
        self.set_cursor(ScreenKey::of(screen), at);
        self.screen = screen;
    }

    /// Goes to `screen` leaving its cursor where it was — the return leg of a nested menu.
    pub fn resume(&mut self, screen: Screen) {
        self.screen = screen;
    }
}
