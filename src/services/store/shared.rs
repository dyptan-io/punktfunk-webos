//! The settings document, in punktfunk's shared shape.
//!
//! `settings.json` persists [`pf_client_core::trust::Settings`] — the same struct the desktop
//! shells, the session binary and the Android client write. This module is the only place that
//! knows how this client's in-memory [`Settings`] maps onto it, so there is ONE stored schema
//! and two presentations of it: the existing webOS UI, and the shared gamepad shell, which
//! speaks `trust::Settings` natively and therefore needs no conversion at all.
//!
//! Anything punktfunk has no field for rides in `trust::Settings::extra`, a `#[serde(flatten)]`
//! map that every writer round-trips untouched. That is what keeps a TV-only row — HDR
//! calibration, the LG game-mode toggle, the log level — from either being dropped by another
//! client or forced into the shared struct. Android's platform rows use the same mechanism.
//!
//! ⚠ Two in-memory views of one file can drift within a session. The shell re-reads through
//! `SettingsStore::load` before every mutation, and the flip must reload the other side when it
//! switches; nothing here can enforce that.

use pf_client_core::trust;

use crate::core::model::{CodecPref, GamepadType, Settings};

/// Prefix for rows only this client has. Namespaced so a future shared field of the same name
/// cannot collide with what a TV persisted.
const P: &str = "webos.";

fn key(name: &str) -> String {
    format!("{P}{name}")
}

fn get<T: serde::de::DeserializeOwned>(t: &trust::Settings, name: &str) -> Option<T> {
    t.extra.get(&key(name)).cloned().and_then(|v| serde_json::from_value(v).ok())
}

fn put<T: serde::Serialize>(t: &mut trust::Settings, name: &str, value: &T) {
    if let Ok(v) = serde_json::to_value(value) {
        t.extra.insert(key(name), v);
    }
}

/// Every field this client used to persist under its own name and now writes under [`P`].
/// Seeing any of them UNPREFIXED is what identifies a document written before the move.
const MOVED: &[&str] = &[
    "hdr_peak_nits",
    "hdr_frame_avg_nits",
    "hdr_black_code",
    "hdr_calibrated",
    "log_level_override",
    "show_logs",
    "game_mode",
    "audio_route",
    "cursor_gestures",
    "theme",
    "stats_overlay",
    "gamepad_type",
    "cursor_capture",
];

/// Whether a `settings` object predates the shared schema.
///
/// 🛑 Not a `from_value` attempt: this client's `Settings` and `trust::Settings` both default
/// every field, so BOTH parse almost anything and neither failing is a signal. The presence of
/// an unprefixed moved key is, because the new shape can only ever write them under `webos.`.
/// Getting this wrong loses a user's settings silently — the old object would parse as a shared
/// one, its unrecognised keys would drift into `extra`, and the UI would show defaults.
pub fn legacy_shape(settings: &serde_json::Value) -> bool {
    let Some(map) = settings.as_object() else {
        return false;
    };
    MOVED.iter().any(|k| map.contains_key(*k))
}

/// This client's settings as the shared document.
///
/// The scalar stream fields are mapped for real, not parked in `extra`: resolution, refresh,
/// bitrate, HDR on/off, audio channels and the two pad-audio lanes mean exactly the same thing
/// on every client, and sharing them is the point of the exercise.
pub fn to_shared(s: &Settings) -> trust::Settings {
    let mut t = trust::Settings {
        width: s.width,
        height: s.height,
        refresh_hz: s.refresh_hz,
        bitrate_kbps: s.bitrate_kbps,
        hdr_enabled: s.hdr_enabled,
        audio_channels: s.audio_channels,
        pad_haptics: s.pad_haptics,
        // Shared `pad_speaker` is a DESTINATION, not a switch: "pad" (the pad's own speaker),
        // "mix" (fold into stream audio) or "off". This client only offers pad-or-nothing.
        pad_speaker: if s.pad_speaker { "pad" } else { "off" }.to_string(),
        show_stats: s.stats_overlay,
        codec: match s.codec {
            CodecPref::Auto => "auto",
            CodecPref::H264 => "h264",
            CodecPref::Hevc => "hevc",
        }
        .to_string(),
        // This client offers capture as a switch, not a two-name mode.
        mouse_mode: if s.cursor_capture { "capture" } else { "desktop" }.to_string(),
        gamepad: shared_gamepad(s.gamepad_type),
        ..trust::Settings::default()
    };

    put(&mut t, "hdr_peak_nits", &s.hdr_peak_nits);
    put(&mut t, "hdr_frame_avg_nits", &s.hdr_frame_avg_nits);
    put(&mut t, "hdr_black_code", &s.hdr_black_code);
    put(&mut t, "hdr_calibrated", &s.hdr_calibrated);
    put(&mut t, "log_level_override", &s.log_level_override);
    put(&mut t, "show_logs", &s.show_logs);
    put(&mut t, "game_mode", &s.game_mode);
    put(&mut t, "audio_route", &s.audio_route);
    put(&mut t, "cursor_gestures", &s.cursor_gestures);
    put(&mut t, "theme", &s.theme);
    t
}

/// The shared document as this client's settings. Absent keys fall back to [`Settings::default`],
/// which is what a document written by another client (or an older build) looks like.
pub fn from_shared(t: &trust::Settings) -> Settings {
    let d = Settings::default();
    Settings {
        width: t.width,
        height: t.height,
        refresh_hz: t.refresh_hz,
        bitrate_kbps: t.bitrate_kbps,
        hdr_enabled: t.hdr_enabled,
        audio_channels: t.audio_channels,
        pad_haptics: t.pad_haptics,
        // "mix" is a declared TODO upstream that renders as off; only "pad" is on here.
        pad_speaker: t.pad_speaker == "pad",
        stats_overlay: t.show_stats,
        codec: match t.codec.as_str() {
            "h264" => CodecPref::H264,
            "hevc" => CodecPref::Hevc,
            // "av1" too: this client cannot decode it, so it reads as the host's choice
            // rather than a codec the UI would show as selected and never deliver.
            _ => CodecPref::Auto,
        },
        cursor_capture: t.mouse_mode != "desktop",
        gamepad_type: local_gamepad(&t.gamepad).unwrap_or(d.gamepad_type),
        hdr_peak_nits: get(t, "hdr_peak_nits").unwrap_or(d.hdr_peak_nits),
        hdr_frame_avg_nits: get(t, "hdr_frame_avg_nits").unwrap_or(d.hdr_frame_avg_nits),
        hdr_black_code: get(t, "hdr_black_code").unwrap_or(d.hdr_black_code),
        hdr_calibrated: get(t, "hdr_calibrated").unwrap_or(d.hdr_calibrated),
        log_level_override: get(t, "log_level_override").unwrap_or(d.log_level_override),
        show_logs: get(t, "show_logs").unwrap_or(d.show_logs),
        game_mode: get(t, "game_mode").unwrap_or(d.game_mode),
        audio_route: get(t, "audio_route").unwrap_or(d.audio_route),
        cursor_gestures: get(t, "cursor_gestures").unwrap_or(d.cursor_gestures),
        theme: get(t, "theme").unwrap_or(d.theme),
    }
}

/// The shared pad name. Lossless in both directions — `GamepadPref` carries every kind this
/// client offers, Edge and Switch Pro included — so the pad choice is a genuinely shared field
/// rather than a TV-only one parked in `extra`.
///
/// Spelled by `GamepadPref::as_str`, never by hand, so the stored string cannot drift from the
/// name `from_name` parses on the other side.
fn shared_gamepad(t: GamepadType) -> String {
    use punktfunk_core::config::GamepadPref as Pref;
    match t {
        GamepadType::Auto => Pref::Auto,
        GamepadType::XboxOne => Pref::XboxOne,
        GamepadType::DualShock4 => Pref::DualShock4,
        GamepadType::DualSense => Pref::DualSense,
        GamepadType::DualSenseEdge => Pref::DualSenseEdge,
        GamepadType::SwitchPro => Pref::SwitchPro,
    }
    .as_str()
    .to_string()
}

/// The inverse. `None` for a kind this client has no row for (a Steam Deck's pad, say), so the
/// caller keeps its default rather than showing a control it cannot honour.
fn local_gamepad(name: &str) -> Option<GamepadType> {
    use punktfunk_core::config::GamepadPref as Pref;
    Some(match Pref::from_name(name)? {
        Pref::Auto => GamepadType::Auto,
        Pref::XboxOne => GamepadType::XboxOne,
        Pref::DualShock4 => GamepadType::DualShock4,
        Pref::DualSense => GamepadType::DualSense,
        Pref::DualSenseEdge => GamepadType::DualSenseEdge,
        Pref::SwitchPro => GamepadType::SwitchPro,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Named only by the fixtures below, so they would be unused imports in a normal build.
    use crate::core::model::{AudioRoutePref, LogLevelOverride, ThemeChoice};

    /// Every field survives the trip. This is the test that matters: the two UIs read the same
    /// file through this pair, so a field that does not round-trip is one the gamepad shell
    /// silently resets the moment it saves.
    #[test]
    fn round_trips_every_field() {
        let mut s = Settings {
            width: 2560,
            height: 1440,
            refresh_hz: 120,
            bitrate_kbps: 42_000,
            hdr_enabled: true,
            hdr_peak_nits: 812,
            hdr_frame_avg_nits: 311,
            hdr_black_code: 7,
            hdr_calibrated: true,
            codec: CodecPref::Hevc,
            stats_overlay: true,
            audio_channels: 6,
            log_level_override: LogLevelOverride::Warn,
            show_logs: true,
            gamepad_type: GamepadType::DualSenseEdge,
            pad_haptics: true,
            pad_speaker: true,
            cursor_capture: false,
            game_mode: true,
            audio_route: AudioRoutePref::NdlOpus,
            cursor_gestures: true,
            theme: ThemeChoice::Funk,
        };
        assert_eq!(from_shared(&to_shared(&s)), s, "non-default settings");

        s = Settings::default();
        assert_eq!(from_shared(&to_shared(&s)), s, "defaults");
    }

    /// A document another client wrote has none of our `webos.` keys; we must read its shared
    /// fields and default the rest rather than refuse it.
    #[test]
    fn reads_a_foreign_document() {
        let t = trust::Settings {
            width: 1920,
            height: 1080,
            refresh_hz: 60,
            bitrate_kbps: 20_000,
            codec: "h264".to_string(),
            ..trust::Settings::default()
        };
        let s = from_shared(&t);
        assert_eq!((s.width, s.height, s.refresh_hz), (1920, 1080, 60));
        assert_eq!(s.codec, CodecPref::H264);
        assert_eq!(s.theme, Settings::default().theme);
    }

    /// The discriminator that protects a real user's file. A document this client wrote before
    /// the move must be recognised as legacy, and one it writes after must not be.
    #[test]
    fn tells_the_two_shapes_apart() {
        let legacy = serde_json::to_value(Settings {
            theme: ThemeChoice::Funk,
            ..Settings::default()
        })
        .expect("encode");
        assert!(legacy_shape(&legacy), "this client's own shape is legacy");

        let moved = serde_json::to_value(to_shared(&Settings::default())).expect("encode");
        assert!(!legacy_shape(&moved), "the shared shape is not legacy");

        // A document from another client has neither the moved keys nor the prefix.
        let foreign = serde_json::to_value(trust::Settings::default()).expect("encode");
        assert!(!legacy_shape(&foreign), "a foreign document is not legacy");
    }

    /// A legacy object read through `Settings`, re-encoded shared, and read back must keep
    /// every value — that is the whole migration, and it runs once against a real file.
    #[test]
    fn migrates_a_legacy_document_without_loss() {
        let before = Settings {
            width: 3840,
            height: 2160,
            refresh_hz: 60,
            bitrate_kbps: 60_000,
            hdr_enabled: true,
            hdr_peak_nits: 700,
            hdr_calibrated: true,
            codec: CodecPref::Hevc,
            gamepad_type: GamepadType::SwitchPro,
            audio_route: AudioRoutePref::NdlOpus,
            theme: ThemeChoice::Funk,
            game_mode: true,
            ..Settings::default()
        };
        let on_disk = serde_json::to_value(before).expect("encode legacy");
        assert!(legacy_shape(&on_disk));

        let parsed: Settings = serde_json::from_value(on_disk).expect("read legacy");
        let after = from_shared(&to_shared(&parsed));
        assert_eq!(after, before);
    }

    /// `av1` is a codec this client cannot decode; it must not present as a selected option.
    #[test]
    fn unknown_codec_reads_as_auto() {
        let t = trust::Settings {
            codec: "av1".to_string(),
            ..trust::Settings::default()
        };
        assert_eq!(from_shared(&t).codec, CodecPref::Auto);
    }
}
