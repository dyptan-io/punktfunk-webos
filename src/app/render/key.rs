//! What each cached tile's pixels depend on.
//!
//! `ui::cache::TileStore` decides staleness by comparing a `u64` version; these are the
//! keys that version is hashed from. Each variant carries *everything* the tile draws, so
//! adding a dependency is adding a field rather than remembering to extend a comparison.
//!
//! App-side on purpose: they name this app's screens and its `Settings`, which is exactly
//! what a widget library must not know.
use crate::core::model::{GamepadType, LogLevelOverride, Settings};

/// Focused widget in the open modal. Each variant carries its content,
/// so value changes (not just focus moves) invalidate the tile.
#[derive(PartialEq, Eq, Hash)]
pub enum ModalFocusKey {
    /// The detected pad type rides along because the Controller row's "Automatic (...)" value
    /// depends on it, not just on `Settings` — a hotplug alone doesn't touch `Settings` at all.
    SettingsRow(usize, Settings, Option<GamepadType>),
    WakeToggle(bool),
    WakeButton(usize),
    PairingDigit(usize, u8),
    PairingButton,
    ForgetButton(usize),
    /// Carries label to prevent stale tiles across screen changes.
    SpeedTestButton(usize, String),
    /// Carries label+menu flag for row list shape changes and ⋯ state.
    MenuRow(usize, String, bool),
    /// (focused row, log level, stats-overlay on, show-logs on) — any change invalidates the tile.
    DiagnosticsRow(usize, LogLevelOverride, bool, bool),
    ExperimentalRow(usize, bool, bool),
    /// (focused row, cursor-capture on, cursor-gestures on) — any change invalidates the tile.
    CursorSettingsRow(usize, bool, bool),
    /// Which `Screen::SendLogs` button is focused (0 = Cancel, 1 = Send).
    SendLogsButton(usize),
}

/// Scrollable modal content keys. Paired with Screen for staleness checks.
#[derive(PartialEq, Eq, Hash)]
pub enum ScrollContentKey {
    /// Settings row list + open dropdown row + detected pad type (see `ModalFocusKey::SettingsRow`).
    Settings(Settings, Option<usize>, Option<GamepadType>),
    /// About window's start line.
    About(usize),
}

/// Each modal's shell content keys. Value changes invalidate the shell;
/// pure focus moves don't (that's `ModalFocusKey`'s job).
#[derive(PartialEq, Eq, Hash)]
pub enum ModalShellKey {
    // Only what `render_settings` reads — the whole `Settings` struct (or the
    // dropdown row) would invalidate this key, forcing a full-screen re-raster,
    // on every keystroke or dropdown open/close. The shell draws chrome only (no
    // row content, not even the Bitrate caution — that's the focus tile's job),
    // so the only thing that can actually change it is the close-button hover.
    Settings {
        hover_close: bool,
    },
    Wake {
        name: String,
        mac_empty: bool,
        sent: bool,
        hover_close: bool,
    },
    Pairing {
        digits: [u8; 4],
        status: Option<String>,
        busy: bool,
        hover_close: bool,
    },
    ForgetHost {
        name: Option<String>,
        hover_close: bool,
    },
    HostMenu {
        name: String,
        subtitle: String,
        rows: usize,
        hover_close: bool,
    },
    WakeSettings {
        title: String,
        auto: bool,
        hover_close: bool,
    },
    About {
        hover_close: bool,
    },
    SpeedTest {
        status: String,
        hover_close: bool,
    },
    Diagnostics {
        log_level: LogLevelOverride,
        stats_overlay: bool,
        show_logs: bool,
        hover_close: bool,
    },
    Experimental {
        ndl_audio_offload: bool,
        game_mode: bool,
        hover_close: bool,
    },
    CursorSettings {
        cursor_capture: bool,
        cursor_gestures: bool,
        hover_close: bool,
    },
    /// Fixed warning copy + two buttons; only the close (X) hover varies.
    SendLogs {
        hover_close: bool,
    },
}
