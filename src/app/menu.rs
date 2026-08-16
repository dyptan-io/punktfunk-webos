//! This app's settings vocabulary: which rows exist, what each offers, and how a pick
//! applies to `Settings`. Shared by `app::state::*` (which mutates) and `app::view::*`
//! (which builds the `ui::widgets::FocusRow` lists). Deliberately not in `ui` — `ui` holds the row
//! *widgets*, not this app's menus.
use crate::core::caps::video_caps;
use crate::core::event::MenuEvent;
use crate::services::store::{CodecPref, GamepadType, LogLevelOverride, Settings, VideoBackend};
use crate::ui::focus::Dir;

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

/// Slider range: 10-200 Mbps, 5 Mbps steps.
pub const BITRATE_MIN_KBPS: u32 = 10_000;
pub const BITRATE_MAX_KBPS: u32 = 200_000;
pub const BITRATE_STEP_KBPS: u32 = 5_000;
/// Sentinel one notch below `BITRATE_MIN_KBPS` on the slider: `punktfunk_core::client::NativeClient`
/// arms its own client-side AIMD bitrate controller (`punktfunk_core::abr`) precisely when it's
/// asked to connect with `bitrate_kbps == 0` — it reacts to unrecoverable frames, heavy loss,
/// one-way-delay rise, and (via `session.rs`'s `report_decode_us` call) decode latency, backing off
/// or climbing every ~750ms. A fixed Mbps number, however carefully picked, never adapts to a link
/// that degrades mid-session — this does.
pub const BITRATE_AUTOMATIC: u32 = 0;
/// Above this, the Bitrate row shows a dull-orange caution caption (not a hard cap).
pub const BITRATE_WARN_KBPS: u32 = 150_000;

/// Row indices for settings modal.
pub const ROW_RESOLUTION: usize = 0;
pub const ROW_FRAMERATE: usize = 1;
pub const ROW_BITRATE: usize = 2;
/// Which decode pipeline to load — see `store::VideoBackend`. Only offered where there is a
/// choice (webOS 3.5-4.x, see `caps::smp_selectable`), and above Codec deliberately: the
/// pick is what decides whether HEVC and HDR exist as options at all.
pub const ROW_VIDEO_BACKEND: usize = 3;
/// Locked where the backend has no HEVC — there is only one decodable codec then (see `row_lock`).
pub const ROW_CODEC: usize = 4;
/// Directly below Codec: HDR applies only to HEVC, so the row locks on an explicit
/// H.264 pick (see `row_lock`) — adjacency keeps that dependency discoverable.
pub const ROW_HDR: usize = 5;
/// Locked where the backend is capped at stereo — the only channel count then (see `row_lock`).
pub const ROW_AUDIO: usize = 6;
/// Which controller the host presents to the game — see `store::GamepadType`. Last of the
/// real settings: it's the only input-side one, and picking `DualSense` is what turns on
/// adaptive triggers (`crate::platform::webos::dualsense`).
pub const ROW_GAMEPAD: usize = 7;
/// Not a setting — a link to `Screen::CursorSettings`, directly below Controller since it's
/// the other input-side entry. Both pointer toggles live behind it (see
/// `app::view::cursorsettings::rows`) rather than on this list: neither is something a user
/// sets more than once, and pairing them makes the gesture toggle discoverable next to the
/// capture mode it interacts with.
pub const ROW_CURSOR: usize = 8;
/// Not a setting — a link to `Screen::Experimental` (unstable toggles, currently the
/// frame pacer). Grouped off the main list so an untested option isn't one keystroke away.
pub const ROW_EXPERIMENTAL: usize = 9;
/// Not a setting — a link to `Screen::Diagnostics` (log level + stats overlay).
/// A debug aid, not something a normal user needs to find quickly.
pub const ROW_DIAGNOSTICS: usize = 10;
/// Not a setting — a link to `Screen::About`. Sits last: every other punktfunk
/// client puts the version + licences at the very bottom of Settings, and a
/// `RowKind::Action` row costs nothing extra to render.
pub const ROW_ABOUT: usize = 11;
pub const SETTINGS_ROW_COUNT: usize = 12;

/// Experimental modal row indices (see `app::view::experimental::rows`).
pub const EXP_ROW_FRAME_PACER: usize = 0;
/// Only present on rooted TVs, so it's the last row when shown.
pub const EXP_ROW_GAME_MODE: usize = 1;

/// Cursor modal row indices (see `app::view::cursorsettings::rows`).
pub const CURSOR_ROW_CAPTURE: usize = 0;
pub const CURSOR_ROW_GESTURES: usize = 1;
pub const CURSOR_ROW_COUNT: usize = 2;

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
pub(crate) fn row_shown(row: usize) -> bool {
    match row {
        // Only a choice where NDL is the narrow v1 generation — everywhere else NDL v2 is
        // strictly better and the row would be a trap.
        ROW_VIDEO_BACKEND => crate::core::caps::smp_selectable(),
        _ => true,
    }
}

/// Why a row is shown but not editable, or `None` while it is. Distinct from [`row_shown`]:
/// a lock can lift without a restart (switching codec away from H.264, plugging in a pad),
/// where an unshown row cannot become shown at all this run. The caption text is
/// [`app::view::settings`]'s business (it needs the OS release to phrase it); this is the
/// predicate both the renderer and the input path read, so a greyed row and a rejected
/// keypress can't disagree.
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

/// `detected` is the attached pad per `gamepad::detect_type` — `None` with nothing attached
/// (or an unrecognized pad), which is what locks the Controller row.
pub(crate) fn row_lock(row: usize, settings: &Settings, detected: Option<GamepadType>) -> Option<RowLock> {
    let caps = video_caps();
    match row {
        ROW_HDR if !caps.hdr => Some(RowLock::NoHdr),
        ROW_HDR if settings.codec == CodecPref::H264 => Some(RowLock::HdrNeedsHevc),
        ROW_CODEC if caps.codec_prefs().len() < 2 => Some(RowLock::OneCodec),
        ROW_AUDIO if audio_option_count() < 2 => Some(RowLock::StereoOnly),
        ROW_GAMEPAD if detected.is_none() => Some(RowLock::NoGamepad),
        _ => None,
    }
}

/// Logical `ROW_*` indices currently visible, in display order — the single source of truth
/// every visibility-aware helper derives from. Settings-independent (see [`row_shown`]), so
/// this mapping is fixed for the run.
pub fn settings_visible_logical_rows() -> impl Iterator<Item = usize> {
    (0..SETTINGS_ROW_COUNT).filter(|&row| row_shown(row))
}

/// Live row count (vs. `SETTINGS_ROW_COUNT`, the maximum).
pub fn settings_row_count() -> usize {
    settings_visible_logical_rows().count()
}

/// On-screen row position -> logical `ROW_*` index, skipping past any hidden rows.
pub fn settings_logical_row(display: usize) -> usize {
    settings_visible_logical_rows().nth(display).unwrap_or(display)
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
pub fn log_level_dropdown_options() -> Vec<String> {
    LOG_LEVEL_OPTIONS
        .iter()
        .map(|&l| log_level_label(l).to_string())
        .collect()
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
pub fn audio_channel_options() -> Vec<(u8, &'static str)> {
    let max = video_caps().max_channels;
    AUDIO_CHANNELS.iter().copied().filter(|(c, _)| *c <= max).collect()
}

/// How many channel counts are offered, without building the list — `row_shown` asks this
/// once per row on every settings-geometry query, which is several times a frame.
pub fn audio_option_count() -> usize {
    let max = video_caps().max_channels;
    AUDIO_CHANNELS.iter().filter(|(c, _)| *c <= max).count()
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

/// Dropdown labels for a row.
pub fn dropdown_options(row_index: usize, detected: Option<GamepadType>) -> Vec<String> {
    match row_index {
        ROW_VIDEO_BACKEND => VIDEO_BACKENDS.iter().map(|&b| video_backend_label(b).into()).collect(),
        ROW_RESOLUTION => RESOLUTIONS
            .iter()
            .map(|(w, h, _, name)| resolution_dropdown_label(*w, *h, name))
            .collect(),
        ROW_FRAMERATE => REFRESH_RATES.iter().map(|hz| format!("{hz} Hz")).collect(),
        ROW_CODEC => video_caps()
            .codec_prefs()
            .iter()
            .map(|&p| codec_label(p).to_string())
            .collect(),
        ROW_AUDIO => audio_channel_options().iter().map(|(_, s)| (*s).to_string()).collect(),
        ROW_GAMEPAD => GAMEPAD_TYPES
            .iter()
            .map(|&t| {
                if t == GamepadType::Auto {
                    gamepad_auto_label(detected)
                } else {
                    gamepad_label(t).to_string()
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// How many options a dropdown row offers, without building the label list — the compose
/// path needs only the count, and `dropdown_options` allocates a `String` per entry.
pub fn dropdown_option_count(row_index: usize) -> usize {
    match row_index {
        ROW_VIDEO_BACKEND => VIDEO_BACKENDS.len(),
        ROW_RESOLUTION => RESOLUTIONS.len(),
        ROW_FRAMERATE => REFRESH_RATES.len(),
        ROW_CODEC => video_caps().codec_prefs().len(),
        ROW_AUDIO => audio_option_count(),
        ROW_GAMEPAD => GAMEPAD_TYPES.len(),
        _ => 0,
    }
}

/// Current dropdown index for a row's setting.
pub fn dropdown_current_index(settings: &Settings, row_index: usize) -> usize {
    match row_index {
        ROW_RESOLUTION => RESOLUTIONS
            .iter()
            .position(|(w, h, _, _)| *w == settings.width && *h == settings.height)
            .unwrap_or(0),
        ROW_FRAMERATE => REFRESH_RATES
            .iter()
            .position(|hz| *hz == settings.refresh_hz)
            .unwrap_or(0),
        ROW_VIDEO_BACKEND => VIDEO_BACKENDS
            .iter()
            .position(|&b| b == settings.video_backend)
            .unwrap_or(0),
        ROW_CODEC => video_caps()
            .codec_prefs()
            .iter()
            .position(|&p| p == settings.codec)
            .unwrap_or(0),
        ROW_AUDIO => audio_channel_options()
            .iter()
            .position(|(c, _)| *c == settings.audio_channels)
            .unwrap_or(0),
        ROW_GAMEPAD => GAMEPAD_TYPES
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
    row_index: usize,
    choice_index: usize,
    detected: Option<GamepadType>,
) {
    if row_lock(row_index, settings, detected).is_some() {
        return;
    }
    match row_index {
        ROW_RESOLUTION => {
            if let Some((w, h, _, _)) = RESOLUTIONS.get(choice_index) {
                settings.width = *w;
                settings.height = *h;
            }
        }
        ROW_FRAMERATE => {
            if let Some(hz) = REFRESH_RATES.get(choice_index) {
                settings.refresh_hz = *hz;
            }
        }
        ROW_VIDEO_BACKEND => {
            if let Some(&backend) = VIDEO_BACKENDS.get(choice_index) {
                settings.video_backend = backend;
                // The pick IS the capability set (see `core::caps::set_backend`), so publish it
                // before clamping — switching back to NDL has to take a now-unpresentable HEVC
                // or HDR value with it rather than leaving it set behind a hidden row.
                crate::core::caps::set_backend(backend);
                settings.clamp_to_caps();
            }
        }
        ROW_CODEC => {
            if let Some(&pref) = video_caps().codec_prefs().get(choice_index) {
                settings.codec = pref;
                // H.264 never resolves HDR (see `RowLock::HdrNeedsHevc`).
                if pref == CodecPref::H264 {
                    settings.hdr_enabled = false;
                }
            }
        }
        ROW_AUDIO => {
            if let Some((channels, _)) = audio_channel_options().get(choice_index) {
                settings.audio_channels = *channels;
            }
        }
        ROW_GAMEPAD => {
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
pub fn adjust_setting(settings: &mut Settings, row_index: usize, forward: bool, detected: Option<GamepadType>) -> bool {
    if row_lock(row_index, settings, detected).is_some() {
        return false;
    }
    match row_index {
        ROW_BITRATE => {
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
        ROW_HDR => {
            settings.hdr_enabled = !settings.hdr_enabled;
            true
        }
        row => {
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
