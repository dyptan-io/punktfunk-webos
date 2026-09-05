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

use crate::core::model::{Persisted, DESKTOP_PIN_ID};
use crate::core::settings::TvSettings;
use pf_client_core::trust::Settings;

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
        profile_id: h.profile_id.clone(),
        pinned_profiles: h.pinned_profiles.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

/// The settings one launch runs with: the global document with the resolved profile applied
/// — one-off ?? per-title ?? host default, the shared resolver's own precedence — then the
/// Desktop card's standing rule that pointer capture is off there (unless the profile sets
/// the mouse mode), then this set's caps. The single merge point both menu loops call.
pub fn launch_settings(
    state: &Persisted,
    addr: &str,
    port: u16,
    launch: Option<&str>,
    one_off: Option<&str>,
) -> Settings {
    let id = launch.unwrap_or(DESKTOP_PIN_ID);
    let host = state.known_hosts.iter().find(|h| h.host == addr && h.port == port);
    let per_title = host.and_then(|h| h.game_profile(id));
    let bound = host.and_then(|h| h.profile_id.as_deref());
    let catalog = profiles::ProfilesFile {
        version: profiles::PROFILES_VERSION,
        profiles: state.profiles.clone(),
    };
    let profile = trust::resolve_profile(&catalog, bound, per_title, one_off);
    let global = &state.settings;
    let mut settings = profile
        .as_ref()
        .map_or_else(|| global.clone(), |p| p.overrides.apply(global));
    if id == DESKTOP_PIN_ID && profile.as_ref().is_none_or(|p| p.overrides.mouse_mode.is_none()) {
        settings.set_cursor_capture(false);
    }
    settings.clamp_to_caps();
    settings
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use pf_client_core::profiles::StreamProfile;

    fn state_with(profile: StreamProfile, bind: impl FnOnce(&mut crate::core::model::KnownHost)) -> Persisted {
        let mut host = crate::core::model::KnownHost {
            host: "10.0.0.2".into(),
            port: 47989,
            ..Default::default()
        };
        bind(&mut host);
        let mut state = Persisted::default();
        state.known_hosts.push(host);
        state.profiles.push(profile);
        state
    }

    /// A title binding beats the host's default; a dangling id falls through to it; the
    /// desktop card streams with capture off unless the profile pins the mouse mode.
    #[test]
    fn launch_follows_the_shared_precedence_and_the_desktop_rule() {
        let mut title_profile = StreamProfile::new("Doom");
        title_profile.overrides.bitrate_kbps = Some(12_000);
        let tid = title_profile.id.clone();
        let mut host_profile = StreamProfile::new("Work");
        host_profile.overrides.bitrate_kbps = Some(34_000);
        let hid = host_profile.id.clone();
        let mut state = state_with(title_profile, |h| {
            h.bind_game_profile("doom", Some(tid.clone()));
        });
        state.profiles.push(host_profile);
        state.known_hosts[0].profile_id = Some(hid);
        state.settings.set_cursor_capture(true);

        let doom = launch_settings(&state, "10.0.0.2", 47989, Some("doom"), None);
        assert_eq!(doom.bitrate_kbps, 12_000);
        let other = launch_settings(&state, "10.0.0.2", 47989, Some("quake"), None);
        assert_eq!(other.bitrate_kbps, 34_000, "the host default covers an unbound title");
        let desktop = launch_settings(&state, "10.0.0.2", 47989, None, None);
        assert!(!desktop.cursor_capture(), "the desktop card streams with capture off");
        assert!(other.cursor_capture(), "a game keeps the global capture");

        state.known_hosts[0].bind_game_profile("doom", Some("gone".into()));
        let dangling = launch_settings(&state, "10.0.0.2", 47989, Some("doom"), None);
        assert_eq!(
            dangling.bitrate_kbps, 34_000,
            "a dangling title binding falls back to the host default"
        );
    }
}
