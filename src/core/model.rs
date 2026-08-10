//! Plain domain data. No I/O — persistence lives in `crate::services`.
use serde::{Deserialize, Serialize};

/// Stream connection target.
pub struct ConnectTarget {
    pub host: String,
    pub port: u16,
    /// The pinned host fingerprint. `None` only on the dev-override path, which connects
    /// before any pairing record exists.
    pub fingerprint: Option<[u8; 32]>,
    /// Library entry id to launch, or `None` for desktop.
    pub launch: Option<String>,
}

/// `Default` exists so the literals that build one can spread `..KnownHost::default()` over the
/// fields they don't care about.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KnownHost {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// The host's pinned leaf certificate SHA-256; `None` = discovered but never paired.
    #[serde(default)]
    pub fingerprint: Option<[u8; 32]>,
    /// Management API port (game library); defaults to `library::DEFAULT_MGMT_PORT`.
    #[serde(default)]
    pub mgmt_port: Option<u16>,
    /// Wake-on-LAN MACs learned from mDNS; empty if never advertised.
    #[serde(default)]
    pub mac: Vec<String>,
    /// Auto-wake on unreachable (per-host, off by default; lives in Wake settings).
    #[serde(default)]
    pub wol_auto: bool,
    /// Pinned game IDs (up to `MAX_PINNED_GAMES`).
    #[serde(default)]
    pub pinned: Vec<String>,
}

/// Max games pinned to one host's always-visible grid row at once.
pub const MAX_PINNED_GAMES: usize = 5;

/// Pin ID for "Desktop" card (stored in pinned like games; counts toward `MAX_PINNED_GAMES`).
pub const DESKTOP_PIN_ID: &str = "__desktop__";

impl KnownHost {
    pub fn is_paired(&self) -> bool {
        self.fingerprint.is_some()
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.iter().any(|p| p == id)
    }

    /// Whether toggling id would do anything (unpin always ok, pin only if under `MAX_PINNED_GAMES`).
    pub fn can_toggle_pin(&self, id: &str) -> bool {
        self.is_pinned(id) || self.pinned.len() < MAX_PINNED_GAMES
    }

    /// Toggles `id`'s pinned state (a `GameEntry::id`, or `DESKTOP_PIN_ID`) —
    /// a no-op when `can_toggle_pin` is false.
    pub fn toggle_pin(&mut self, id: &str) {
        match self.pinned.iter().position(|p| p == id) {
            Some(i) => drop(self.pinned.remove(i)),
            None if self.can_toggle_pin(id) => self.pinned.push(id.to_string()),
            None => {}
        }
    }
}

/// Codec preference selectable in Settings — a *preference*, not a demand. The host
/// resolves the session codec from the client's advertised set via its own precedence
/// ladder (HEVC > H.264), honouring the preference only when its encoder can
/// actually produce it — so "H264" on a host that can't encode H.264 still gets HEVC
/// rather than no session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecPref {
    /// No preference — the host's precedence ladder decides (HEVC in practice).
    #[default]
    Auto,
    H264,
    Hevc,
}

/// Override for the VUI `video_full_range_flag` sent to the decoder — see
/// `session::connect`'s colour-info splice. `Auto` forwards the host's own
/// `ColorInfo.full_range` unchanged; `Full`/`Limited` force it, to test whether the
/// panel's own default (rather than what the stream signals) is what's washing out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorRangeOverride {
    #[default]
    Auto,
    Full,
    Limited,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
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

    /// Settings' display label — `None` only for `Auto`, which has no catalog entry (it isn't
    /// a pad, it's "figure out the pad"; see `gamepad_auto_label` for what it shows instead).
    pub fn label(self) -> Option<&'static str> {
        GAMEPAD_CATALOG.iter().find(|s| s.kind == self).map(|s| s.label)
    }

    /// Identifies an attached pad by its real USB vendor/product id — not by SDL's reported
    /// name, which can be too generic to use (a `DualShock 4` over Bluetooth reports plain
    /// `"Wireless Controller"`, no `dualshock`/`ps4` substring at all, and — unhelpfully — a
    /// substring an Xbox pad's own name also contains; verified on-device via
    /// `/proc/bus/input/devices`: `N: Name="Wireless Controller"`, `Vendor=054c Product=05c4`).
    /// Ids are stable across a whole pad lineup; names aren't.
    pub fn for_ids(vendor: u16, product: u16) -> Option<Self> {
        GAMEPAD_CATALOG
            .iter()
            .find(|s| s.vendor == vendor && (s.products.is_empty() || s.products.contains(&product)))
            .map(|s| s.kind)
    }
}

/// One entry per pad this client tells apart by hardware id, in the order Settings offers them
/// (right after `Auto`, which isn't itself a pad and so has no entry here). The single source
/// for a `GamepadType`'s label and the USB ids that identify it — see [`GamepadType::label`]
/// and [`GamepadType::for_ids`], which are the only things allowed to read it directly.
pub(crate) struct GamepadSpec {
    pub(crate) kind: GamepadType,
    label: &'static str,
    vendor: u16,
    /// Empty matches any product from this vendor — Xbox spans many pad generations that
    /// don't need telling apart (mapping it doesn't change what's sent to the host, which
    /// already defaults to an Xbox pad; it only makes the "Automatic (Xbox)" label correct).
    /// Every other entry pins the exact ids it's actually known by.
    products: &'static [u16],
}

const SONY_VENDOR_ID: u16 = 0x054C;
const NINTENDO_VENDOR_ID: u16 = 0x057E;
const MICROSOFT_VENDOR_ID: u16 = 0x045E;

pub(crate) const GAMEPAD_CATALOG: [GamepadSpec; 5] = [
    GamepadSpec {
        kind: GamepadType::DualSense,
        label: "DualSense",
        vendor: SONY_VENDOR_ID,
        products: &[0x0CE6],
    },
    GamepadSpec {
        kind: GamepadType::DualSenseEdge,
        label: "DualSense Edge",
        vendor: SONY_VENDOR_ID,
        products: &[0x0DF2],
    },
    GamepadSpec {
        kind: GamepadType::DualShock4,
        label: "DualShock 4",
        vendor: SONY_VENDOR_ID,
        // v1 and the slim v2 revision — both the same pad as far as this client cares.
        products: &[0x05C4, 0x09CC],
    },
    GamepadSpec {
        kind: GamepadType::XboxOne,
        label: "Xbox",
        vendor: MICROSOFT_VENDOR_ID,
        products: &[],
    },
    GamepadSpec {
        kind: GamepadType::SwitchPro,
        label: "Switch Pro",
        vendor: NINTENDO_VENDOR_ID,
        products: &[0x2009],
    },
];

/// Override for the on-device log verbosity, settable live from the Diagnostics
/// screen — see `logger::set_level_override`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevelOverride {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Stream settings: resolution/framerate/bitrate/HDR/video-backend.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    /// Refresh rate (30/60/120) — sent to the host as the exact wire `Mode.refresh_hz`.
    pub refresh_hz: u32,
    /// `0` (Automatic — `punktfunk_core`'s own client-side AIMD bitrate controller, see
    /// `ui::BITRATE_AUTOMATIC`) or 10_000-150_000 (10-150 Mbps) fixed, adjusted via the settings
    /// slider — see `ui::BITRATE_MIN_KBPS`/`BITRATE_MAX_KBPS`.
    pub bitrate_kbps: u32,
    pub hdr_enabled: bool,
    /// Preferred session codec — see [`CodecPref`].
    #[serde(default)]
    pub codec: CodecPref,
    /// Whether the in-stream stats overlay (resolution/codec, measured fps, drops,
    /// decoder feed time) is drawn in the top-right corner during a stream. Off by
    /// default; takes effect on the next stream.
    #[serde(default)]
    pub stats_overlay: bool,
    /// Requested audio channel count: 2 (stereo), 6 (5.1) or 8 (7.1). The host clamps to
    /// what it can actually capture, and the *resolved* count drives the decoder and
    /// playback layout — `audio.rs` has always handled up to 8; only the request was
    /// pinned at stereo. `#[serde(default …)]` so an existing settings.json loads as 2.
    #[serde(default = "default_audio_channels")]
    pub audio_channels: u8,
    /// Forces the VUI range flag sent to the decoder regardless of what the host
    /// signals — see [`ColorRangeOverride`]. Debug aid for the washed-out-colour
    /// investigation; takes effect on the next connect.
    #[serde(default)]
    pub color_range_override: ColorRangeOverride,
    /// On-device log verbosity — see [`LogLevelOverride`]. Persisted, so a user's
    /// choice in Diagnostics survives restarts (fresh install defaults to `Info`);
    /// applied live via `logger::set_level_override` the moment it's changed. A
    /// `TELEMETRY_LEVEL` launch (`logger::launch_level_override`) still overrides
    /// the persisted value for that run — see `load_settings`.
    #[serde(default)]
    pub log_level_override: LogLevelOverride,
    /// Diagnostics' "Show logs" toggle, applied at startup (`App::new`). Distinct
    /// from the Yellow-button overlay cycle (`main.rs`'s `LOG_OVERLAY_STATE`),
    /// which stays ephemeral and never writes here.
    #[serde(default)]
    pub show_logs: bool,
    /// Experimental PTS smoothing for the video pump (see `session::PtsPacer`). Off
    /// by default — untested on real hardware; takes effect on the next stream.
    #[serde(default)]
    pub video_pacing: bool,
    /// Which controller the host presents to the game — see [`GamepadType`]. Defaults to
    /// `Auto`, which mirrors the attached pad (so a `DualSense` gets adaptive triggers without
    /// anyone having to find this setting); pick a kind explicitly to override that. Takes
    /// effect on the next stream, since it rides the handshake.
    #[serde(default)]
    pub gamepad_type: GamepadType,
    /// Let the TV capture the pointer for the host in-stream. On (default): local cursor
    /// hidden, relative `MouseMove` deltas sent (absolute coords stop at the panel edge), host
    /// draws the only cursor. Off: absolute `MouseMoveAbs`, and `CLIENT_CAP_CURSOR` tells a
    /// capable host to stop compositing its own so the local pointer stays visible — otherwise
    /// two cursors or none. Takes effect next stream; `serde(default)` keeps old settings.json
    /// loading as `true`.
    #[serde(default = "default_cursor_capture")]
    pub cursor_capture: bool,
    /// Ask the TV to switch to its Game picture mode for the duration of a stream (the
    /// app-plane stand-in for HDMI ALLM — see `platform::webos::game_mode`). Off by default;
    /// unverified on non-rooted installs, so it rides the Experimental screen. Applied at
    /// stream start (SDR "game" / HDR "hdrGame" per the negotiated colour path) and reverted
    /// on stream exit. `serde(default)` so an existing settings.json loads as `false`.
    #[serde(default)]
    pub game_mode: bool,
    /// Resolve the Magic Remote's OK button into left click / right click / drag by how long
    /// it's held (see `platform::webos::mouse::RemoteButtons`). Off by default — with it
    /// off, OK stays the plain immediate left click it has always been, since a remote with
    /// no working Red button then has no other way to left-click. Off also means no added
    /// wait on the release. `serde(default)` so an existing settings.json loads as `false`.
    #[serde(default)]
    pub cursor_gestures: bool,
}

fn default_audio_channels() -> u8 {
    2
}

fn default_cursor_capture() -> bool {
    true
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
            // own client-side AIMD controller does — see `ui::BITRATE_AUTOMATIC`.
            bitrate_kbps: 0,
            hdr_enabled: true,
            stats_overlay: false,
            codec: CodecPref::Auto,
            audio_channels: default_audio_channels(),
            color_range_override: ColorRangeOverride::Auto,
            log_level_override: LogLevelOverride::Info,
            show_logs: false,
            video_pacing: false,
            gamepad_type: GamepadType::Auto,
            cursor_capture: default_cursor_capture(),
            game_mode: false,
            cursor_gestures: false,
        }
    }
}

/// Cover-art paths for a title (host-relative, fetched via mTLS).
/// art.rs prefers `portrait`, falls back to `header`. `hero`/`logo` unused.
#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)]
pub struct Artwork {
    pub portrait: Option<String>,
    pub hero: Option<String>,
    pub logo: Option<String>,
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
    use super::GamepadType;

    /// Every non-`Auto` variant must have a catalog entry — `label()` fails silently (`None`)
    /// rather than refusing to compile if one's missing. The `match` below has no wildcard
    /// arm, so adding a `GamepadType` variant breaks this test's compile until it's listed
    /// here too — the reminder to also add it to `GAMEPAD_CATALOG`.
    #[test]
    fn catalog_covers_every_kind_but_auto() {
        fn check(kind: GamepadType) {
            match kind {
                GamepadType::Auto => assert!(kind.label().is_none(), "Auto must not have a catalog entry"),
                GamepadType::XboxOne
                | GamepadType::DualShock4
                | GamepadType::DualSense
                | GamepadType::DualSenseEdge
                | GamepadType::SwitchPro => assert!(kind.label().is_some(), "{kind:?} has no GAMEPAD_CATALOG entry"),
            }
        }
        check(GamepadType::Auto);
        check(GamepadType::XboxOne);
        check(GamepadType::DualShock4);
        check(GamepadType::DualSense);
        check(GamepadType::DualSenseEdge);
        check(GamepadType::SwitchPro);
    }

    #[test]
    fn known_pads_are_recognized_by_id() {
        assert_eq!(GamepadType::for_ids(0x054C, 0x05C4), Some(GamepadType::DualShock4));
        assert_eq!(GamepadType::for_ids(0x054C, 0x09CC), Some(GamepadType::DualShock4));
        assert_eq!(GamepadType::for_ids(0x054C, 0x0CE6), Some(GamepadType::DualSense));
        assert_eq!(GamepadType::for_ids(0x054C, 0x0DF2), Some(GamepadType::DualSenseEdge));
        assert_eq!(GamepadType::for_ids(0x057E, 0x2009), Some(GamepadType::SwitchPro));
    }

    /// Any Microsoft product id counts — unlike Sony/Nintendo, Xbox isn't pinned to one pad
    /// generation, since mapping it changes nothing about the session (the host's default
    /// already builds an Xbox pad); this only feeds the Settings label.
    #[test]
    fn any_xbox_product_id_is_recognized() {
        assert_eq!(GamepadType::for_ids(0x045E, 0x02FF), Some(GamepadType::XboxOne));
        assert_eq!(GamepadType::for_ids(0x045E, 0x0B12), Some(GamepadType::XboxOne));
    }

    /// A genuinely unrecognized vendor must stay unmapped so the host keeps choosing.
    #[test]
    fn unknown_pads_defer_to_the_host() {
        assert_eq!(GamepadType::for_ids(0x1234, 0x5678), None);
    }

    /// A product id matching a Sony pad by coincidence, under a different vendor, must not
    /// be mistaken for one — the (vendor, product) pair is what's mapped, not either alone.
    #[test]
    fn product_id_alone_is_not_enough() {
        assert_eq!(GamepadType::for_ids(0x045E, 0x05C4), None);
        assert_eq!(GamepadType::for_ids(0x054C, 0x2009), None);
    }
}
