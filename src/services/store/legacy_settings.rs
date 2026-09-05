//! The settings object as this client wrote it before the shared schema: read once by the
//! migration in `legacy`, never written. Every field defaults, so a partial document reads.
use crate::core::model::{AudioRoutePref, CodecPref, GamepadType, GamepadUiMode, LogLevelOverride};

/// Stream settings: resolution/framerate/bitrate/HDR/codec, plus the input and diagnostics
/// toggles the Settings screens expose.
///
/// `serde(default)` on the container, so every field falls back to [`LegacySettings::default`] when a
/// settings.json written by an older build doesn't carry it — adding a field needs nothing else.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(super) struct LegacySettings {
    pub width: u32,
    pub height: u32,
    /// Refresh rate (30/60/120) — sent to the host as the exact wire `Mode.refresh_hz`.
    pub refresh_hz: u32,
    /// [`BITRATE_AUTOMATIC`] (`punktfunk_core`'s own client-side AIMD bitrate controller) or a
    /// fixed [`BITRATE_MIN_KBPS`]..=[`BITRATE_MAX_KBPS`], adjusted via the settings slider.
    pub bitrate_kbps: u32,
    pub hdr_enabled: bool,
    /// The measured panel volume — see [`HdrDisplay`]. The defaults are the values this client
    /// shipped hardcoded for every TV (an LG CX's), so an uncalibrated set behaves exactly as it
    /// always has.
    pub hdr_peak_nits: u16,
    pub hdr_frame_avg_nits: u16,
    /// See [`HdrDisplay::black_code`]. The default is the code nearest the 0.0005 nits this
    /// client used to send.
    pub hdr_black_code: u16,
    /// Whether the user has actually run the calibration. It gates one behaviour beyond the
    /// numbers: a calibrated panel pins its own volume on the decoder and stops applying the
    /// host's per-content mastering metadata, since re-tone-mapping to the content would undo
    /// the measurement (see `session::pump`).
    pub hdr_calibrated: bool,
    /// Preferred session codec — see [`CodecPref`].
    pub codec: CodecPref,
    /// Whether the in-stream stats overlay (resolution/codec, measured fps, drops,
    /// decoder feed time) is drawn in the top-right corner during a stream. Off by
    /// default; takes effect on the next stream.
    pub stats_overlay: bool,
    /// Requested audio channel count: 2 (stereo), 6 (5.1) or 8 (7.1). The host clamps to
    /// what it can actually capture, and the *resolved* count drives the decoder and
    /// playback layout — `audio.rs` has always handled up to 8; only the request was
    /// pinned at stereo.
    pub audio_channels: u8,
    /// On-device log verbosity — see [`LogLevelOverride`]. Persisted, so a user's
    /// choice in Diagnostics survives restarts (fresh install defaults to `Info`);
    /// applied live via `logger::set_level_override` the moment it's changed. A
    /// `TELEMETRY_LEVEL` launch (`logger::launch_level_override`) still overrides
    /// the persisted value for that run — see `store::load`.
    pub log_level_override: LogLevelOverride,
    /// Diagnostics' "Show logs" toggle, applied at startup (`App::new`). Distinct
    /// from the Yellow-button overlay cycle (`runtime`'s log-overlay state), which
    /// stays ephemeral and never writes here.
    pub show_logs: bool,
    /// Which controller the host presents to the game — see [`GamepadType`]. Defaults to
    /// `Auto`, which mirrors the attached pad (so a `DualSense` gets adaptive triggers without
    /// anyone having to find this setting); pick a kind explicitly to override that. Takes
    /// effect on the next stream, since it rides the handshake.
    pub gamepad_type: GamepadType,
    /// `DualSense` audio haptics — the coil lane of the `0xD1` pad-audio plane, rendered as
    /// rumble on this client (`session::pad_audio`). Off = the lane is not declared to the host.
    pub pad_haptics: bool,
    /// The pad-speaker lane, declared only for a Bluetooth pad — the `0x36` report over the Luna
    /// bus is its one transport, so a USB pad leaves it off however this reads.
    pub pad_speaker: bool,
    /// Let the TV capture the pointer for the host in-stream. On by default — most cards are
    /// games, where a relative pointer is what the game expects; each host's Desktop card
    /// overrides it back off (see [`desktop_capture_override`]).
    ///
    /// On: local cursor hidden, relative `MouseMove` deltas sent (absolute coords stop at the
    /// panel edge), host draws the only cursor. Off: absolute `MouseMoveAbs`, and `CLIENT_CAP_CURSOR` tells a
    /// capable host to stop compositing its own so the local pointer stays visible — otherwise
    /// two cursors or none. Only the mouse follows this flag — a USB keyboard is grabbed either
    /// way, or the compositor sees modifiers and fights the host pointer. Takes effect next stream.
    pub cursor_capture: bool,
    /// Ask the TV to switch to its Game picture mode for the duration of a stream (the
    /// app-plane stand-in for HDMI ALLM — see `platform::webos::game_mode`). Off by default;
    /// unverified on non-rooted installs, so it rides the Experimental screen. Applied at
    /// stream start (SDR "game" / HDR "hdrGame" per the negotiated colour path) and reverted
    /// on stream exit.
    pub game_mode: bool,
    /// Where this session's audio is decoded and played — see [`AudioRoutePref`]. Takes effect on
    /// the next stream, and caps the channel layouts the Audio row offers.
    ///
    /// **No route decides whether NDL's audio plane exists** — every accepted V2 load has one,
    /// since NDL only paces the picture against a fed plane. The routes differ in what RIDES it:
    /// `run_clock_plane`'s silent metronome, or the host's Opus.
    pub audio_route: AudioRoutePref,
    /// Resolve the Magic Remote's OK button into left click / right click / drag by how long
    /// it's held (see `platform::webos::mouse::RemoteButtons`). Off by default — with it
    /// off, OK stays the plain immediate left click it has always been, since a remote with
    /// no working Red button then has no other way to left-click. Off also means no added
    /// wait on the release.
    pub cursor_gestures: bool,
    /// Draw the shared gamepad shell (`pf-console-ui`) instead of this client's own menus.
    /// WHEN it takes over is [`Settings::gamepad_ui_mode`]; this is whether it may at all.
    ///
    /// Stored under the shell's own unprefixed key rather than a `webos.` namespace: the
    /// shell's Settings rows read and write these very keys, so the two UIs edit one setting
    /// instead of two that have to be kept in step.
    pub gamepad_ui: bool,
    /// When [`Settings::gamepad_ui`] actually fronts the app. Read per menu entry, so a change
    /// — or a pad appearing — lands on the next return to the menu.
    pub gamepad_ui_mode: GamepadUiMode,
}
