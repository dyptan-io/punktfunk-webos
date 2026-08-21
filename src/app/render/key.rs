//! What each cached tile's pixels depend on.
//!
//! `ui::cache::TileStore` decides staleness by comparing a `u64` version; these are the
//! keys that version is hashed from. Each variant carries *everything* the tile draws, so
//! adding a dependency is adding a field rather than remembering to extend a comparison.
//!
//! App-side on purpose: they name this app's screens and its `Settings`, which is exactly
//! what a widget library must not know.
//!
//! Every key here is hashed the moment it is built and then dropped — nothing stores one (see
//! `App::modal_shell_version`). The borrowed `&str` fields say so in the type: a key that could
//! outlive the state it describes would have to own a copy of every label, once per frame.
use crate::app::state::hostmenu::HostAction;
use crate::core::model::{GamepadType, LogLevelOverride, Settings, SettingsOverride};

/// Focused widget in the open modal. Each variant carries its content,
/// so value changes (not just focus moves) invalidate the tile.
#[derive(PartialEq, Eq, Hash)]
pub enum ModalFocusKey<'a> {
    /// The detected pad type rides along because the Controller row's "Automatic (...)" value
    /// depends on it, not just on `Settings` — a hotplug alone doesn't touch `Settings` at all.
    /// The override rides along because it decides which rows wear a "use global" button —
    /// a change there moves no value in `Settings` at all.
    SettingsRow(usize, Settings, SettingsOverride, Option<GamepadType>),
    WakeToggle(bool),
    WakeButton(usize),
    PairingDigit(usize, u8),
    PairingButton,
    ForgetButton(usize),
    /// Carries label to prevent stale tiles across screen changes.
    SpeedTestButton(usize, &'a str),
    /// (focused row, its action, the host's pairing state, which trailing button is focused).
    /// The action and the pairing state are what the row's label is derived from, so they
    /// stand in for it — see `app::state::hostmenu::host_menu_row`.
    MenuRow(usize, HostAction, bool, Option<usize>),
    /// (focused row, log level, stats-overlay on, show-logs on) — any change invalidates the tile.
    DiagnosticsRow(usize, LogLevelOverride, bool, bool),
    ExperimentalRow(usize, bool, bool, Option<bool>),
    /// (focused row, cursor-capture on, cursor-gestures on, which rows are overridden) — any
    /// change invalidates the tile.
    CursorSettingsRow(usize, bool, bool, SettingsOverride),
    /// Which `Screen::SendLogs` button is focused (0 = Cancel, 1 = Send).
    SendLogsButton(usize),
    /// Which `Screen::RemoveCollection` button is focused (0 = Remove, 1 = Cancel).
    RemoveCollectionButton(usize),
    /// (focused row, the row's name, whether it is the one already holding the card, which
    /// trailing button is focused, whether the row is being dragged) — what the focused row
    /// draws, and nothing else: the list behind it is its own tiles.
    CollectionRow(usize, &'a str, bool, Option<usize>, bool),
}

/// Scrollable modal content keys. Paired with Screen for staleness checks.
///
/// Settings has no variant here: its rows are baked one tile each, keyed by
/// [`ui::widgets::FocusRow::key`] — see [`tile::list_row`]. A single strip keyed on the
/// whole `Settings` struct meant one changed value re-rasterized every row.
#[derive(PartialEq, Eq, Hash)]
pub enum ScrollContentKey {
    /// About window's start line.
    About(usize),
}

/// Each modal's shell content keys. Value changes invalidate the shell;
/// pure focus moves don't (that's `ModalFocusKey`'s job).
///
/// The close-button hover is not in here. It changes every shell alike and belongs to none of
/// them, so `modal_shell_version` hashes it alongside whichever key it got — one place instead
/// of a `hover_close` field repeated down every variant and every arm that builds one.
#[derive(PartialEq, Eq, Hash)]
pub enum ModalShellKey<'a> {
    // Only what `render_settings` reads — the whole `Settings` struct (or the
    // dropdown row) would invalidate this key, forcing a full-screen re-raster,
    // on every keystroke or dropdown open/close. The shell draws chrome only (no
    // row content, not even the Bitrate caution — that's the focus tile's job),
    // so nothing but the title suffix can actually change it.
    Settings {
        /// The per-game screen's dim title suffix — `None` on the global one. The only thing
        /// separating the two shells.
        game: Option<&'a str>,
    },
    Wake {
        name: &'a str,
        mac_empty: bool,
        sent: bool,
    },
    Pairing {
        digits: [u8; 4],
        status: Option<&'a str>,
        busy: bool,
    },
    ForgetHost {
        name: Option<&'a str>,
    },
    HostMenu {
        name: &'a str,
        subtitle: &'a str,
        rows: usize,
    },
    WakeSettings {
        title: &'a str,
        auto: bool,
    },
    About,
    SpeedTest {
        status: &'a str,
    },
    Diagnostics {
        log_level: LogLevelOverride,
        stats_overlay: bool,
        show_logs: bool,
    },
    Experimental {
        ndl_audio_offload: bool,
        game_mode: bool,
        /// The root-probe verdict — it locks the Game mode row and rewrites its caption.
        rooted: Option<bool>,
    },
    CursorSettings {
        cursor_capture: bool,
        cursor_gestures: bool,
        over: SettingsOverride,
    },
    /// Fixed warning copy + two buttons — nothing screen-specific left to key on.
    SendLogs,
    /// The card it asks about, by what the subtitle is derived from.
    RemoveCollection {
        name: &'a str,
        games: usize,
    },
    /// The shell is title, rule and the card being moved — the rows are their own tiles, so
    /// nothing about the collections themselves belongs here except how many there are (the
    /// card's height follows the row count).
    Collections {
        card: &'a str,
        rows: usize,
    },
}
