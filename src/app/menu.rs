//! This app's settings vocabulary: which rows exist, what each offers, and how a pick
//! applies to `Settings`. Shared by `app::state::*` (which mutates) and `app::view::*`
//! (which builds the `ui::widgets::FocusRow` lists). Deliberately not in `ui` — `ui` holds the row
//! *widgets*, not this app's menus.
use crate::core::caps::video_caps;
use crate::core::event::MenuEvent;
use crate::core::model::{BITRATE, BITRATE_AUTOMATIC, BITRATE_MIN_KBPS};
use crate::services::store::{
    AudioRoutePref, CodecPref, ExitAction, GamepadType, GamepadUiMode, LogLevelOverride, OverrideField, Settings,
    SettingsOverride,
};
use crate::ui::focus::Dir;
use crate::ui::widgets::{FocusRow, RowSubtext};

/// This app's input vocabulary mapped onto `ui`'s spatial one. `ui` navigates by
/// direction; deciding that "Up" means a d-pad press is the app's business, and this is
/// where that translation happens once.
pub fn nav_dir(ev: MenuEvent) -> Option<Dir> {
    match ev {
        MenuEvent::Up => Some(Dir::Up),
        MenuEvent::Down => Some(Dir::Down),
        MenuEvent::Left => Some(Dir::Left),
        MenuEvent::Right => Some(Dir::Right),
        _ => None,
    }
}

/// User-requested presets: 1080p, 1440p, 4K. `(width, height, row value, dropdown name)` —
/// the row's collapsed value stays plain pixel dimensions, while the dropdown option gets
/// the friendlier name alongside them (see `resolution_dropdown_label`).
pub const RESOLUTIONS: [(u32, u32, &str, &str); 3] = [
    (1920, 1080, "1920 x 1080", "FHD"),
    (2560, 1440, "2560 x 1440", "QHD"),
    (3840, 2160, "3840 x 2160", "4K"),
];

/// Sent to host as exact wire refresh rate.
pub const REFRESH_RATES: [u32; 3] = [30, 60, 120];

/// Above this, the Bitrate row shows a dull-orange caution caption (not a hard cap). Presentation
/// only, unlike the range itself (`core::model::BITRATE_MIN_KBPS`/`BITRATE_MAX_KBPS`).
pub const BITRATE_WARN_KBPS: u32 = 150_000;

/// One row of a settings-shaped screen.
///
/// An enum rather than the bare `usize` indices this used to be, because a dozen tables
/// answer per row (labels, locks, override fields, dropdown options and their application)
/// and every one of them had a `_ =>` arm — so a new row compiled, and silently behaved like
/// whatever the fallback did. Here it does not compile until each table answers for it.
///
/// The variants past [`Self::About`] are on no list of their own: they are the Cursor
/// sub-screen's two toggles, and the per-game list's Reset action. They are the same kind of
/// thing (a row the override table is keyed by), so they are the same type — which is what
/// removed the second index space and the hand-written mapping between them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SettingsRow {
    Resolution,
    Framerate,
    Bitrate,
    /// Locked where the backend has no HEVC — there is only one decodable codec then (see
    /// [`row_lock`]).
    Codec,
    /// Directly below Codec: HDR applies only to HEVC, so the row locks on an explicit H.264
    /// pick (see [`row_lock`]) — adjacency keeps that dependency discoverable.
    Hdr,
    /// Locked where the backend is capped at stereo — the only channel count then.
    Audio,
    /// Which controller the host presents to the game — see `store::GamepadType`. Picking
    /// `DualSense` is what turns on adaptive triggers
    /// (`crate::platform::webos::dualsense`). On the per-game list directly; on the global one
    /// it lives behind [`Self::Controller`].
    Gamepad,
    /// Not a setting — a link to `Screen::ControllerSettings`, holding everything about the pad
    /// itself. Four controller rows on one list crowded out the stream settings people actually
    /// come to Settings for.
    Controller,
    /// Not a setting — a link to `Screen::CursorSettings`, directly below Controller since
    /// it's the other input-side entry. Both pointer toggles live behind it rather than on
    /// this list: neither is something a user sets more than once, and pairing them makes the
    /// gesture toggle discoverable next to the capture mode it interacts with.
    Cursor,
    /// Whether the shared gamepad shell may front the app at all — the cross-client
    /// `gamepad_ui_enabled`. Device-wide, and on the Controller sub-screen: what drives the
    /// TV and which menus it drives are one question.
    GamepadUi,
    /// When it does — the cross-client `gamepad_ui_mode`. Directly below the switch that
    /// gates it, the same adjacency Hdr keeps to Codec.
    GamepadUiMode,
    /// Which look the menus draw in — see `ui::theme::PRESETS`. Cosmetic and device-wide, so
    /// it is on the global list only, and applies the moment it is picked.
    Theme,
    /// Not a setting — a link to `Screen::Experimental` (Game mode on rooted sets). Grouped off
    /// the main list so an untested option isn't one keystroke away.
    Experimental,
    /// Not a setting — a link to `Screen::Diagnostics` (log level + stats overlay). A debug
    /// aid, not something a normal user needs to find quickly.
    Diagnostics,
    /// Not a setting — a link to `Screen::About`. Sits last: every other punktfunk client puts
    /// the version + licences at the very bottom of Settings.
    About,
    /// The Cursor sub-screen's rows. On no list here — see [`CURSOR_ROWS`].
    CursorCapture,
    CursorGestures,
    /// The action row at the foot of the *per-game* list only. It drops every override,
    /// putting the game back to what its screen inherits. The global list has no counterpart:
    /// there is nothing above it to fall back to.
    Reset,
}

/// The global list, in display order.
const GLOBAL_ROWS: [SettingsRow; 12] = [
    SettingsRow::Resolution,
    SettingsRow::Framerate,
    SettingsRow::Bitrate,
    SettingsRow::Codec,
    SettingsRow::Hdr,
    SettingsRow::Audio,
    SettingsRow::Controller,
    SettingsRow::Cursor,
    SettingsRow::Theme,
    SettingsRow::Experimental,
    SettingsRow::Diagnostics,
    SettingsRow::About,
];

/// Which rows a settings-shaped screen shows — the scope its screen carries (see
/// [`crate::core::screen::Screen::Settings`]). Both scopes share every [`SettingsRow`] and
/// therefore every mutator, dropdown list and lock in this module.
pub use crate::core::screen::SettingsScope;

/// The per-game list. No links out — Experimental and Diagnostics are device-wide, so neither they nor anything
/// behind them appears. Cursor keeps its link row and its sub-screen, exactly as on the global
/// list — it holds two toggles either way, and duplicating that layout only for this screen
/// would make the same settings look like two different things. See `store::SettingsOverride`.
const GAME_ROWS: [SettingsRow; 9] = [
    SettingsRow::Resolution,
    SettingsRow::Framerate,
    SettingsRow::Bitrate,
    SettingsRow::Codec,
    SettingsRow::Hdr,
    SettingsRow::Audio,
    SettingsRow::Gamepad,
    SettingsRow::Cursor,
    SettingsRow::Reset,
];

/// The Cursor sub-screen's list, in display order (see `app::view::cursorsettings::rows`).
/// Its rows are [`SettingsRow`]s like any other, so the override table, the toggle values and
/// the per-game marks all reach them without a second index space in between.
pub const CURSOR_ROWS: [SettingsRow; 2] = [SettingsRow::CursorCapture, SettingsRow::CursorGestures];

/// The Controller sub-screen's list, in display order (see `app::view::controllersettings`).
/// Same rule as [`CURSOR_ROWS`]: they are [`SettingsRow`]s like any other, so the override
/// table and the mutators reach them without a second index space.
pub const CONTROLLER_ROWS: [SettingsRow; 3] =
    [SettingsRow::Gamepad, SettingsRow::GamepadUi, SettingsRow::GamepadUiMode];

/// Experimental modal rows (see `app::view::experimental::rows`). A separate type from
/// [`SettingsRow`]: these are device-wide toggles that no per-game override reaches.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ExpRow {
    /// Locked whenever [`exp_row_lock`] returns a reason. Always listed, locked rather than
    /// hidden when it can't be used.
    GameMode,
    /// Which audio route a session builds — see `store::AudioRoutePref`. The one dropdown on
    /// this screen. Experimental because two of its three picks are hardware paths that no
    /// runtime probe can verify; locked to the software route where there is no NDL plane.
    AudioProcessing,
    /// Opens `Screen::HdrCalibration` — measures this panel's HDR volume so the client stops
    /// advertising one TV's numbers to every TV. Experimental because the measurement is by eye
    /// and the patterns run on the video plane outside a session.
    HdrCalibration,
    /// Rumble from the pad's `0xD1` coil lane (`session::pad_audio`). On a libScePad title
    /// (Spider-Man, GTA V Enhanced…) it is the ONLY vibration there is — those games drive the
    /// coils and never the classic motors. Here rather than on the main list because it is on by
    /// default and wants no attention, and because the lane is verified on one panel so far.
    PadHaptics,
    /// The pad's own speaker, on either transport. Same reasoning as [`Self::PadHaptics`].
    PadSpeaker,
}

/// Order is display order. `AudioProcessing` stays at index 1 so its dropdown's `(Screen, row)`
/// tile key does not move; a new row goes on the end for the same reason.
pub const EXP_ROWS: [ExpRow; 5] = [
    ExpRow::GameMode,
    ExpRow::AudioProcessing,
    ExpRow::HdrCalibration,
    ExpRow::PadHaptics,
    ExpRow::PadSpeaker,
];

/// Whether this build links the shared shell at all. Every Linux target does (see Cargo.toml);
/// elsewhere the row is listed and locked rather than missing, which keeps the screen's row
/// indices the same everywhere.
pub(crate) const CONSOLE_UI_BUILT: bool = cfg!(target_os = "linux");

/// Display position of [`ExpRow::AudioProcessing`] — the row a dropdown can hang off, which is
/// what `DropdownState::row` names.
pub const EXP_ROW_AUDIO: usize = 1;

/// Diagnostics modal row indices (see `app::view::diagnostics::rows`). Log level keeps
/// index 0 so its dropdown's `(Screen, row)` tile key stays stable.
pub const DIAG_ROW_LOG_LEVEL: usize = 0;
pub const DIAG_ROW_STATS_OVERLAY: usize = 1;
/// Menu-driven mirror of the Yellow-button log overlay — for remotes without one.
pub const DIAG_ROW_SHOW_LOGS: usize = 2;
/// Sends the current session log to the paired host, falling back to the developer.
pub const DIAG_ROW_SEND_LOGS: usize = 3;
pub const DIAGNOSTICS_ROW_COUNT: usize = 4;

/// Whether one focusable row is offered at all.
///
/// **The sole visibility predicate.** A row would be hidden only when nothing the user can reach
/// from inside the app could ever make it usable — the environment decides it. Nothing qualifies
/// today; the predicate stays as the one place that would. Everything a *setting* constrains stays on
/// screen and greys out instead, so the dependency is visible rather than inferred from a
/// vanishing row: see [`row_lock`].
///
/// Consequence worth keeping: no user action changes this, so the display↔logical mapping is
/// fixed for the run and no site has to re-anchor focus after a mutation.
pub(crate) fn row_shown(_row: SettingsRow) -> bool {
    true
}

/// Why a row is shown but not editable, or `None` while it is. Distinct from [`row_shown`]:
/// a lock can lift without a restart (switching codec away from H.264, plugging in a pad),
/// where an unshown row cannot become shown at all this run. The caption text is
/// [`app::view::settings`]'s business (it needs the OS release to phrase it); this is the
/// predicate both the renderer and the input path read, so a greyed row and a rejected
/// keypress can't disagree.
#[derive(Clone, Copy)]
pub(crate) enum RowLock {
    /// HDR under an explicit H.264 pick: the host never resolves HDR for such a session, so the
    /// toggle would be a no-op. `Automatic` leaves it editable — HEVC may still be resolved.
    /// Application is gated on the *negotiated* codec too, see `session::connect`.
    HdrNeedsHevc,
    /// The active backend has no HDR at all (NDL v1) — nothing to toggle either way.
    NoHdr,
    /// One decodable codec, so `codec_prefs` collapses to a single entry.
    OneCodec,
    /// The active backend decodes one channel count (NDL v1), so no route could offer more.
    StereoOnly,
    /// The *selected audio processing* carries stereo only (`AudioRoutePref::max_channels`) —
    /// i.e. audio offload, whose plane decodes nothing wider. Named as a route lock rather than
    /// [`Self::StereoOnly`] because, unlike the TV's Sound Out or the backend, this limit follows
    /// from a setting the user owns and the caption can say where to change it.
    RouteStereoOnly,
    /// Nothing is plugged into the TV, so there is no controller to describe to the host.
    NoGamepad,
    /// This build has no shared shell linked (see [`CONSOLE_UI_BUILT`]), so there is nothing
    /// for the switch to hand the menus to.
    NoShell,
    /// The console switch above is off, so its mode picks nothing.
    ConsoleOff,
}

/// Why an Experimental row can't be changed. Same contract as [`RowLock`]: the predicate that
/// greys the row is the one that rejects the keypress, so the two can't disagree.
#[derive(Clone, Copy)]
pub(crate) enum ExpRowLock {
    /// The root probe hasn't answered yet.
    RootUnknown,
    /// Not a rooted TV, so Game mode has no way to reach `settingsservice`.
    NotRooted,
    /// No NDL audio plane on this backend (webOS 4 and below), so the software route is
    /// the whole list — see `store::AudioRoutePref::available`.
    SoftwareOnly,
    /// HDR is switched off in Settings, so there is no PQ signal to measure a panel with.
    HdrOff,
}

/// `rooted` is the root-probe verdict, `None` while it is still running.
pub(crate) fn exp_row_lock(row: ExpRow, settings: &Settings, rooted: Option<bool>) -> Option<ExpRowLock> {
    match (row, rooted) {
        (ExpRow::GameMode, None) => Some(ExpRowLock::RootUnknown),
        (ExpRow::GameMode, Some(false)) => Some(ExpRowLock::NotRooted),
        (ExpRow::AudioProcessing, _) if audio_routes().len() < 2 => Some(ExpRowLock::SoftwareOnly),
        (ExpRow::HdrCalibration, _) if !settings.hdr_enabled || !video_caps().hdr => Some(ExpRowLock::HdrOff),
        // The pad rows never lock: both default on, and a pad that cannot play a lane simply
        // never has it declared (`pad_audio::caps_for`) — nothing for the user to be told.
        (ExpRow::GameMode, Some(true))
        | (ExpRow::AudioProcessing, _)
        | (ExpRow::HdrCalibration, _)
        | (ExpRow::PadHaptics, _)
        | (ExpRow::PadSpeaker, _) => None,
    }
}

/// The audio routes offered here, in display order (see `store::AudioRoutePref::available`).
pub(crate) fn audio_routes() -> &'static [AudioRoutePref] {
    AudioRoutePref::available(video_caps())
}

/// Dropdown labels for [`ExpRow::AudioProcessing`] — the screen's only dropdown, so there is no
/// per-row table here the way `dropdown_options` is one.
pub(crate) fn audio_route_options() -> Vec<Label> {
    audio_routes().iter().map(|&r| audio_route_label(r).into()).collect()
}

pub(crate) fn audio_route_current_index(settings: &Settings) -> usize {
    audio_routes()
        .iter()
        .position(|&r| r == settings.audio_route)
        .unwrap_or(0)
}

/// Applies an audio-route pick. The layout preference is left alone — it is narrowed per session
/// by `session::connect`'s `Negotiated::clamp`, not rewritten here — but a route that carries one
/// layout locks the Audio row (`RowLock::RouteStereoOnly`), so the two are set in the order that
/// row's caption names.
pub(crate) fn apply_audio_route(settings: &mut Settings, choice_index: usize) {
    let Some(&route) = audio_routes().get(choice_index) else {
        return;
    };
    settings.audio_route = route;
    settings.clamp_to_caps();
}

/// What is known about this pairing's power rights on the host menu's host — the screen state
/// behind the exit-behaviour row, and the one predicate that both greys it and rejects a
/// keypress on it (same contract as [`ExpRowLock`]).
///
/// Owned here rather than in `services::power` because two of the four cases are local facts
/// the host is never asked about, and because the captions that render them are the app's.
/// Only [`Self::Rights`] carries a host answer; everything else is a reason we don't have one,
/// which matters because a refused pairing needs widening on the host while an unreachable one
/// may simply be asleep — the same locked row for opposite reasons.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PowerAccess {
    /// No pairing with this host, so there is no access mask to carry a power grant.
    NotPaired,
    /// The probe hasn't answered yet.
    Unknown,
    /// The host never answered — asleep or off the network.
    Unreachable,
    /// The host answered, but has no power actions at all: it predates the route (a 404), so
    /// there is no grant to widen and nothing to say about permissions.
    Unsupported,
    /// What the host says this pairing may invoke. Empty means it answered and offers nothing.
    Rights(crate::services::power::PowerRights),
}

impl PowerAccess {
    /// Whether the row is usable at all — the host answered and offers something.
    pub(crate) fn unlocked(self) -> bool {
        matches!(self, Self::Rights(r) if r.any())
    }

    /// Whether one pick would actually be accepted. Unknown-yet and unreachable say yes: the
    /// row is locked in those states anyway, and claiming a stored pick is impossible on no
    /// evidence would be worse than saying nothing.
    pub(crate) fn allows(self, action: ExitAction) -> bool {
        match self {
            Self::Rights(r) => r.allows(action),
            Self::NotPaired | Self::Unknown | Self::Unreachable | Self::Unsupported => true,
        }
    }
}

/// Host power row indices. The dropdown hangs off `POWER_ROW_EXIT`, which is what
/// `DropdownState::row` names and what keys its overlay tile.
pub const POWER_ROW_AUTO: usize = 0;
pub const POWER_ROW_EXIT: usize = 1;
pub const POWER_ROW_COUNT: usize = 2;

pub(crate) fn exit_action_label(action: ExitAction) -> &'static str {
    match action {
        ExitAction::None => "None",
        ExitAction::Sleep => "Sleep",
        ExitAction::Shutdown => "Shut down",
    }
}

/// What each pick actually does to the machine. Spelled out per value because the labels name
/// a power state and not its cost: one of these ends the host's uptime.
pub(crate) fn exit_action_caption(action: ExitAction) -> &'static str {
    match action {
        ExitAction::None => "The host keeps running after you leave",
        ExitAction::Sleep => "Suspend the host — Wake-on-LAN brings it back",
        ExitAction::Shutdown => "Power the host off completely",
    }
}

/// Dropdown labels for the Wake screen's exit-behaviour row.
pub(crate) fn exit_action_options() -> Vec<Label> {
    ExitAction::ALL.iter().map(|&a| exit_action_label(a).into()).collect()
}

pub(crate) fn exit_action_current_index(action: ExitAction) -> usize {
    ExitAction::ALL.iter().position(|&a| a == action).unwrap_or(0)
}

/// `detected` is the attached pad per `gamepad::detect_type` — `None` with nothing attached
/// (or an unrecognized pad), which is what locks the Controller row.
pub(crate) fn row_lock(row: SettingsRow, settings: &Settings, detected: Option<GamepadType>) -> Option<RowLock> {
    let caps = video_caps();
    match row {
        SettingsRow::Hdr if !caps.hdr => Some(RowLock::NoHdr),
        SettingsRow::Hdr if settings.codec == CodecPref::H264 => Some(RowLock::HdrNeedsHevc),
        SettingsRow::Codec if caps.codec_prefs().len() < 2 => Some(RowLock::OneCodec),
        // Device before route: where the client itself decodes stereo only, no audio-processing
        // pick could widen it, and naming one would send the user somewhere that cannot help.
        SettingsRow::Audio if channel_options_up_to(caps.max_channels).len() < 2 => Some(RowLock::StereoOnly),
        SettingsRow::Audio if audio_channel_options(settings).len() < 2 => Some(RowLock::RouteStereoOnly),
        SettingsRow::Gamepad if detected.is_none() => Some(RowLock::NoGamepad),
        // Listed and locked rather than hidden where no shell is linked, so the row indices are
        // the same on every target — the same reason [`CONSOLE_UI_BUILT`] exists.
        SettingsRow::GamepadUi | SettingsRow::GamepadUiMode if !CONSOLE_UI_BUILT => Some(RowLock::NoShell),
        // The mode decides nothing while the switch above it is off. Greyed, not hidden: the
        // dependency is the point, and it sits directly under the row that lifts it.
        SettingsRow::GamepadUiMode if !settings.gamepad_ui => Some(RowLock::ConsoleOff),
        _ => None,
    }
}

/// Logical `ROW_*` indices currently visible, in display order — the single source of truth
/// every visibility-aware helper derives from. Settings-independent (see [`row_shown`]), so
/// this mapping is fixed for the run.
pub fn settings_visible_logical_rows(set: SettingsScope) -> impl Iterator<Item = SettingsRow> {
    let rows: &'static [SettingsRow] = match set {
        SettingsScope::Global => &GLOBAL_ROWS,
        SettingsScope::Game => &GAME_ROWS,
    };
    rows.iter().copied().filter(|&row| row_shown(row))
}

/// Live row count — what the list is actually showing this run.
pub fn settings_row_count(set: SettingsScope) -> usize {
    settings_visible_logical_rows(set).count()
}

/// On-screen row position -> the row shown there, skipping past any hidden rows. `None` past
/// the end of the list — with the rows a type rather than an index there is no "the position
/// itself" to fall back on, which is what used to let an out-of-range focus address a row.
pub fn settings_logical_row(set: SettingsScope, display: usize) -> Option<SettingsRow> {
    settings_visible_logical_rows(set).nth(display)
}

/// Current value of `row` if it is a toggle — the start point the switch slide animates
/// from. `None` for every other row kind.
pub fn toggle_value(settings: &Settings, row: SettingsRow) -> Option<bool> {
    match row {
        SettingsRow::Hdr => Some(settings.hdr_enabled),
        SettingsRow::GamepadUi => Some(settings.gamepad_ui),
        SettingsRow::CursorCapture => Some(settings.cursor_capture),
        SettingsRow::CursorGestures => Some(settings.cursor_gestures),
        _ => None,
    }
}

/// The override fields a settings row owns — one table for both the mark and the capture, so
/// a row can't show a dot for a field it doesn't record. The Cursor *link* row owns both
/// toggles behind it, or a game overriding only a cursor one shows a dot on its card and
/// nothing on the list saying where it came from.
fn row_fields(row: SettingsRow) -> &'static [OverrideField] {
    match row {
        SettingsRow::Resolution => &[OverrideField::Mode],
        SettingsRow::Framerate => &[OverrideField::RefreshHz],
        SettingsRow::Bitrate => &[OverrideField::BitrateKbps],
        SettingsRow::Hdr => &[OverrideField::HdrEnabled],
        SettingsRow::Codec => &[OverrideField::Codec],
        SettingsRow::Audio => &[OverrideField::AudioChannels],
        // Global-only today, so no dot renders; named with Gamepad anyway, so putting it on
        // the per-game list later cannot silently lose one.
        SettingsRow::Gamepad | SettingsRow::Controller => &[OverrideField::GamepadKind],
        SettingsRow::CursorCapture => &[OverrideField::CursorCapture],
        SettingsRow::CursorGestures => &[OverrideField::CursorGestures],
        SettingsRow::Cursor => &[OverrideField::CursorCapture, OverrideField::CursorGestures],
        // Rows that override nothing: device-wide switches, links out or an action.
        SettingsRow::GamepadUi
        | SettingsRow::GamepadUiMode
        | SettingsRow::Theme
        | SettingsRow::Experimental
        | SettingsRow::Diagnostics
        | SettingsRow::About
        | SettingsRow::Reset => &[],
    }
}

/// Whether `row` currently overrides the global value — what decides that the row gets a
/// "use global" delete affordance.
pub fn override_is_set(over: &SettingsOverride, row: SettingsRow) -> bool {
    row_fields(row).iter().any(|&f| over.is_set(f))
}

/// Marks `row` as overriding the global and, on the focused row, names the gesture that
/// clears it. Every settings-shaped screen goes through here, so the mark and the affordance
/// explaining it can't drift apart or be forgotten by a new sub-screen.
///
/// Focused only because subtext renders nowhere else; a caption the row already carries wins,
/// since a lock explains why the row can't be used at all. The colour lives here rather than
/// in `ui`, which knows only that some rows carry a mark.
pub fn decorate_override(row: &mut FocusRow, over: &SettingsOverride, logical: SettingsRow, focused: bool) {
    row.mark = override_is_set(over, logical).then(|| crate::ui::theme::palette().warning);
    if row.mark.is_some() && focused && row.subtext.is_none() {
        row.subtext = Some(RowSubtext::hint("Reset to use the global setting"));
    }
}

/// Drops `row` back to inheriting the global — every field it owns, so clearing the Cursor
/// link row clears both toggles behind it.
pub fn override_clear(over: &mut SettingsOverride, row: SettingsRow) {
    for &field in row_fields(row) {
        over.clear(field);
    }
}

/// Records `row`'s value from `edited` against the `global` the game inherits from — a pick
/// landing on the global's own value stores nothing (see [`SettingsOverride::capture`]).
///
/// Strictly the fields the row owns: an H.264 pick's effect on HDR is
/// `Settings::presentable`'s job, not a second override written behind the user's back and
/// stranded the moment they clear the Codec row.
pub fn override_capture(over: &mut SettingsOverride, row: SettingsRow, edited: &Settings, global: &Settings) {
    // An adjustable row owning no field would have its edit silently reverted by
    // `edit_game_override`'s re-merge. Unreachable today (link rows don't adjust).
    debug_assert!(
        !row_fields(row).is_empty(),
        "settings row {row:?} is adjustable but overrides nothing"
    );
    for &field in row_fields(row) {
        over.capture(field, edited, global);
    }
}

pub fn cycle_index(current: usize, len: usize, forward: bool) -> usize {
    if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}

pub fn resolution_label(width: u32, height: u32) -> String {
    RESOLUTIONS
        .iter()
        .find(|(w, h, _, _)| *w == width && *h == height)
        .map_or_else(|| format!("{width}x{height}"), |(_, _, s, _)| s.to_string())
}

/// Dropdown-only label — the row's own value stays the plain pixel dimensions
/// ([`resolution_label`]); the option list gets the friendlier name plus the vertical count
/// alongside them, e.g. "FHD - 1080p - 1920x1080".
fn resolution_dropdown_label(width: u32, height: u32, name: &str) -> String {
    format!("{name} - {height}p - {width}x{height}")
}

pub const LOG_LEVEL_OPTIONS: [LogLevelOverride; 4] = [
    LogLevelOverride::Debug,
    LogLevelOverride::Info,
    LogLevelOverride::Warn,
    LogLevelOverride::Error,
];

pub fn log_level_label(l: LogLevelOverride) -> &'static str {
    match l {
        LogLevelOverride::Debug => "Debug",
        LogLevelOverride::Info => "Info",
        LogLevelOverride::Warn => "Warn",
        LogLevelOverride::Error => "Error",
    }
}

/// Diagnostics' one dropdown row — options list + current index, same shape as
/// `dropdown_options`/`dropdown_current_index` but for `Screen::Diagnostics`
/// rather than a `Settings` row (there is no row-index namespace to share).
pub fn log_level_dropdown_options() -> Vec<Label> {
    LOG_LEVEL_OPTIONS.iter().map(|&l| log_level_label(l).into()).collect()
}

pub fn log_level_dropdown_current_index(level: LogLevelOverride) -> usize {
    LOG_LEVEL_OPTIONS.iter().position(|&o| o == level).unwrap_or(0)
}

pub fn codec_label(pref: CodecPref) -> &'static str {
    match pref {
        CodecPref::Auto => "Automatic",
        CodecPref::H264 => "H.264",
        CodecPref::Hevc => "HEVC",
    }
}

/// Controller types offered, in display order. `Automatic` first (the default, and what an
/// existing install already has); the rest are ordered by how likely a TV user is to own one.
pub const GAMEPAD_TYPES: [GamepadType; 6] = [
    GamepadType::Auto,
    GamepadType::DualSense,
    GamepadType::DualSenseEdge,
    GamepadType::DualShock4,
    GamepadType::XboxOne,
    GamepadType::SwitchPro,
];

pub fn gamepad_label(t: GamepadType) -> &'static str {
    match t {
        GamepadType::Auto => "Automatic",
        GamepadType::XboxOne => "Xbox",
        GamepadType::DualShock4 => "DualShock 4",
        GamepadType::DualSense => "DualSense",
        GamepadType::DualSenseEdge => "DualSense Edge",
        GamepadType::SwitchPro => "Switch Pro",
    }
}

/// "Automatic", or "Automatic (`DualSense`)" once a recognized pad is attached — what `Auto`
/// will actually resolve to for this session (see `gamepad::detect_type`), rather than leaving
/// the user to guess.
pub fn gamepad_auto_label(detected: Option<GamepadType>) -> String {
    detected.map_or_else(
        || "Automatic".to_string(),
        |t| format!("Automatic ({})", gamepad_label(t)),
    )
}

/// Every channel count this client can label; what is *offered* is [`audio_channel_options`].
const AUDIO_CHANNELS: [(u8, &str); 3] = [(2, "Stereo"), (6, "5.1 surround"), (8, "7.1 surround")];

/// The channel counts offered: what this client can decode, capped by what the selected route can
/// put on a speaker (`AudioRoutePref::max_channels`).
///
/// Filtered by the route because that ceiling is static — the Opus plane carries nothing above
/// stereo — so offering a width the route would only make the handshake clamp is a pick that does
/// nothing. NOT filtered by the TV's current Sound Out: that one changes
/// under a running app and is applied per session instead (`session::connect`'s
/// `Negotiated::clamp`).
///
/// The stored preference is left alone by all of this — see [`audio_row_channels`].
///
/// A prefix of [`AUDIO_CHANNELS`] rather than a fresh `Vec`, because the list is ascending and
/// the filter is a ceiling — and because the callers that only want the count ask on every
/// settings-geometry query, which is several times a frame.
pub fn audio_channel_options(settings: &Settings) -> &'static [(u8, &'static str)] {
    channel_options_up_to(settings.audio_route.max_channels(video_caps()))
}

/// The prefix of [`AUDIO_CHANNELS`] a `max`-channel ceiling leaves — the shared filter, so the
/// device ceiling and the route ceiling are read the same way.
fn channel_options_up_to(max: u8) -> &'static [(u8, &'static str)] {
    let offered = AUDIO_CHANNELS.iter().take_while(|(c, _)| *c <= max).count();
    &AUDIO_CHANNELS[..offered]
}

/// The layout this row shows, and the one the handshake will ask for: the stored preference held
/// down to what the route carries.
///
/// The preference itself is never rewritten (`Settings::clamp_to_caps` only applies the
/// decoder-wide ceiling), so a 5.1 pick narrowed to stereo by the offload route comes back whole
/// on the software one.
pub(crate) fn audio_row_channels(settings: &Settings) -> u8 {
    settings
        .audio_channels
        .min(settings.audio_route.max_channels(video_caps()))
}

/// Dropdown label for a route. Names the decode step and the sink behind it — the pick is a
/// hardware path, and this screen's audience is the one that wants the API named.
pub(crate) fn audio_route_label(route: AudioRoutePref) -> &'static str {
    match route {
        AudioRoutePref::Software => "Software (SDL)",
        AudioRoutePref::NdlOpus => "Offload (NDL)",
    }
}

pub(crate) fn audio_label(channels: u8) -> String {
    AUDIO_CHANNELS
        .iter()
        .find(|(c, _)| *c == channels)
        .map_or_else(|| format!("{channels} channels"), |(_, s)| (*s).to_string())
}

/// One dropdown option's label. A `Cow`, because most rows offer `&'static str` constants and
/// only three (resolution, framerate, the detected-gamepad line) format anything: the open
/// dropdown's labels are rebuilt every frame it is up, so a `String` per option per frame is
/// an allocation storm for text that is usually already in the binary.
pub type Label = std::borrow::Cow<'static, str>;

/// Dropdown labels for a row.
pub fn dropdown_options(row: SettingsRow, settings: &Settings, detected: Option<GamepadType>) -> Vec<Label> {
    match row {
        SettingsRow::Theme => crate::ui::theme::PRESETS.iter().map(|t| t.name.into()).collect(),
        SettingsRow::GamepadUiMode => GamepadUiMode::ALL.iter().map(|m| m.label().into()).collect(),
        SettingsRow::Resolution => RESOLUTIONS
            .iter()
            .map(|(w, h, _, name)| resolution_dropdown_label(*w, *h, name).into())
            .collect(),
        SettingsRow::Framerate => REFRESH_RATES.iter().map(|hz| format!("{hz} Hz").into()).collect(),
        SettingsRow::Codec => video_caps()
            .codec_prefs()
            .iter()
            .map(|&p| codec_label(p).into())
            .collect(),
        SettingsRow::Audio => audio_channel_options(settings)
            .iter()
            .map(|(_, s)| (*s).into())
            .collect(),
        SettingsRow::Gamepad => GAMEPAD_TYPES
            .iter()
            .map(|&t| {
                if t == GamepadType::Auto {
                    gamepad_auto_label(detected).into()
                } else {
                    gamepad_label(t).into()
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// How many options a dropdown row offers, without building the label list — the compose
/// path needs only the count, and `dropdown_options` allocates a `String` per entry.
pub fn dropdown_option_count(row: SettingsRow, settings: &Settings) -> usize {
    match row {
        SettingsRow::Theme => crate::ui::theme::PRESETS.len(),
        SettingsRow::GamepadUiMode => GamepadUiMode::ALL.len(),
        SettingsRow::Resolution => RESOLUTIONS.len(),
        SettingsRow::Framerate => REFRESH_RATES.len(),
        SettingsRow::Codec => video_caps().codec_prefs().len(),
        SettingsRow::Audio => audio_channel_options(settings).len(),
        SettingsRow::Gamepad => GAMEPAD_TYPES.len(),
        _ => 0,
    }
}

/// Current dropdown index for a row's setting.
pub fn dropdown_current_index(settings: &Settings, row: SettingsRow) -> usize {
    match row {
        SettingsRow::Theme => crate::ui::theme::PRESETS
            .iter()
            .position(|t| t.choice == settings.theme)
            .unwrap_or(0),
        SettingsRow::GamepadUiMode => GamepadUiMode::ALL
            .iter()
            .position(|&m| m == settings.gamepad_ui_mode)
            .unwrap_or(0),
        SettingsRow::Resolution => RESOLUTIONS
            .iter()
            .position(|(w, h, _, _)| *w == settings.width && *h == settings.height)
            .unwrap_or(0),
        SettingsRow::Framerate => REFRESH_RATES
            .iter()
            .position(|hz| *hz == settings.refresh_hz)
            .unwrap_or(0),
        SettingsRow::Codec => video_caps()
            .codec_prefs()
            .iter()
            .position(|&p| p == settings.codec)
            .unwrap_or(0),
        SettingsRow::Audio => audio_channel_options(settings)
            .iter()
            .position(|(c, _)| *c == audio_row_channels(settings))
            .unwrap_or(0),
        SettingsRow::Gamepad => GAMEPAD_TYPES
            .iter()
            .position(|&t| t == settings.gamepad_type)
            .unwrap_or(0),
        _ => 0,
    }
}

/// Applies a dropdown pick. Refuses on a locked row (see [`row_lock`]) rather than trusting
/// every caller to have checked first — the same guard [`adjust_setting`] applies, so there is
/// one place a locked row's value is actually protected, not one per call site.
pub fn apply_dropdown_choice(
    settings: &mut Settings,
    row: SettingsRow,
    choice_index: usize,
    detected: Option<GamepadType>,
) {
    if row_lock(row, settings, detected).is_some() {
        return;
    }
    match row {
        SettingsRow::Theme => {
            if let Some(t) = crate::ui::theme::PRESETS.get(choice_index) {
                settings.theme = t.choice;
            }
        }
        SettingsRow::GamepadUiMode => {
            if let Some(&mode) = GamepadUiMode::ALL.get(choice_index) {
                settings.gamepad_ui_mode = mode;
            }
        }
        SettingsRow::Resolution => {
            if let Some((w, h, _, _)) = RESOLUTIONS.get(choice_index) {
                settings.width = *w;
                settings.height = *h;
            }
        }
        SettingsRow::Framerate => {
            if let Some(hz) = REFRESH_RATES.get(choice_index) {
                settings.refresh_hz = *hz;
            }
        }
        SettingsRow::Codec => {
            if let Some(&pref) = video_caps().codec_prefs().get(choice_index) {
                settings.codec = pref;
                // H.264 never resolves HDR (see `RowLock::HdrNeedsHevc`).
                if pref == CodecPref::H264 {
                    settings.hdr_enabled = false;
                }
            }
        }
        SettingsRow::Audio => {
            if let Some((channels, _)) = audio_channel_options(settings).get(choice_index) {
                settings.audio_channels = *channels;
            }
        }
        SettingsRow::Gamepad => {
            if let Some(&t) = GAMEPAD_TYPES.get(choice_index) {
                settings.gamepad_type = t;
            }
        }
        _ => {}
    }
}

/// Apply left/right adjustment to a setting row. Returns true if changed.
///
/// Every dropdown row shares one arm — the option list is the authority on how many entries
/// there are (see [`dropdown_option_count`]), so a new dropdown row needs no code here.
///
/// A locked row (see [`row_lock`]) refuses every adjustment: the same predicate that greys it
/// is what rejects the keypress, so nothing can edit a value the UI shows as fixed.
pub fn adjust_setting(settings: &mut Settings, row: SettingsRow, forward: bool, detected: Option<GamepadType>) -> bool {
    if row_lock(row, settings, detected).is_some() {
        return false;
    }
    match row {
        SettingsRow::Bitrate => {
            if settings.bitrate_kbps == BITRATE_AUTOMATIC {
                if forward {
                    settings.bitrate_kbps = BITRATE_MIN_KBPS;
                }
                // Already at the floor going backward from Automatic — nothing below it.
            } else if !forward && settings.bitrate_kbps == BITRATE_MIN_KBPS {
                settings.bitrate_kbps = BITRATE_AUTOMATIC;
            } else {
                let stop = BITRATE.index(settings.bitrate_kbps) as i32 + if forward { 1 } else { -1 };
                settings.bitrate_kbps = BITRATE.value(stop);
            }
            true
        }
        SettingsRow::Hdr => {
            settings.hdr_enabled = !settings.hdr_enabled;
            true
        }
        SettingsRow::GamepadUi => {
            settings.gamepad_ui = !settings.gamepad_ui;
            true
        }
        SettingsRow::CursorCapture => {
            settings.cursor_capture = !settings.cursor_capture;
            true
        }
        SettingsRow::CursorGestures => {
            settings.cursor_gestures = !settings.cursor_gestures;
            true
        }
        // Every dropdown row shares this arm, and the link/action rows fall through it with
        // an empty option list — `dropdown_option_count` is the one table that decides which
        // is which.
        SettingsRow::Resolution
        | SettingsRow::Framerate
        | SettingsRow::Theme
        | SettingsRow::GamepadUiMode
        | SettingsRow::Controller
        | SettingsRow::Codec
        | SettingsRow::Audio
        | SettingsRow::Gamepad
        | SettingsRow::Cursor
        | SettingsRow::Experimental
        | SettingsRow::Diagnostics
        | SettingsRow::About
        | SettingsRow::Reset => {
            let len = dropdown_option_count(row, settings);
            if len == 0 {
                return false;
            }
            let next = cycle_index(dropdown_current_index(settings, row), len, forward);
            apply_dropdown_choice(settings, row, next, detected);
            true
        }
    }
}

/// Sets the Bitrate row directly from a dragged/clicked `fraction` (0.0-1.0 along the track),
/// snapped to the nearest [`BITRATE`] stop — the mouse-drag counterpart of [`adjust_setting`]'s
/// per-notch `Left`/`Right`. The bottom stop snaps to `Automatic`, mirroring the notch
/// `adjust_setting` leaves for it at the low end.
pub fn set_bitrate_fraction(settings: &mut Settings, fraction: f32) {
    let stepped = BITRATE.value(BITRATE.stop_at(fraction));
    settings.bitrate_kbps = if stepped <= BITRATE_MIN_KBPS {
        BITRATE_AUTOMATIC
    } else {
        stepped
    };
}
