//! Plain domain data. No I/O — persistence lives in `crate::services`.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Stream connection target.
pub struct ConnectTarget {
    pub host: String,
    pub port: u16,
    /// The pinned host fingerprint. Always present: the pin *is* the pair state, and
    /// every launch path bails on an unpaired host before building one of these.
    pub fingerprint: [u8; 32],
    /// Library entry id to launch, or `None` for desktop.
    pub launch: Option<String>,
}

/// `Default` is both how the literals that build one spread over the fields they don't care about
/// and how a record missing a field loads (`serde(default)` on the container) — so a field added
/// here needs no migration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownHost {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// The host's pinned leaf certificate SHA-256; `None` = discovered but never paired.
    pub fingerprint: Option<[u8; 32]>,
    /// Management API port (game library); defaults to `library::DEFAULT_MGMT_PORT`.
    pub mgmt_port: Option<u16>,
    /// Wake-on-LAN MACs learned from mDNS; empty if never advertised.
    pub mac: Vec<String>,
    /// Auto-wake on unreachable (per-host, off by default; lives in Wake settings).
    pub wol_auto: bool,
    /// Per-game state for this host, keyed by `GameEntry::id` (or [`DESKTOP_PIN_ID`]) — pins
    /// and settings overrides together, so one prune drops both when a game leaves the library.
    /// A `BTreeMap` so the file's key order is stable and diffable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub games: BTreeMap<String, GamePrefs>,
}

/// What one game carries on one host. Absent from `KnownHost::games` entirely when it holds
/// nothing — an untouched game costs no bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamePrefs {
    /// Pin slot — the ordering key of the pinned block. `None` = not pinned. Values are
    /// monotonic per host, not dense: unpinning leaves a hole rather than renumbering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<u32>,
    /// Settings this game overrides; every unset field falls through to the global [`Settings`].
    #[serde(skip_serializing_if = "SettingsOverride::is_empty")]
    pub over: SettingsOverride,
}

impl GamePrefs {
    /// Nothing worth persisting — the prune drops these.
    fn is_empty(&self) -> bool {
        self.pin.is_none() && self.over.is_empty()
    }
}

/// A sparse [`Settings`] diff: `Some` overrides the global value for one game, `None` inherits it.
///
/// Deliberately *not* every field. The experimental and diagnostics toggles are device-wide
/// and stay global-only, as does `video_backend` — it is a process-global
/// (`core::caps::set_backend`, which both `session::connect`'s clamp and the caps-derived row
/// locks read), so a per-game value would need an apply/restore around every launch.
///
/// `Copy + Hash + Eq` because it rides the render cache keys next to `Settings`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsOverride {
    /// Width and height move together — they are one Resolution row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_hz: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<CodecPref>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamepad_type: Option<GamepadType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_capture: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_gestures: Option<bool>,
}

impl SettingsOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// `base` with every set field applied. The result still needs
    /// [`Settings::clamp_to_caps`] before it reaches the wire, exactly like a global value.
    /// Clears every field that already equals `global` — a value the user has just set back
    /// to what the global screen says is not an override, and must not linger in the
    /// document. Run after every edit, so the record stays minimal (and disappears
    /// entirely, via [`KnownHost::edit_overrides`], once nothing differs).
    pub fn drop_matching(&mut self, global: &Settings) {
        // Field by field against the global, not against the merge: a field is an override
        // exactly when it is set *and* differs, and an unset one has nothing to drop.
        macro_rules! drop_if_global {
            ($($field:ident => $global:expr),* $(,)?) => {
                $(if self.$field == Some($global) {
                    self.$field = None;
                })*
            };
        }
        drop_if_global! {
            mode => (global.width, global.height),
            refresh_hz => global.refresh_hz,
            bitrate_kbps => global.bitrate_kbps,
            hdr_enabled => global.hdr_enabled,
            codec => global.codec,
            audio_channels => global.audio_channels,
            gamepad_type => global.gamepad_type,
            cursor_capture => global.cursor_capture,
            cursor_gestures => global.cursor_gestures,
        }
    }

    #[must_use]
    pub fn merge_into(&self, mut base: Settings) -> Settings {
        if let Some((w, h)) = self.mode {
            base.width = w;
            base.height = h;
        }
        if let Some(v) = self.refresh_hz {
            base.refresh_hz = v;
        }
        if let Some(v) = self.bitrate_kbps {
            base.bitrate_kbps = v;
        }
        if let Some(v) = self.hdr_enabled {
            base.hdr_enabled = v;
        }
        if let Some(v) = self.codec {
            base.codec = v;
        }
        if let Some(v) = self.audio_channels {
            base.audio_channels = v;
        }
        if let Some(v) = self.gamepad_type {
            base.gamepad_type = v;
        }
        if let Some(v) = self.cursor_capture {
            base.cursor_capture = v;
        }
        if let Some(v) = self.cursor_gestures {
            base.cursor_gestures = v;
        }
        base
    }
}

/// Max games pinned to one host's always-visible grid row at once.
pub const MAX_PINNED_GAMES: usize = 5;

/// Pin ID for the "Desktop" card — a `games` key like any other, and it counts toward
/// `MAX_PINNED_GAMES`. Never pruned, since no library listing contains it.
pub const DESKTOP_PIN_ID: &str = "__desktop__";

impl KnownHost {
    pub fn is_paired(&self) -> bool {
        self.fingerprint.is_some()
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.games.get(id).is_some_and(|g| g.pin.is_some())
    }

    /// Pinned ids, in pin order.
    pub fn pinned_ids(&self) -> Vec<&str> {
        let mut pinned: Vec<(u32, &str)> = self
            .games
            .iter()
            .filter_map(|(id, g)| g.pin.map(|p| (p, id.as_str())))
            .collect();
        pinned.sort_unstable();
        pinned.into_iter().map(|(_, id)| id).collect()
    }

    pub fn pinned_count(&self) -> usize {
        self.games.values().filter(|g| g.pin.is_some()).count()
    }

    /// Whether toggling id would do anything (unpin always ok, pin only if under `MAX_PINNED_GAMES`).
    pub fn can_toggle_pin(&self, id: &str) -> bool {
        self.is_pinned(id) || self.pinned_count() < MAX_PINNED_GAMES
    }

    /// Toggles `id`'s pinned state (a `GameEntry::id`, or `DESKTOP_PIN_ID`) —
    /// a no-op when `can_toggle_pin` is false.
    pub fn toggle_pin(&mut self, id: &str) {
        if self.is_pinned(id) {
            if let Some(g) = self.games.get_mut(id) {
                g.pin = None;
            }
            self.drop_if_empty(id);
        } else if self.can_toggle_pin(id) {
            // Monotonic, never renumbered: a re-pin lands at the end of the block, which is
            // where someone who just pinned it expects to find it.
            let next = self.games.values().filter_map(|g| g.pin).max().map_or(0, |m| m + 1);
            self.games.entry(id.to_string()).or_default().pin = Some(next);
        }
    }

    /// This game's overrides, or the empty set — reading never creates an entry.
    pub fn overrides(&self, id: &str) -> SettingsOverride {
        self.games.get(id).map_or_else(SettingsOverride::default, |g| g.over)
    }

    /// Runs `edit` against this game's overrides, creating the entry only if needed and
    /// dropping it again when the edit leaves nothing behind.
    pub fn edit_overrides(&mut self, id: &str, edit: impl FnOnce(&mut SettingsOverride)) {
        edit(&mut self.games.entry(id.to_string()).or_default().over);
        self.drop_if_empty(id);
    }

    fn drop_if_empty(&mut self, id: &str) {
        if self.games.get(id).is_some_and(GamePrefs::is_empty) {
            self.games.remove(id);
        }
    }

    /// Drops per-game state for ids the host no longer lists. `live` must come from a
    /// *successful* library fetch — an error or an offline host would otherwise wipe
    /// everything. [`DESKTOP_PIN_ID`] is always kept: it is never in a library listing.
    /// Returns whether anything was removed.
    pub fn prune_games(&mut self, live: impl Fn(&str) -> bool) -> bool {
        let before = self.games.len();
        self.games.retain(|id, _| id == DESKTOP_PIN_ID || live(id));
        self.games.len() != before
    }
}

/// A fresh host's per-game map: just `id` pinned. What every add/pair flow seeds
/// [`KnownHost::games`] with, so the Desktop card starts in the pinned block.
pub fn pinned_only(id: &str) -> BTreeMap<String, GamePrefs> {
    BTreeMap::from([(
        id.to_string(),
        GamePrefs {
            pin: Some(0),
            ..GamePrefs::default()
        },
    )])
}

/// The cursor-capture override the Desktop card carries: *off*, since capture is on globally
/// for the games that are the common case and the desktop is the one card where the host's own
/// pointer should stay visible. Doubles as the shipped demo of per-game overrides.
///
/// `None` when the global value is already off — an override equal to the global reads as noise
/// (matching [`SettingsOverride::drop_matching`]). The one place this default lives: new hosts
/// get it from [`new_host_games`], existing ones from `store`'s version bootstrap.
pub fn desktop_capture_override(global: &Settings) -> Option<bool> {
    global.cursor_capture.then_some(false)
}

/// The `games` map a genuinely new host starts with: Desktop pinned, wearing
/// [`desktop_capture_override`].
pub fn new_host_games(global: &Settings) -> BTreeMap<String, GamePrefs> {
    let mut games = pinned_only(DESKTOP_PIN_ID);
    if let Some(prefs) = games.get_mut(DESKTOP_PIN_ID) {
        prefs.over.cursor_capture = desktop_capture_override(global);
    }
    games
}

/// Upserts by `(host, port)`, keeping the existing fingerprint if the new record is unpaired
/// (a fresh mDNS discovery shouldn't clobber a paired host) — same reasoning for `mac`,
/// learned separately (see `App::drain_discovery`) and not necessarily known again at the
/// point something else re-upserts this host. `games` and `wol_auto` are *always* kept from the
/// existing record: only [`KnownHost::toggle_pin`], the per-game settings screen and the Wake
/// screen change them, so no add/edit/re-pair flow may clobber any of it.
pub fn upsert_known_host(hosts: &mut Vec<KnownHost>, mut new: KnownHost) {
    let Some(existing) = hosts.iter_mut().find(|h| h.host == new.host && h.port == new.port) else {
        hosts.push(new);
        return;
    };
    if !new.is_paired() {
        new.fingerprint = existing.fingerprint;
    }
    if new.mac.is_empty() {
        new.mac.clone_from(&existing.mac);
    }
    new.games.clone_from(&existing.games);
    new.wol_auto = existing.wol_auto;
    *existing = new;
}

/// Video decode backend, selectable in Settings on webOS 3.5-4.x only — see
/// [`crate::core::caps`]. On webOS 5+ NDL v2 is the only path and the row is hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoBackend {
    /// NDL `DirectMedia` — v2 on webOS 5+, v1 on 3.5-4.x (H.264/SDR there).
    #[default]
    Ndl,
    /// SMP (`libplayerAPIs_C.so`): HEVC and HDR on a TV whose NDL generation has
    /// neither, plus `pauseAtDecodeTime` pacing. Falls back to NDL if the load fails.
    Smp,
}

/// Codec preference selectable in Settings — a *preference*, not a demand. The host
/// resolves the session codec from the client's advertised set via its own precedence
/// ladder (HEVC > H.264), honouring the preference only when its encoder can
/// actually produce it — so "H264" on a host that can't encode H.264 still gets HEVC
/// rather than no session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecPref {
    /// No preference — the host's precedence ladder decides (HEVC in practice).
    #[default]
    Auto,
    H264,
    Hevc,
}

/// Which controller the host should present to the game, selectable in Settings.
///
/// This is the *virtual* pad the host builds, not what the user is holding — the host
/// translates. It matters beyond glyphs: a game only emits adaptive-trigger effects when it
/// sees a `DualSense`, so [`GamepadType::DualSense`] is what makes `crate::platform::webos::dualsense` have
/// anything to replay. The host resolves the choice against what its platform can actually
/// build (the `PlayStation` and Switch backends need Linux UHID) and falls back on its own if
/// not, so an unbuildable pick degrades to a working session rather than none.
///
/// A deliberate subset of `punktfunk_core::config::GamepadPref`'s eleven variants: the Steam
/// Controller/Deck backends exist for clients running *on* that hardware, which a TV is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GamepadType {
    /// Mirror whichever controller is attached, falling back to the host's own choice (an
    /// Xbox pad) when the pad isn't one this client recognizes — see
    /// `gamepad::detect_type`. Resolved per session, so the stored preference stays "match my
    /// pad" rather than freezing to whatever was plugged in once.
    #[default]
    Auto,
    /// The host's Xbox pad. The alias keeps a settings file written when this client also
    /// offered a separate Xbox 360 pick loadable: the two differed only in the virtual pad's
    /// name.
    #[serde(alias = "xbox360")]
    XboxOne,
    DualShock4,
    /// Adaptive triggers, lightbar, touchpad, motion — see [`crate::platform::webos::dualsense`].
    DualSense,
    /// `DualSense` plus the two back buttons and two Fn buttons.
    DualSenseEdge,
    SwitchPro,
}

impl GamepadType {
    /// The wire preference sent in the handshake, which becomes the session-default pad kind.
    pub fn to_core(self) -> punktfunk_core::config::GamepadPref {
        use punktfunk_core::config::GamepadPref as P;
        match self {
            Self::Auto => P::Auto,
            Self::XboxOne => P::XboxOne,
            Self::DualShock4 => P::DualShock4,
            Self::DualSense => P::DualSense,
            Self::DualSenseEdge => P::DualSenseEdge,
            Self::SwitchPro => P::SwitchPro,
        }
    }

    /// Whether a host pad of this kind can emit `DualSense` HID feedback (adaptive triggers,
    /// lightbar). The Edge is a `DualSense` plus extra buttons, so it carries the same effects.
    pub fn is_dualsense(self) -> bool {
        matches!(self, Self::DualSense | Self::DualSenseEdge)
    }
}

/// Override for the on-device log verbosity, settable live from the Diagnostics
/// screen — see `logger::set_level_override`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevelOverride {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Stream settings: resolution/framerate/bitrate/HDR/codec, plus the input and diagnostics
/// toggles the Settings screens expose.
///
/// `serde(default)` on the container, so every field falls back to [`Settings::default`] when a
/// settings.json written by an older build doesn't carry it — adding a field needs nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    /// Refresh rate (30/60/120) — sent to the host as the exact wire `Mode.refresh_hz`.
    pub refresh_hz: u32,
    /// `0` (Automatic — `punktfunk_core`'s own client-side AIMD bitrate controller, see
    /// `menu::BITRATE_AUTOMATIC`) or 10_000-150_000 (10-150 Mbps) fixed, adjusted via the settings
    /// slider — see `menu::BITRATE_MIN_KBPS`/`BITRATE_MAX_KBPS`.
    pub bitrate_kbps: u32,
    pub hdr_enabled: bool,
    /// Preferred session codec — see [`CodecPref`].
    pub codec: CodecPref,
    /// Which decode pipeline to load — see [`VideoBackend`]. Only offered on webOS 3.5-4.x;
    /// takes effect on the next stream, and changes what this client advertises
    /// (`core::caps::set_backend`).
    pub video_backend: VideoBackend,
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
    /// Whether the real Opus stream rides NDL's audio plane (hardware decode) instead of the
    /// software decoder. Takes effect on the next stream.
    ///
    /// **This does not decide whether the plane exists** — every accepted V2 load has one, since
    /// NDL only paces the picture against a fed plane. Off, it carries `run_clock_plane`'s silent
    /// metronome and software decode serves the speakers.
    ///
    /// Off by default: the audio-enabled load is rejected on some webOS 5+ sets, and a set that
    /// accepts it can still play nothing, which no runtime probe detects. 5.1/7.1 stay on the
    /// software decoder regardless — NDL's Opus struct has no multistream mapping field.
    pub ndl_audio_offload: bool,
    /// Resolve the Magic Remote's OK button into left click / right click / drag by how long
    /// it's held (see `platform::webos::mouse::RemoteButtons`). Off by default — with it
    /// off, OK stays the plain immediate left click it has always been, since a remote with
    /// no working Red button then has no other way to left-click. Off also means no added
    /// wait on the release.
    pub cursor_gestures: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            width: 3840,
            height: 2160,
            refresh_hz: 60,
            // Automatic: a fixed number, however carefully picked (aurora-tv's own
            // moonlight-tv wiki calls ~35-40 Mbps the practical sweet spot for this decode
            // path), never adapts to a link that degrades mid-session the way punktfunk's
            // own client-side AIMD controller does — see `menu::BITRATE_AUTOMATIC`.
            bitrate_kbps: 0,
            hdr_enabled: true,
            stats_overlay: false,
            codec: CodecPref::Auto,
            video_backend: VideoBackend::Ndl,
            audio_channels: 2,
            log_level_override: LogLevelOverride::Info,
            show_logs: false,
            gamepad_type: GamepadType::Auto,
            cursor_capture: true,
            game_mode: false,
            ndl_audio_offload: false,
            cursor_gestures: false,
        }
    }
}

impl Settings {
    /// Normalise to what the active backend can present (`core::caps`). Called on load and
    /// whenever the backend row changes, so the document never holds a *set* value whose row
    /// the UI has just hidden. `session::connect` clamps the wire regardless.
    pub fn clamp_to_caps(&mut self) {
        let backend = crate::core::caps::effective_backend(self.video_backend);
        if backend != self.video_backend {
            tracing::info!("settings: SMP isn't offerable on this TV — using NDL");
            self.video_backend = backend;
        }
        let caps = crate::core::caps::video_caps();
        let codecs = caps.codec_prefs();
        if !codecs.contains(&self.codec) {
            tracing::info!(
                "settings: {:?} isn't offerable on this video backend — using {:?}",
                self.codec,
                codecs[0],
            );
            self.codec = codecs[0];
        }
        if !caps.hdr && self.hdr_enabled {
            tracing::info!("settings: HDR isn't presentable on this video backend — turning it off");
            self.hdr_enabled = false;
        }
        if self.audio_channels > caps.max_channels {
            tracing::info!(
                "settings: {} audio channels exceeds this backend's {} — clamping",
                self.audio_channels,
                caps.max_channels,
            );
            self.audio_channels = caps.max_channels;
        }
    }
}

/// Everything this app persists, as one document — `settings.json`, written by
/// `services::store::StateWriter`.
///
/// One file rather than one per concern: separate files can disagree after a mid-write power
/// cut, which on a TV is the normal way to turn it off. Only the credentials
/// (`client-{cert,key}.pem`) stay outside, so a settings document that fails to parse can fall
/// back to defaults without silently discarding the identity every host has pinned.
///
/// `PartialEq` is load-bearing: `services::store::StateWriter` skips writing an unchanged
/// snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub settings: Settings,
    pub known_hosts: Vec<KnownHost>,
    /// The sidebar host row the user last had active — so relaunching lands back on its game
    /// grid instead of an unfocused sidebar. `(host, port)`, not an index: `known_hosts` order
    /// isn't stable across a forget/re-add.
    pub selected_host: Option<(String, u16)>,
    /// The app version that last wrote this document (`CARGO_PKG_VERSION`). `None` means it
    /// was written before versioning existed — the only signal a future migration gets about
    /// which shape it is reading. `store::load` stamps it on first sight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Cover-art paths for a title (host-relative, fetched via mTLS). Cards prefer
/// `portrait` then `header` then `hero`; the hero backdrop prefers `hero`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Artwork {
    pub portrait: Option<String>,
    pub hero: Option<String>,
    pub header: Option<String>,
}

/// One title in the host's unified library. `id` is store-qualified (`steam:<appid>`,
/// `custom:<id>`) and doubles as the launch handle `session::connect`'s `launch`
/// parameter takes — the host resolves the actual launch spec itself from `id`.
#[derive(Clone, Debug, Deserialize)]
pub struct GameEntry {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub art: Artwork,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> KnownHost {
        KnownHost {
            games: pinned_only(DESKTOP_PIN_ID),
            ..KnownHost::default()
        }
    }

    #[test]
    fn merge_leaves_unset_fields_at_the_global_value() {
        let global = Settings {
            width: 1920,
            height: 1080,
            bitrate_kbps: 20_000,
            ..Settings::default()
        };
        let over = SettingsOverride {
            bitrate_kbps: Some(50_000),
            ..SettingsOverride::default()
        };
        let merged = over.merge_into(global);
        assert_eq!(merged.bitrate_kbps, 50_000);
        assert_eq!((merged.width, merged.height), (1920, 1080));
        assert_eq!(merged.refresh_hz, global.refresh_hz);
    }

    #[test]
    fn a_value_set_back_to_the_global_stops_being_an_override() {
        let global = Settings::default();
        let mut over = SettingsOverride {
            refresh_hz: Some(120),
            bitrate_kbps: Some(50_000),
            ..SettingsOverride::default()
        };
        over.drop_matching(&global);
        assert_eq!(over.refresh_hz, Some(120), "still differs");
        assert_eq!(over.bitrate_kbps, Some(50_000));
        // The user picks the global's own value back.
        over.refresh_hz = Some(global.refresh_hz);
        over.drop_matching(&global);
        assert_eq!(over.refresh_hz, None, "matching the global is not an override");
        over.bitrate_kbps = Some(global.bitrate_kbps);
        over.drop_matching(&global);
        assert!(over.is_empty(), "nothing differs, so nothing is persisted");
    }

    #[test]
    fn an_edit_that_undoes_itself_leaves_no_entry_behind() {
        let mut h = host();
        h.edit_overrides("steam:1", |o| o.refresh_hz = Some(120));
        assert!(h.games.contains_key("steam:1"));
        h.edit_overrides("steam:1", |o| o.refresh_hz = None);
        assert!(!h.games.contains_key("steam:1"), "an empty record is dropped");
    }

    #[test]
    fn pins_keep_their_order_and_respect_the_limit() {
        let mut h = host();
        for i in 0..MAX_PINNED_GAMES {
            h.toggle_pin(&format!("g{i}"));
        }
        // Desktop was already pinned, so the last one had no slot left.
        assert_eq!(h.pinned_count(), MAX_PINNED_GAMES);
        assert_eq!(h.pinned_ids()[0], DESKTOP_PIN_ID);
        assert!(!h.is_pinned(&format!("g{}", MAX_PINNED_GAMES - 1)));
        // Unpinning frees a slot, and a re-pin lands at the end of the block.
        h.toggle_pin("g0");
        assert!(!h.is_pinned("g0"));
        h.toggle_pin("g0");
        assert_eq!(*h.pinned_ids().last().expect("just pinned"), "g0");
    }

    #[test]
    fn pruning_drops_vanished_games_but_never_desktop() {
        let mut h = host();
        h.toggle_pin("steam:gone");
        h.edit_overrides("steam:here", |o| o.hdr_enabled = Some(false));
        assert!(h.prune_games(|id| id == "steam:here"));
        assert!(h.games.contains_key(DESKTOP_PIN_ID));
        assert!(h.games.contains_key("steam:here"));
        assert!(!h.games.contains_key("steam:gone"));
        assert!(!h.prune_games(|id| id == "steam:here"), "a second pass is a no-op");
    }

    #[test]
    fn upsert_keeps_per_game_state() {
        let mut hosts = vec![host()];
        hosts[0].edit_overrides("steam:1", |o| o.codec = Some(CodecPref::Hevc));
        upsert_known_host(&mut hosts, KnownHost::default());
        assert_eq!(hosts[0].overrides("steam:1").codec, Some(CodecPref::Hevc));
        assert!(hosts[0].is_pinned(DESKTOP_PIN_ID));
    }
}
