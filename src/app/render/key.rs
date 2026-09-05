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
use crate::app::menu::PowerAccess;
use crate::app::screens::rowbuttons::RowButton;
use crate::app::state::hdrcalibration::HdrStep;
use crate::app::state::hostmenu::HostAction;
use crate::core::model::{ExitAction, HdrDisplay};

/// Focused widget in the open modal. Each variant carries its content,
/// so value changes (not just focus moves) invalidate the tile.
#[derive(PartialEq, Eq, Hash)]
pub enum ModalFocusKey<'a> {
    HostPowerRow {
        row: usize,
        auto: bool,
        exit: ExitAction,
        access: PowerAccess,
    },
    WakeButton(usize),
    PairingDigit(usize, u8),
    PairingButton,
    ForgetButton(usize),
    /// Carries label to prevent stale tiles across screen changes.
    SpeedTestButton(usize, &'a str),
    /// (focused row, its action, the host's pairing state, which trailing button is focused).
    /// The action and the pairing state are what the row's label is derived from, so they
    /// stand in for it — see `app::state::hostmenu::host_menu_row`.
    MenuRow(usize, HostAction, bool, Option<RowButton>, Option<ExitAction>),
    /// (focused row, which measurement is being made, the volume it has reached, whether the
    /// pattern feed has stalled, whether the tick has focus) — the card's copy, its slider and
    /// its caution all move with these.
    HdrCalibrationRow(usize, HdrStep, HdrDisplay, bool, Option<RowButton>),
    /// Which `Screen::SendLogs` button is focused (0 = Cancel, 1 = Send).
    SendLogsButton(usize),
    /// Which `Screen::RemoveCollection` button is focused (0 = Remove, 1 = Cancel).
    RemoveCollectionButton(usize),
    /// Which `Screen::ResetHdrCalibration` button is focused (0 = Clear, 1 = Cancel).
    ResetHdrButton(usize),
    /// (focused row, the row's name, whether it is the one already holding the card, which
    /// trailing button is focused, whether the row is being dragged) — what the focused row
    /// draws, and nothing else: the list behind it is its own tiles.
    CollectionRow(usize, &'a str, bool, Option<RowButton>, bool),
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
    Wake {
        name: &'a str,
        mac_empty: bool,
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
        /// What the power row currently says — it changes with the host coming up or going
        /// down, which moves no value the other fields here would notice.
        power: Option<ExitAction>,
    },
    HostPower {
        title: &'a str,
        auto: bool,
        exit: ExitAction,
        /// Rides along because it decides both the exit row's caption and whether it is greyed
        /// — the probe landing changes the card without moving any stored value.
        access: PowerAccess,
    },
    About,
    SpeedTest {
        status: &'a str,
    },
    /// The calibration card: its subtitle is the step's instruction and its rows carry the
    /// measurement, so both move the whole shell.
    HdrCalibration {
        step: HdrStep,
        display: HdrDisplay,
        stalled: bool,
    },
    /// Fixed warning copy + two buttons — nothing screen-specific left to key on.
    SendLogs,
    /// Fixed copy too: what it clears is the one calibration there is.
    ResetHdrCalibration,
    /// The card it asks about, by what the subtitle is derived from.
    RemoveCollection {
        name: &'a str,
        games: usize,
    },
    /// The shell is title, rule and the card being moved — the rows are their own tiles, so
    /// nothing about the collections themselves belongs here except how many there are (the
    /// card's height follows the row count).
    Collections {
        /// Picks the heading — see `App::collections_target_held`.
        held: bool,
        card: &'a str,
        rows: usize,
    },
}
