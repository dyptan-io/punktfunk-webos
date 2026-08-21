//! This app's settings vocabulary: which rows exist, what each offers, and how a pick
//! applies to `Settings`. Shared by `app::state::*` (which mutates) and `app::view::*`
//! (which builds the `ui::widgets::FocusRow` lists). Deliberately not in `ui` — `ui` holds the row
//! *widgets*, not this app's menus.
use crate::core::caps::video_caps;
use crate::core::event::MenuEvent;
use crate::core::model::{BITRATE_AUTOMATIC, BITRATE_MAX_KBPS, BITRATE_MIN_KBPS, BITRATE_STEP_KBPS};
use crate::services::store::{
    CodecPref, GamepadType, LogLevelOverride, OverrideField, Settings, SettingsOverride, VideoBackend,
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
    /// Which decode pipeline to load — see `store::VideoBackend`. Only offered where there is
    /// a choice (webOS 3.5-4.x, see `caps::smp_selectable`), and above Codec deliberately: the
    /// pick is what decides whether HEVC and HDR exist as options at all.
    VideoBackend,
    /// Locked where the backend has no HEVC — there is only one decodable codec then (see
    /// [`row_lock`]).
    Codec,
    /// Directly below Codec: HDR applies only to HEVC, so the row locks on an explicit H.264
    /// pick (see [`row_lock`]) — adjacency keeps that dependency discoverable.
    Hdr,
    /// Locked where the backend is capped at stereo — the only channel count then.
    Audio,
    /// Which controller the host presents to the game — see `store::GamepadType`. Last of the
    /// real settings: it's the only input-side one, and picking `DualSense` is what turns on
    /// adaptive triggers (`crate::platform::webos::dualsense`).
    Gamepad,
    /// Not a setting — a link to `Screen::CursorSettings`, directly below Controller since
    /// it's the other input-side entry. Both pointer toggles live behind it rather than on
    /// this list: neither is something a user sets more than once, and pairing them makes the
    /// gesture toggle discoverable next to the capture mode it interacts with.
    Cursor,
    /// Which look the menus draw in — see `ui::theme::PRESETS`. Cosmetic and device-wide, so
    /// it is on the global list only, and applies the moment it is picked.
    Theme,
    /// Not a setting — a link to `Screen::Experimental` (unstable toggles: NDL audio offload,
    /// and Game mode on rooted sets). Grouped off the main list so an untested option isn't
    /// one keystroke away.
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
const GLOBAL_ROWS: [SettingsRow; 13] = [
    SettingsRow::Resolution,
    SettingsRow::Framerate,
    SettingsRow::Bitrate,
    SettingsRow::VideoBackend,
    SettingsRow::Codec,
    SettingsRow::Hdr,
    SettingsRow::Audio,
    SettingsRow::Gamepad,
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

/// The per-game list. No [`SettingsRow::VideoBackend`] (`caps::set_backend` is a
/// process-global, so a per-game backend would need an apply/restore around every launch) and
/// no links out — Experimental and Diagnostics are device-wide, so neither they nor anything
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

/// Experimental modal rows (see `app::view::experimental::rows`). A separate type from
/// [`SettingsRow`]: these are device-wide toggles that no per-game override reaches.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ExpRow {
    HwAudio,
    /// Locked whenever [`exp_row_lock`] returns a reason. Always listed, locked rather than
    /// hidden when it can't be used.
    GameMode,
}

pub const EXP_ROWS: [ExpRow; 2] = [ExpRow::HwAudio, ExpRow::GameMode];

/// Diagnostics modal row indices (see `app::view::diagnostics::rows`). Log level keeps
/// index 0 so its dropdown's `(Screen, row)` tile key stays stable.
pub const DIAG_ROW_LOG_LEVEL: usize = 0;
pub const DIAG_ROW_STATS_OVERLAY: usize = 1;
/// Menu-driven mirror of the Yellow-button log overlay — for remotes without one.
pub const DIAG_ROW_SHOW_LOGS: usize = 2;
/// Uploads the current session's log file to the developer (see `app::sendlogs`).
/// An action row, not a setting — Confirm opens a warning/confirmation modal first.
pub const DIAG_ROW_SEND_LOGS: usize = 3;
pub const DIAGNOSTICS_ROW_COUNT: usize = 4;

/// Whether one focusable row is offered at all.
///
/// **The sole visibility predicate.** A row is hidden only when nothing the user can reach from
/// inside the app could ever make it usable — the environment decides it (the OS release for the
/// backend row, root for Experimental's Game mode). Everything a *setting* constrains stays on
/// screen and greys out instead, so the dependency is visible rather than inferred from a
/// vanishing row: see [`row_lock`].
///
/// Consequence worth keeping: no user action changes this, so the display↔logical mapping is
/// fixed for the run and no site has to re-anchor focus after a mutation.
pub(crate) fn row_shown(row: SettingsRow) -> bool {
    match row {
        // Only a choice where NDL is the narrow v1 generation — everywhere else NDL v2 is
        // strictly better and the row would be a trap.
        SettingsRow::VideoBackend => crate::core::caps::smp_selectable(),
        _ => true,
    }
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
    /// The backend is capped at stereo, leaving one channel count.
    StereoOnly,
    /// Nothing is plugged into the TV, so there is no controller to describe to the host.
    NoGamepad,
}

/// Why an Experimental row can't be changed. Same contract as [`RowLock`]: the predicate that
/// greys the row is the one that rejects the keypress, so the two can't disagree.
#[derive(Clone, Copy)]
pub(crate) enum ExpRowLock {
    /// The root probe hasn't answered yet.
    RootUnknown,
    /// Not a rooted TV, so Game mode has no way to reach `settingsservice`.
    NotRooted,
}

/// `rooted` is the root-probe verdict, `None` while it is still running.
pub(crate) fn exp_row_lock(row: ExpRow, rooted: Option<bool>) -> Option<ExpRowLock> {
    match (row, rooted) {
        (ExpRow::GameMode, None) => Some(ExpRowLock::RootUnknown),
        (ExpRow::GameMode, Some(false)) => Some(ExpRowLock::NotRooted),
        (ExpRow::GameMode, Some(true)) | (ExpRow::HwAudio, _) => None,
    }
}

/// `detected` is the attached pad per `gamepad::detect_type` — `None` with nothing attached
/// (or an unrecognized pad), which is what locks the Controller row.
pub(crate) fn row_lock(row: SettingsRow, settings: &Settings, detected: Option<GamepadType>) -> Option<RowLock> {
    let caps = video_caps();
    match row {
        SettingsRow::Hdr if !caps.hdr => Some(RowLock::NoHdr),
        SettingsRow::Hdr if settings.codec == CodecPref::H264 => Some(RowLock::HdrNeedsHevc),
        SettingsRow::Codec if caps.codec_prefs().len() < 2 => Some(RowLock::OneCodec),
        SettingsRow::Audio if audio_channel_options().len() < 2 => Some(RowLock::StereoOnly),
        SettingsRow::Gamepad if detected.is_none() => Some(RowLock::NoGamepad),
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
        SettingsRow::Gamepad => &[OverrideField::GamepadKind],
        SettingsRow::CursorCapture => &[OverrideField::CursorCapture],
        SettingsRow::CursorGestures => &[OverrideField::CursorGestures],
        SettingsRow::Cursor => &[OverrideField::CursorCapture, OverrideField::CursorGestures],
        // Rows that override nothing: the backend is a process-global, and the rest are links
        // out or an action.
        SettingsRow::VideoBackend
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

/// The channel counts offered, filtered to what the active backend can present.
///
/// A prefix of [`AUDIO_CHANNELS`] rather than a fresh `Vec`, because the list is ascending and
/// the filter is a ceiling — and because the callers that only want the count ask on every
/// settings-geometry query, which is several times a frame.
pub fn audio_channel_options() -> &'static [(u8, &'static str)] {
    let max = video_caps().max_channels;
    let offered = AUDIO_CHANNELS.iter().take_while(|(c, _)| *c <= max).count();
    &AUDIO_CHANNELS[..offered]
}

pub(crate) fn audio_label(channels: u8) -> String {
    AUDIO_CHANNELS
        .iter()
        .find(|(c, _)| *c == channels)
        .map_or_else(|| format!("{channels} channels"), |(_, s)| (*s).to_string())
}

/// The backend choices offered, in display order (NDL first — it's the default and needs no
/// wrapper `.so`). Only reachable while the row is shown (see `row_shown`).
pub const VIDEO_BACKENDS: [VideoBackend; 2] = [VideoBackend::Ndl, VideoBackend::Smp];

/// Dropdown label for a backend. Derived from the value, not a parallel list: the dropdown is
/// indexed into [`VIDEO_BACKENDS`], so two lists that drift apply the option above or below the
/// one the user picked. The row's own value column uses the short name instead.
fn video_backend_label(backend: VideoBackend) -> &'static str {
    match backend {
        VideoBackend::Ndl => "NDL (DirectMedia)",
        VideoBackend::Smp => "SMP (Media Pipeline)",
    }
}

/// One dropdown option's label. A `Cow`, because most rows offer `&'static str` constants and
/// only three (resolution, framerate, the detected-gamepad line) format anything: the open
/// dropdown's labels are rebuilt every frame it is up, so a `String` per option per frame is
/// an allocation storm for text that is usually already in the binary.
pub type Label = std::borrow::Cow<'static, str>;

/// Dropdown labels for a row.
pub fn dropdown_options(row: SettingsRow, detected: Option<GamepadType>) -> Vec<Label> {
    match row {
        SettingsRow::Theme => crate::ui::theme::PRESETS.iter().map(|t| t.name.into()).collect(),
        SettingsRow::VideoBackend => VIDEO_BACKENDS.iter().map(|&b| video_backend_label(b).into()).collect(),
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
        SettingsRow::Audio => audio_channel_options().iter().map(|(_, s)| (*s).into()).collect(),
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
pub fn dropdown_option_count(row: SettingsRow) -> usize {
    match row {
        SettingsRow::Theme => crate::ui::theme::PRESETS.len(),
        SettingsRow::VideoBackend => VIDEO_BACKENDS.len(),
        SettingsRow::Resolution => RESOLUTIONS.len(),
        SettingsRow::Framerate => REFRESH_RATES.len(),
        SettingsRow::Codec => video_caps().codec_prefs().len(),
        SettingsRow::Audio => audio_channel_options().len(),
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
        SettingsRow::Resolution => RESOLUTIONS
            .iter()
            .position(|(w, h, _, _)| *w == settings.width && *h == settings.height)
            .unwrap_or(0),
        SettingsRow::Framerate => REFRESH_RATES
            .iter()
            .position(|hz| *hz == settings.refresh_hz)
            .unwrap_or(0),
        SettingsRow::VideoBackend => VIDEO_BACKENDS
            .iter()
            .position(|&b| b == settings.video_backend)
            .unwrap_or(0),
        SettingsRow::Codec => video_caps()
            .codec_prefs()
            .iter()
            .position(|&p| p == settings.codec)
            .unwrap_or(0),
        SettingsRow::Audio => audio_channel_options()
            .iter()
            .position(|(c, _)| *c == settings.audio_channels)
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
        SettingsRow::VideoBackend => {
            if let Some(&backend) = VIDEO_BACKENDS.get(choice_index) {
                settings.video_backend = backend;
                // The pick IS the capability set (see `core::caps::set_backend`), so publish it
                // before clamping — switching back to NDL has to take a now-unpresentable HEVC
                // or HDR value with it rather than leaving it set behind a hidden row.
                crate::core::caps::set_backend(backend);
                settings.clamp_to_caps();
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
            if let Some((channels, _)) = audio_channel_options().get(choice_index) {
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
                let delta = i64::from(BITRATE_STEP_KBPS) * if forward { 1 } else { -1 };
                let next = (i64::from(settings.bitrate_kbps) + delta)
                    .clamp(i64::from(BITRATE_MIN_KBPS), i64::from(BITRATE_MAX_KBPS));
                settings.bitrate_kbps = next as u32;
            }
            true
        }
        SettingsRow::Hdr => {
            settings.hdr_enabled = !settings.hdr_enabled;
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
        | SettingsRow::VideoBackend
        | SettingsRow::Codec
        | SettingsRow::Audio
        | SettingsRow::Gamepad
        | SettingsRow::Cursor
        | SettingsRow::Experimental
        | SettingsRow::Diagnostics
        | SettingsRow::About
        | SettingsRow::Reset => {
            let len = dropdown_option_count(row);
            if len == 0 {
                return false;
            }
            let next = cycle_index(dropdown_current_index(settings, row), len, forward);
            apply_dropdown_choice(settings, row, next, detected);
            true
        }
    }
}

/// Sets the Bitrate row directly from a dragged/clicked `fraction` (0.0-1.0 along the
/// track), snapped to [`BITRATE_STEP_KBPS`] — the mouse-drag counterpart of
/// [`adjust_setting`]'s per-notch `Left`/`Right`. Below one step above the floor snaps to
/// `Automatic`, mirroring the notch `adjust_setting` leaves for it at the low end.
pub fn set_bitrate_fraction(settings: &mut Settings, fraction: f32) {
    let span = BITRATE_MAX_KBPS - BITRATE_MIN_KBPS;
    let raw = BITRATE_MIN_KBPS + (fraction.clamp(0.0, 1.0) * span as f32) as u32;
    let stepped = ((raw + BITRATE_STEP_KBPS / 2) / BITRATE_STEP_KBPS) * BITRATE_STEP_KBPS;
    settings.bitrate_kbps = if stepped <= BITRATE_MIN_KBPS {
        BITRATE_AUTOMATIC
    } else {
        stepped.clamp(BITRATE_MIN_KBPS, BITRATE_MAX_KBPS)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPES: [SettingsScope; 2] = [SettingsScope::Global, SettingsScope::Game];

    #[test]
    fn display_to_logical_is_a_bijection_over_the_visible_range() {
        for set in SCOPES {
            let visible: Vec<SettingsRow> = settings_visible_logical_rows(set).collect();
            assert_eq!(visible.len(), settings_row_count(set));
            for (display, &logical) in visible.iter().enumerate() {
                assert_eq!(settings_logical_row(set, display), Some(logical));
            }
            assert_eq!(
                settings_logical_row(set, visible.len()),
                None,
                "past the end of the list"
            );
            for (i, row) in visible.iter().enumerate() {
                assert!(!visible[..i].contains(row), "{row:?} is listed twice");
            }
        }
    }

    #[test]
    fn only_shown_rows_are_reachable_from_a_display_position() {
        for set in SCOPES {
            for display in 0..settings_row_count(set) {
                assert!(settings_logical_row(set, display).is_some_and(row_shown));
            }
        }
    }

    /// The per-game list carries no links out and no backend row, but keeps Cursor and Reset.
    #[test]
    fn the_game_scope_lists_its_own_rows_only() {
        let game: Vec<SettingsRow> = settings_visible_logical_rows(SettingsScope::Game).collect();
        for absent in [
            SettingsRow::VideoBackend,
            SettingsRow::Experimental,
            SettingsRow::Diagnostics,
            SettingsRow::About,
        ] {
            assert!(
                !game.contains(&absent),
                "row {absent:?} must not be on the per-game list"
            );
        }
        assert!(game.contains(&SettingsRow::Cursor));
        assert!(game.contains(&SettingsRow::Reset));
        assert!(!settings_visible_logical_rows(SettingsScope::Global).any(|r| r == SettingsRow::Reset));
    }

    /// The sub-screen's rows are `SettingsRow`s like any other, so the override table reaches
    /// them — that is the whole reason the two index spaces were merged.
    #[test]
    fn the_cursor_link_row_owns_both_rows_behind_it() {
        let on = Settings {
            cursor_capture: !Settings::default().cursor_capture,
            cursor_gestures: !Settings::default().cursor_gestures,
            ..Settings::default()
        };
        for row in CURSOR_ROWS {
            let mut over = SettingsOverride::default();
            override_capture(&mut over, row, &on, &Settings::default());
            assert!(override_is_set(&over, row));
            assert!(
                override_is_set(&over, SettingsRow::Cursor),
                "the link row shows the mark"
            );
            override_clear(&mut over, SettingsRow::Cursor);
            assert!(!override_is_set(&over, row), "clearing the link row clears both");
        }
    }

    #[test]
    fn cycle_index_wraps_both_ways() {
        assert_eq!(cycle_index(0, 3, true), 1);
        assert_eq!(cycle_index(2, 3, true), 0);
        assert_eq!(cycle_index(0, 3, false), 2);
        assert_eq!(cycle_index(1, 3, false), 0);
        assert_eq!(cycle_index(0, 1, true), 0);
        assert_eq!(cycle_index(0, 1, false), 0);
    }

    fn bitrate_at(fraction: f32) -> u32 {
        let mut s = Settings::default();
        set_bitrate_fraction(&mut s, fraction);
        s.bitrate_kbps
    }

    #[test]
    fn the_bottom_of_the_bitrate_track_is_the_automatic_notch() {
        assert_eq!(bitrate_at(0.0), BITRATE_AUTOMATIC);
        assert_eq!(bitrate_at(-1.0), BITRATE_AUTOMATIC);
        // Half a step above the floor still rounds down onto it, so still Automatic.
        let half_step = (BITRATE_STEP_KBPS / 2 - 1) as f32 / (BITRATE_MAX_KBPS - BITRATE_MIN_KBPS) as f32;
        assert_eq!(bitrate_at(half_step), BITRATE_AUTOMATIC);
        // One full step above it is the first real value.
        let one_step = BITRATE_STEP_KBPS as f32 / (BITRATE_MAX_KBPS - BITRATE_MIN_KBPS) as f32;
        assert_eq!(bitrate_at(one_step), BITRATE_MIN_KBPS + BITRATE_STEP_KBPS);
    }

    #[test]
    fn the_bitrate_track_stays_stepped_and_inside_its_range() {
        for i in 0..=100 {
            let v = bitrate_at(i as f32 / 100.0);
            if v == BITRATE_AUTOMATIC {
                continue;
            }
            assert_eq!(v % BITRATE_STEP_KBPS, 0, "{v} is off the step grid");
            assert!((BITRATE_MIN_KBPS..=BITRATE_MAX_KBPS).contains(&v), "{v} out of range");
        }
        assert_eq!(bitrate_at(1.0), BITRATE_MAX_KBPS);
        assert_eq!(bitrate_at(2.0), BITRATE_MAX_KBPS);
    }

    /// The offered list is a prefix, so it must stay ascending — a ceiling filter over an
    /// unsorted table would silently drop a middle entry.
    #[test]
    fn the_channel_table_is_ascending_so_the_offered_list_is_a_prefix() {
        assert!(AUDIO_CHANNELS.windows(2).all(|w| w[0].0 < w[1].0));
        let offered = audio_channel_options();
        assert_eq!(offered, &AUDIO_CHANNELS[..offered.len()]);
    }

    #[test]
    fn exp_game_mode_is_locked_until_the_root_probe_says_yes() {
        assert!(exp_row_lock(ExpRow::GameMode, None).is_some());
        assert!(exp_row_lock(ExpRow::GameMode, Some(false)).is_some());
        assert!(exp_row_lock(ExpRow::GameMode, Some(true)).is_none());
        assert!(exp_row_lock(ExpRow::HwAudio, None).is_none());
    }
}
