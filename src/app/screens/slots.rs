//! Per-screen payloads: the state a single screen owns while it is up.
//!
//! Grouped so it is visible at a glance which state belongs to a screen rather than to the app —
//! and so a screen's own `open_*` is the only thing that has to remember what to reset. Fields
//! that are `Option` are `None` off-screen; the flat ones (pairing's PIN, the host-menu cursor
//! into the sidebar) are reset by the `open_*` that raises their screen.

use crate::app::state::speedtest::SpeedTestState;
use crate::app::state::addhost::AddHostState;
use crate::app::WakeState;
use crate::core::screen::PairingFocus;

#[derive(Default)]
pub(crate) struct ScreenSlots {
    /// The sidebar row the host menu (and everything reached from it) is acting on, `None`
    /// otherwise.
    pub(crate) host_menu_index: Option<usize>,
    /// Whether focus is on the ⋯ button of the host menu's focused row rather than on the row
    /// body — the list-modal counterpart of `HomeFocus::SidebarMenu`. Only the "Wake host" row
    /// has one (see `host_menu_actions`).
    pub(crate) host_menu_dots: bool,
    /// The sidebar row `Screen::EditHost` is editing, `None` otherwise.
    pub(crate) edit_host_index: Option<usize>,
    /// The in-flight/finished speed test, `None` when that screen isn't open.
    pub(crate) speed_test: Option<SpeedTestState>,
    /// The host being measured, for the status line.
    pub(crate) speed_test_name: String,
    pub(crate) add_host: AddHostState,
    /// The active "host unreachable — wake it?" prompt/wait, if any — see `WakeState`.
    pub(crate) wake: Option<WakeState>,
    /// PIN entry: 4 digits, each 0-9, edited one at a time.
    pub(crate) pin_digits: [u8; 4],
    pub(crate) pin_digit_index: usize,
    /// Whether the pairing modal's input is on the PIN row or the Request-access button.
    pub(crate) pairing_focus: PairingFocus,
    pub(crate) pairing_status: Option<String>,
    pub(crate) pairing_busy: bool,
    /// Index into `hosts.entries` currently being paired — captured when entering
    /// `Screen::Pairing`.
    pub(crate) pairing_entry: usize,
}
