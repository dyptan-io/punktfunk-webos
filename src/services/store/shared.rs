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

use pf_client_core::profiles;

use crate::core::model::{CodecPref, GamepadType, Settings, SettingsOverride};

/// Prefix for rows only this client has. Namespaced so a future shared field of the same name
/// cannot collide with what a TV persisted.
const P: &str = "webos.";

fn key(name: &str) -> String {
    format!("{P}{name}")
}

fn get<T: serde::de::DeserializeOwned>(t: &trust::Settings, name: &str) -> Option<T> {
    get_raw(t, &key(name))
}

fn put<T: serde::Serialize>(t: &mut trust::Settings, name: &str, value: &T) {
    put_raw(t, key(name), value);
}

/// The same, under a key spelled exactly as given — for rows this client SHARES with the shell
/// rather than owns. [`P`] would hide such a row from the shell's own Settings screen, which
/// reads the unprefixed name, and the two UIs would then be editing two settings that merely
/// look alike.
fn get_raw<T: serde::de::DeserializeOwned>(t: &trust::Settings, key: &str) -> Option<T> {
    t.extra.get(key).cloned().and_then(|v| serde_json::from_value(v).ok())
}

fn put_raw<T: serde::Serialize>(t: &mut trust::Settings, key: String, value: &T) {
    if let Ok(v) = serde_json::to_value(value) {
        t.extra.insert(key, v);
    }
}

/// The console-vs-cursor pair, spelled as `pf_console_ui`'s own settings rows spell it.
const GAMEPAD_UI_KEY: &str = "gamepad_ui_enabled";
const GAMEPAD_UI_MODE_KEY: &str = "gamepad_ui_mode";

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

/// This client's settings as the shared document, written over `base`.
///
/// The scalar stream fields are mapped for real, not parked in `extra`: resolution, refresh,
/// bitrate, HDR on/off, audio channels and the two pad-audio lanes mean exactly the same thing
/// on every client, and sharing them is the point of the exercise.
///
/// 🛑 `base` is the document as it was last read (`Persisted::shared_base`), NOT a default. The
/// shared schema is wider than this client's `Settings`, so building it from `Settings` alone
/// silently resets every field only the other writers know about — the gamepad shell's palette
/// and library view among them. Everything not named below is carried through untouched.
pub fn to_shared(base: &trust::Settings, s: &Settings) -> trust::Settings {
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
        codec: shared_codec(s.codec).to_string(),
        mouse_mode: shared_mouse_mode(s.cursor_capture).to_string(),
        gamepad: shared_gamepad(s.gamepad_type),
        ..base.clone()
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
    put_raw(&mut t, GAMEPAD_UI_KEY.to_string(), &s.gamepad_ui);
    put_raw(&mut t, GAMEPAD_UI_MODE_KEY.to_string(), &s.gamepad_ui_mode);
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
        codec: local_codec(&t.codec),
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
        gamepad_ui: get_raw(t, GAMEPAD_UI_KEY).unwrap_or(d.gamepad_ui),
        gamepad_ui_mode: get_raw(t, GAMEPAD_UI_MODE_KEY).unwrap_or(d.gamepad_ui_mode),
    }
}

/// Enough of a host record for the shared shell to name and reach it.
///
/// Lives here rather than beside the shell's `SettingsStore`, which is arm-gated and therefore
/// never *runs* on a test runner — the armv7 build cannot execute on one. Real conversion logic
/// belongs where `task test` can execute it; only glue belongs behind that gate.
///
/// 🛑 `id` is pinned to `None` on purpose, overriding the base. `KnownHost::default()` MINTS a
/// fresh stable id — right for punktfunk, where a record is created once and keeps it, wrong
/// here: this converts on demand, so taking the default would hand the shell a DIFFERENT id for
/// the same host on every call, and "Copy link" would emit a `punktfunk://` link whose record id
/// matches nothing. This client keys hosts by `addr:port` and mints no ids, so `None` is the only
/// truthful answer; Copy link degrades to "isn't saved any more" instead of lying.
pub fn to_shared_host(h: &crate::core::model::KnownHost) -> trust::KnownHost {
    trust::KnownHost {
        name: h.name.clone(),
        addr: h.host.clone(),
        port: h.port,
        fp_hex: h.fingerprint.map(hex).unwrap_or_default(),
        paired: h.fingerprint.is_some(),
        mac: h.mac.clone(),
        os: h.os.clone(),
        mgmt_port: h.mgmt_port,
        id: None,
        ..trust::KnownHost::default()
    }
}

/// The shell's stable row key for a host: its pinned fingerprint, else `addr:port`
/// (`pf_console_ui::HostRow::key`, the desktop's rule — the two must agree or a link copied
/// on one client names nothing on the other).
///
/// Every host-scoped command the shell raises (Forget, Wake, Edit, the clipboard toggle)
/// carries this string and nothing else, so [`find_known`] has to invert it. That is the
/// whole reason both live here, ungated, where `task test` can actually run them.
pub fn host_key(fp_hex: &str, addr: &str, port: u16) -> String {
    if fp_hex.is_empty() {
        format!("{addr}:{port}")
    } else {
        fp_hex.to_string()
    }
}

/// [`host_key`] for a record this client holds.
pub fn known_host_key(h: &crate::core::model::KnownHost) -> String {
    host_key(&h.fingerprint.map(hex).unwrap_or_default(), &h.host, h.port)
}

/// The record a shell row key addresses, or `None` if it names no host this client knows.
///
/// A pinned-profile card's key is `<host key>\0<profile id>`. This client mints none (it has
/// no profile catalog — see `store::console::ConsoleStore::profiles`), but the key is the
/// shell's shape, not ours, so the suffix is trimmed rather than trusted to be absent.
pub fn find_known(hosts: &[crate::core::model::KnownHost], key: &str) -> Option<usize> {
    let key = key.split('\0').next().unwrap_or(key);
    hosts.iter().position(|h| known_host_key(h) == key)
}

/// The store a library id belongs to — the `steam` of `steam:570`.
///
/// `GameEntry::id` is store-qualified by contract (see its doc); the shell prints this as the
/// card's store line. An id with no prefix yields the whole id rather than an empty string,
/// which reads as "unknown store" instead of a blank.
pub fn store_of(id: &str) -> &str {
    id.split_once(':').map_or(id, |(store, _)| store)
}

/// A pinned fingerprint as the lowercase hex the shell's rows and links carry.
pub fn hex(bytes: [u8; 32]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The inverse: a fingerprint the shell handed back, or `None` for anything that is not
/// exactly 32 bytes of hex.
///
/// Strict on purpose. This is the pin a session is verified against, so a short, odd-length or
/// non-hex string has to fail rather than silently produce a shorter key that would either be
/// rejected on the wire or — worse — compared against the wrong thing.
pub fn parse_fp(fp_hex: &str) -> Option<[u8; 32]> {
    if fp_hex.len() != 64 {
        return None;
    }
    let bytes: Vec<u8> = (0..64)
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(fp_hex.get(i..i + 2)?, 16).ok())
        .collect();
    bytes.try_into().ok()
}

/// The shared pad name. Lossless in both directions — `GamepadPref` carries every kind this
/// client offers, Edge and Switch Pro included — so the pad choice is a genuinely shared field
/// rather than a TV-only one parked in `extra`.
///
/// Spelled by `GamepadPref::as_str`, never by hand, so the stored string cannot drift from the
/// name `from_name` parses on the other side.
fn shared_gamepad(t: GamepadType) -> String {
    gamepad_pref(t).as_str().to_string()
}

/// One game's overrides, read through its binding — the empty set when it has none, or when
/// the id it names is no longer in the catalog. A dangling binding is never an error here, the
/// same rule punktfunk's own resolver follows.
pub fn game_overrides(catalog: &[profiles::StreamProfile], bound: Option<&str>) -> SettingsOverride {
    bound
        .and_then(|id| catalog.iter().find(|p| p.id == id))
        .map_or_else(SettingsOverride::default, |p| override_from_overlay(&p.overrides))
}

/// Writes `over` back as `game`'s profile, and returns the id it is bound to now.
///
/// Creating on first edit is what gives a TV a catalog at all: there is no desktop app beside
/// it to make one, so the act of giving a game its own settings is the act of making a profile.
/// An edit that empties the override deletes the profile and returns `None` — a named entry
/// that overrides nothing would be a card the user never asked for.
pub fn bind_game_overrides(
    catalog: &mut Vec<profiles::StreamProfile>,
    bound: Option<&str>,
    title: &str,
    over: &SettingsOverride,
) -> Option<String> {
    let existing = bound.and_then(|id| catalog.iter().position(|p| p.id == id));
    if over.is_empty() {
        if let Some(i) = existing {
            catalog.remove(i);
        }
        return None;
    }
    let overlay = overlay_from_override(over);
    if let Some(i) = existing {
        catalog[i].overrides = overlay;
        return Some(catalog[i].id.clone());
    }
    let mut profile = profiles::StreamProfile::new(unique_name(catalog, title));
    profile.overrides = overlay;
    let id = profile.id.clone();
    catalog.push(profile);
    Some(id)
}

/// The game's own name, or that name with a counter — two hosts can list the same title, and
/// the catalog is one flat list whose names are what the user picks from.
fn unique_name(catalog: &[profiles::StreamProfile], title: &str) -> String {
    let taken = |name: &str| catalog.iter().any(|p| p.name == name);
    if !taken(title) {
        return title.to_string();
    }
    (2..)
        .map(|n| format!("{title} ({n})"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| title.to_string())
}

/// One game's overrides as a profile overlay, and back.
///
/// Same table as [`to_shared`]/[`from_shared`], for the same reason: a per-game override and a
/// global setting mean the same thing to a host, so they must reach the wire through one
/// mapping. The two rows punktfunk has no field for ride `extra` under [`P`], exactly as the
/// global document's do.
///
/// `mode` is the one pair that is not one-to-one: this client picks resolution as a unit, so a
/// half-set overlay (width without height, from another client) reads as no override rather
/// than a resolution this UI cannot show.
pub fn overlay_from_override(o: &SettingsOverride) -> profiles::SettingsOverlay {
    let mut overlay = profiles::SettingsOverlay {
        width: o.mode.map(|(w, _)| w),
        height: o.mode.map(|(_, h)| h),
        refresh_hz: o.refresh_hz,
        bitrate_kbps: o.bitrate_kbps,
        hdr_enabled: o.hdr_enabled,
        codec: o.codec.map(|c| shared_codec(c).to_string()),
        audio_channels: o.audio_channels,
        gamepad: o.gamepad_type.map(shared_gamepad),
        mouse_mode: o.cursor_capture.map(|c| shared_mouse_mode(c).to_string()),
        ..profiles::SettingsOverlay::default()
    };
    if let Some(on) = o.cursor_gestures {
        overlay
            .extra
            .insert(key("cursor_gestures"), serde_json::Value::Bool(on));
    }
    overlay
}

/// The inverse. Anything this client has no row for stays in the overlay untouched — it is
/// read here, never rewritten from this shape.
pub fn override_from_overlay(o: &profiles::SettingsOverlay) -> SettingsOverride {
    SettingsOverride {
        mode: o.width.zip(o.height),
        refresh_hz: o.refresh_hz,
        bitrate_kbps: o.bitrate_kbps,
        hdr_enabled: o.hdr_enabled,
        codec: o.codec.as_deref().map(local_codec),
        audio_channels: o.audio_channels,
        gamepad_type: o.gamepad.as_deref().and_then(local_gamepad),
        cursor_capture: o.mouse_mode.as_deref().map(|m| m != "desktop"),
        cursor_gestures: o
            .extra
            .get(&key("cursor_gestures"))
            .and_then(serde_json::Value::as_bool),
    }
}

/// This client's codec pick as punktfunk's wire name, and back — the same two arms
/// [`to_shared`] and [`from_shared`] use, named so the override mapping cannot spell them
/// differently.
fn shared_codec(c: CodecPref) -> &'static str {
    match c {
        CodecPref::Auto => "auto",
        CodecPref::H264 => "h264",
        CodecPref::Hevc => "hevc",
    }
}

fn local_codec(name: &str) -> CodecPref {
    match name {
        "h264" => CodecPref::H264,
        "hevc" => CodecPref::Hevc,
        // "av1" too: this client cannot decode it, so it reads as the host's choice.
        _ => CodecPref::Auto,
    }
}

/// Capture is a switch here and a two-name mode in the shared schema.
fn shared_mouse_mode(capture: bool) -> &'static str {
    if capture {
        "capture"
    } else {
        "desktop"
    }
}

/// This client's pad choice as punktfunk's. Also what the console shell's button-glyph legend
/// is picked by, which is why it is a value rather than only ever a string.
pub fn gamepad_pref(t: GamepadType) -> punktfunk_core::config::GamepadPref {
    use punktfunk_core::config::GamepadPref as Pref;
    match t {
        GamepadType::Auto => Pref::Auto,
        GamepadType::XboxOne => Pref::XboxOne,
        GamepadType::DualShock4 => Pref::DualShock4,
        GamepadType::DualSense => Pref::DualSense,
        GamepadType::DualSenseEdge => Pref::DualSenseEdge,
        GamepadType::SwitchPro => Pref::SwitchPro,
    }
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
    use crate::core::model::{AudioRoutePref, GamepadUiMode, LogLevelOverride, SettingsOverride, ThemeChoice};

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
            gamepad_ui: false,
            gamepad_ui_mode: GamepadUiMode::Always,
        };
        assert_eq!(
            from_shared(&to_shared(&trust::Settings::default(), &s)),
            s,
            "non-default settings"
        );

        s = Settings::default();
        assert_eq!(from_shared(&to_shared(&trust::Settings::default(), &s)), s, "defaults");
    }

    /// The shared schema is wider than this client's `Settings`, and everything outside it has
    /// to survive a save from this side. Without the carried base the gamepad shell's own rows
    /// — its palette, its library view — would reset the moment either UI wrote the file, which
    /// reads as a setting that will not stick rather than as data loss.
    #[test]
    fn a_save_keeps_the_fields_this_client_does_not_model() {
        let stored = trust::Settings {
            ui_palette: "mint".to_string(),
            library_view: "grid".to_string(),
            render_scale: 1.5,
            mic_enabled: true,
            extra: [("android.low_latency".to_string(), serde_json::Value::Bool(true))]
                .into_iter()
                .collect(),
            ..trust::Settings::default()
        };
        // What a load does: read it into this client's shape, keeping the document as the base.
        let mine = from_shared(&stored);
        // …and what a save does, after this client changed something of its own.
        let written = to_shared(
            &stored,
            &Settings {
                bitrate_kbps: 42_000,
                ..mine
            },
        );

        assert_eq!(written.bitrate_kbps, 42_000, "this client's own edit lands");
        assert_eq!(written.ui_palette, "mint", "the shell's palette survives");
        assert_eq!(written.library_view, "grid", "so does its library view");
        assert!((written.render_scale - 1.5).abs() < f64::EPSILON);
        assert!(written.mic_enabled);
        assert_eq!(
            written.extra.get("android.low_latency"),
            Some(&serde_json::Value::Bool(true)),
            "another client's namespaced rows ride through untouched"
        );
        // And this client's own prefixed rows are still written alongside them.
        assert!(written.extra.contains_key("webos.theme"));
    }

    /// The console-vs-cursor pair is the one thing this client must NOT namespace: the shared
    /// shell's own Settings rows read these exact key names, so a `webos.` prefix would leave
    /// the two UIs editing settings that merely look alike — the switch would appear to do
    /// nothing from whichever side you were not on.
    #[test]
    fn the_controller_ui_pair_is_stored_under_the_shells_own_keys() {
        let s = Settings {
            gamepad_ui: false,
            gamepad_ui_mode: GamepadUiMode::Always,
            ..Settings::default()
        };
        let written = to_shared(&trust::Settings::default(), &s);
        assert_eq!(
            written.extra.get("gamepad_ui_enabled"),
            Some(&serde_json::Value::Bool(false)),
        );
        assert_eq!(
            written.extra.get("gamepad_ui_mode"),
            Some(&serde_json::Value::String("always".to_string())),
            "the mode's stored spelling is the shell's, not this enum's debug name"
        );
        assert!(!written.extra.contains_key("webos.gamepad_ui_enabled"));
        // A value the shell wrote reads straight back — the same file, one setting.
        let mut shell_wrote = trust::Settings::default();
        shell_wrote
            .extra
            .insert("gamepad_ui_enabled".to_string(), serde_json::Value::Bool(true));
        shell_wrote.extra.insert(
            "gamepad_ui_mode".to_string(),
            serde_json::Value::String("connected".to_string()),
        );
        let mine = from_shared(&shell_wrote);
        assert!(mine.gamepad_ui);
        assert_eq!(mine.gamepad_ui_mode, GamepadUiMode::Connected);
    }

    /// Every override field survives the trip through a profile overlay. The two rows
    /// punktfunk has no field for ride `extra`, so "it round-trips" is the only thing that
    /// proves they were not quietly dropped on the way.
    #[test]
    fn an_override_round_trips_through_a_profile_overlay() {
        let over = SettingsOverride {
            mode: Some((2560, 1440)),
            refresh_hz: Some(120),
            bitrate_kbps: Some(60_000),
            hdr_enabled: Some(true),
            codec: Some(CodecPref::Hevc),
            audio_channels: Some(6),
            gamepad_type: Some(GamepadType::DualSenseEdge),
            cursor_capture: Some(false),
            cursor_gestures: Some(true),
        };
        assert_eq!(override_from_overlay(&overlay_from_override(&over)), over);

        // An empty one stays empty rather than becoming a set of defaults — that is the
        // difference between "inherits the globals" and "pinned to whatever they were".
        let empty = SettingsOverride::default();
        assert!(overlay_from_override(&empty).is_empty());
        assert_eq!(override_from_overlay(&overlay_from_override(&empty)), empty);

        // Resolution is picked as a unit here, so a half-set overlay reads as no override.
        let half = profiles::SettingsOverlay {
            width: Some(1920),
            ..profiles::SettingsOverlay::default()
        };
        assert_eq!(override_from_overlay(&half).mode, None);
    }

    /// The catalog write-back: first edit creates, later edits update in place, and emptying
    /// deletes. A TV has no desktop app beside it, so this is the only thing that ever makes
    /// a profile there — if it did not create, the shell's profile list would stay empty.
    #[test]
    fn a_games_first_override_creates_its_profile_and_emptying_it_deletes_it() {
        let mut catalog = Vec::new();
        let over = SettingsOverride {
            refresh_hz: Some(60),
            ..SettingsOverride::default()
        };
        let id = bind_game_overrides(&mut catalog, None, "Hollow Knight", &over).expect("a profile");
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "Hollow Knight");
        assert_eq!(game_overrides(&catalog, Some(&id)), over);

        // A second edit rewrites that profile rather than making another.
        let wider = SettingsOverride {
            bitrate_kbps: Some(20_000),
            ..over
        };
        let same = bind_game_overrides(&mut catalog, Some(&id), "Hollow Knight", &wider);
        assert_eq!(same.as_deref(), Some(id.as_str()));
        assert_eq!(catalog.len(), 1);
        assert_eq!(game_overrides(&catalog, Some(&id)), wider);

        // Clearing every row takes the profile with it: a named entry overriding nothing is a
        // card the user never asked for.
        assert_eq!(
            bind_game_overrides(&mut catalog, Some(&id), "Hollow Knight", &SettingsOverride::default()),
            None
        );
        assert!(catalog.is_empty());
        // And a binding left pointing at it reads as no override rather than an error.
        assert_eq!(game_overrides(&catalog, Some(&id)), SettingsOverride::default());
    }

    /// Two hosts can list the same title, and the catalog is one flat list of names.
    #[test]
    fn a_second_game_of_the_same_name_gets_its_own_entry() {
        let mut catalog = Vec::new();
        let over = SettingsOverride {
            refresh_hz: Some(60),
            ..SettingsOverride::default()
        };
        bind_game_overrides(&mut catalog, None, "Doom", &over);
        bind_game_overrides(&mut catalog, None, "Doom", &over);
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[1].name, "Doom (2)");
        assert_ne!(catalog[0].id, catalog[1].id);
    }

    /// The rule the flip runs on, both directions. Mirrors Android's `gamepadUiActive` minus
    /// its `tv` term — every webOS set is a TV, so that term would make the mode meaningless.
    #[test]
    fn the_controller_ui_applies_only_when_the_switch_and_the_mode_agree() {
        let connected = Settings {
            gamepad_ui: true,
            gamepad_ui_mode: GamepadUiMode::Connected,
            ..Settings::default()
        };
        assert!(connected.gamepad_ui_active(true), "a pad is what that mode waits for");
        assert!(
            !connected.gamepad_ui_active(false),
            "and with none, the cursor menus keep the screen"
        );

        let always = Settings {
            gamepad_ui_mode: GamepadUiMode::Always,
            ..connected
        };
        assert!(always.gamepad_ui_active(false), "no pad needed under Always");

        // The switch is the outer gate: off means off under either mode.
        for mode in GamepadUiMode::ALL {
            let off = Settings {
                gamepad_ui: false,
                gamepad_ui_mode: mode,
                ..Settings::default()
            };
            assert!(!off.gamepad_ui_active(true), "{mode:?} must not survive the switch");
        }
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

        let moved = serde_json::to_value(to_shared(&trust::Settings::default(), &Settings::default())).expect("encode");
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
        let after = from_shared(&to_shared(&trust::Settings::default(), &parsed));
        assert_eq!(after, before);
    }

    /// Host records reach the shell named and reachable, with `paired` following the fingerprint
    /// and the hex being the full 32 bytes.
    #[test]
    fn hosts_convert_for_the_shell() {
        let paired = crate::core::model::KnownHost {
            name: "desk".into(),
            host: "192.168.1.5".into(),
            port: 47_989,
            fingerprint: Some([0xab; 32]),
            mgmt_port: Some(47_990),
            ..Default::default()
        };
        let h = to_shared_host(&paired);
        assert_eq!(
            (h.name.as_str(), h.addr.as_str(), h.port),
            ("desk", "192.168.1.5", 47_989)
        );
        assert_eq!(h.mgmt_port, Some(47_990));
        assert!(h.paired);
        assert_eq!(h.fp_hex, "ab".repeat(32), "32 bytes is 64 hex chars");
        // Not merely absent — STABLE. `KnownHost::default()` mints a fresh id, so a conversion
        // that took the default would differ on every call for the same host.
        assert!(h.id.is_none(), "no minted record id to report");
        assert_eq!(to_shared_host(&paired).id, h.id, "same host, same answer");

        let unpaired = crate::core::model::KnownHost {
            fingerprint: None,
            ..paired
        };
        let h = to_shared_host(&unpaired);
        assert!(!h.paired, "no fingerprint means not paired");
        assert!(h.fp_hex.is_empty());
    }

    /// The shell addresses hosts by row key alone, so every key this client mints must lead
    /// back to the record it came from — a miss is a Forget or a Wake that silently does
    /// nothing. Both spellings, and the pinned-card suffix the shell may append.
    #[test]
    fn row_keys_invert_to_their_host() {
        let paired = crate::core::model::KnownHost {
            name: "desk".into(),
            host: "192.168.1.5".into(),
            port: 47_989,
            fingerprint: Some([0xab; 32]),
            ..Default::default()
        };
        let unpaired = crate::core::model::KnownHost {
            name: "typed".into(),
            host: "10.0.0.9".into(),
            port: 47_989,
            fingerprint: None,
            ..Default::default()
        };
        let hosts = vec![paired.clone(), unpaired.clone()];

        assert_eq!(known_host_key(&paired), "ab".repeat(32), "paired keys on its pin");
        assert_eq!(
            known_host_key(&unpaired),
            "10.0.0.9:47989",
            "unpaired keys on addr:port"
        );
        assert_eq!(find_known(&hosts, &known_host_key(&paired)), Some(0));
        assert_eq!(find_known(&hosts, &known_host_key(&unpaired)), Some(1));
        // A pinned profile card rides its host's key behind a NUL.
        let pinned = format!("{}\0work", known_host_key(&paired));
        assert_eq!(find_known(&hosts, &pinned), Some(0), "the card resolves to its host");
        assert_eq!(find_known(&hosts, "nothing"), None);
    }

    /// The pin the shell hands back has to survive the round trip exactly, and anything that
    /// is not a whole fingerprint has to be refused rather than truncated — this is the value
    /// a session is verified against.
    #[test]
    fn fingerprints_round_trip_and_refuse_junk() {
        let fp = [0xab; 32];
        assert_eq!(parse_fp(&hex(fp)), Some(fp));
        assert_eq!(parse_fp(""), None, "an unpaired row carries no pin");
        assert_eq!(parse_fp(&"ab".repeat(31)), None, "62 hex chars is not a fingerprint");
        assert_eq!(parse_fp(&"zz".repeat(32)), None, "not hex at all");
    }

    /// The card's store line comes off the id, and an id without a prefix must not read blank.
    #[test]
    fn store_comes_off_the_id() {
        assert_eq!(store_of("steam:570"), "steam");
        assert_eq!(store_of("custom:my-thing"), "custom");
        assert_eq!(store_of("bare"), "bare");
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
