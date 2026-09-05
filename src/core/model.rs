//! Plain domain data. No I/O — persistence lives in `crate::services`.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pf_client_core::profiles::StreamProfile;

use crate::core::caps::VideoCaps;

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
    /// Host OS identity chain from mDNS, used for the Desktop card's mark.
    pub os: String,
    /// Auto-wake on unreachable (per-host, off by default; lives in Host power settings).
    pub wol_auto: bool,
    /// What to do to this host when the app exits (per-host, off by default; sits under
    /// `wol_auto` in Host power settings — the two are the same switch pointing opposite ways).
    pub exit_action: ExitAction,
    /// Per-game state for this host, keyed by `GameEntry::id` (or [`DESKTOP_PIN_ID`]) — pins
    /// and settings overrides together, so one prune drops both when a game leaves the library.
    /// A `BTreeMap` so the file's key order is stable and diffable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub games: BTreeMap<String, GamePrefs>,
    /// This host's default settings profile, by catalog id — what a title with no binding of
    /// its own streams with (the shared `KnownHost::profile_id`). `None` is the global document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// Grid section order, Library included as the [`Collection::dynamic`] entry. One vector
    /// carries everything: order, names and membership. `None` means "never migrated" — see
    /// `services::store::load`, which is the only place that may leave it so.
    ///
    /// Visible only so the add/pair flows can seed it in a struct literal. Read it through
    /// [`KnownHost::collections`] and change it through the methods below, which are what
    /// keep a game in at most one collection and Library unremovable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) collections: Option<Vec<Collection>>,
}

/// What the app does to a host on its way out — the second half of that host's power settings.
///
/// Maps onto punktfunk's host actions (`POST /api/v1/actions/{id}`, host-side from core
/// 0.33.0), which need the pairing's Host power grant. Defaults to [`Self::None`]: a client
/// that quietly powered a machine down would be worse than one that never offered to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitAction {
    #[default]
    None,
    Sleep,
    Shutdown,
}

impl ExitAction {
    /// Every value, in the order the dropdown lists them.
    pub const ALL: [Self; 3] = [Self::None, Self::Sleep, Self::Shutdown];

    /// The host action id to invoke, or `None` when there is nothing to do.
    pub fn action_id(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Sleep => Some("power.sleep"),
            Self::Shutdown => Some("power.shutdown"),
        }
    }
}

/// One grid section: a named, ordered set of game ids. Exactly one per host is
/// [`Collection::dynamic`] (Library), whose members are computed rather than stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub name: String,
    /// Member ids ([`GameEntry::id`] or [`DESKTOP_PIN_ID`]), in user order. Unbounded — there
    /// is no per-collection card limit.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub games: Vec<String>,
    /// Library: holds whatever is in no other collection, so `games` is always empty on disk.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dynamic: bool,
}

impl Collection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            games: Vec::new(),
            dynamic: false,
        }
    }

    /// The one dynamic entry. Never removable, members never stored.
    pub fn library() -> Self {
        Self {
            name: LIBRARY_COLLECTION.to_string(),
            games: Vec::new(),
            dynamic: true,
        }
    }
}

/// What one game carries on one host. Absent from `KnownHost::games` entirely when it holds
/// nothing — an untouched game costs no bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamePrefs {
    /// Pre-collections pin slot, read once by the migration in `services::store::load` and
    /// never written again — collections carry the ordering now.
    #[serde(rename = "pin", skip_serializing)]
    pub legacy_pin: Option<u32>,
    /// The settings profile this game streams with — an id into [`Persisted::profiles`], the
    /// shared catalog punktfunk's other clients keep. `None` inherits the globals.
    ///
    /// The overrides this used to hold inline are that profile's sparse overlay now, so one
    /// game's settings are a thing the shared shell can list, bind and show a chip for rather
    /// than a shape only this client understood.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl GamePrefs {
    /// Nothing worth persisting — the prune drops these. `legacy_pin` never serializes, so
    /// it does not keep an entry alive.
    fn is_empty(&self) -> bool {
        self.profile.is_none()
    }
}

/// Max *user* collections per host. The dynamic Library entry is not one of them.
pub const MAX_COLLECTIONS: usize = 20;

/// Max collection name length, in chars.
pub const MAX_COLLECTION_NAME: usize = 24;

/// The dynamic entry's name on a freshly migrated host — renameable like any other.
pub const LIBRARY_COLLECTION: &str = "Library";

/// The collection a migrated host's old pins land in, and the one a new host starts with.
pub const PINNED_COLLECTION: &str = "Pinned";

/// Pin ID for the "Desktop" card — a `games` key and a collection member like any other.
/// Never pruned, since no library listing contains it.
pub const DESKTOP_PIN_ID: &str = "__desktop__";

impl KnownHost {
    pub fn is_paired(&self) -> bool {
        self.fingerprint.is_some()
    }

    /// The grid sections, in order. Empty only on a host `store::load` never migrated —
    /// every path that builds or loads a host leaves at least the Library entry here.
    pub fn collections(&self) -> &[Collection] {
        self.collections.as_deref().unwrap_or_default()
    }

    /// Whether this host still needs the pre-collections migration (see `services::store`).
    pub(crate) fn needs_migration(&self) -> bool {
        self.collections.is_none()
    }

    /// Installs the migrated vector. Only `services::store`'s migration calls this; every
    /// other mutation goes through the methods below, which cannot break the invariants.
    pub(crate) fn set_collections(&mut self, collections: Vec<Collection>) {
        self.collections = Some(collections);
    }

    fn collections_mut(&mut self) -> &mut Vec<Collection> {
        self.collections.get_or_insert_with(|| vec![Collection::library()])
    }

    /// Index of the dynamic (Library) entry. Present on every migrated host; `None` only
    /// before migration, where there is nothing to draw anyway.
    pub fn library_index(&self) -> Option<usize> {
        self.collections().iter().position(|c| c.dynamic)
    }

    /// Which collection holds `id`, or `None` for Library — where every id that no
    /// collection names implicitly lives.
    pub fn collection_of(&self, id: &str) -> Option<usize> {
        self.collections()
            .iter()
            .position(|c| !c.dynamic && c.games.iter().any(|g| g == id))
    }

    /// Moves `id` into collection `to`, or back to Library with `None`. A game is in at
    /// most one collection, so this removes it from any other first. Appends: a just-moved
    /// card is looked for at the end of the block it joined.
    pub fn move_to(&mut self, id: &str, to: Option<usize>) {
        let collections = self.collections_mut();
        for entry in collections.iter_mut() {
            entry.games.retain(|g| g != id);
        }
        if let Some(entry) = to.and_then(|i| collections.get_mut(i)).filter(|c| !c.dynamic) {
            entry.games.push(id.to_string());
        }
    }

    /// This game's bound profile id, if it has one. Reading never creates an entry.
    pub fn game_profile(&self, id: &str) -> Option<&str> {
        self.games.get(id).and_then(|g| g.profile.as_deref())
    }

    /// Binds (or with `None`, clears) this game's profile, dropping the entry when nothing
    /// is left in it.
    pub fn bind_game_profile(&mut self, id: &str, profile: Option<String>) {
        self.games.entry(id.to_string()).or_default().profile = profile;
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
        let mut dropped = self.games.len() != before;
        for entry in self.collections.iter_mut().flatten() {
            let before = entry.games.len();
            entry.games.retain(|id| id == DESKTOP_PIN_ID || live(id));
            dropped |= entry.games.len() != before;
        }
        dropped
    }
}

/// Editing a host's collections: what the collections modal and its dialogs drive. Split out
/// so the invariants have one home — a game in at most one collection, Library unremovable,
/// names trimmed and unique — and so no caller can reach past them into the vector itself.
///
impl KnownHost {
    /// The member `id` trades places with inside its own collection: its neighbour in
    /// *grid* order. [`DESKTOP_PIN_ID`] is an ordinary member here — it moves, and is moved
    /// past, like any card. `None` at either end of the block, and in Library, whose order
    /// is recency rather than the user's.
    pub fn collection_neighbour(&self, id: &str, forward: bool) -> Option<&str> {
        let at = self.collection_of(id)?;
        let games = &self.collections().get(at)?.games;
        let pos = games.iter().position(|g| g == id)?;
        if forward {
            games.get(pos + 1)
        } else {
            games[..pos].last()
        }
        .map(String::as_str)
    }

    /// Swaps `id` with [`Self::collection_neighbour`] — the in-collection card reorder.
    /// `false` when there is nowhere to go, which the caller shows as a reject nudge.
    pub fn swap_within_collection(&mut self, id: &str, forward: bool) -> bool {
        let Some(other) = self.collection_neighbour(id, forward).map(str::to_string) else {
            return false;
        };
        let Some(at) = self.collection_of(id) else {
            return false;
        };
        let Some(entry) = self.collections_mut().get_mut(at) else {
            return false;
        };
        let (Some(a), Some(b)) = (
            entry.games.iter().position(|g| g == id),
            entry.games.iter().position(|g| *g == other),
        ) else {
            return false;
        };
        entry.games.swap(a, b);
        true
    }

    /// Number of user collections — what [`MAX_COLLECTIONS`] bounds.
    pub fn user_collection_count(&self) -> usize {
        self.collections().iter().filter(|c| !c.dynamic).count()
    }

    pub fn can_add_collection(&self) -> bool {
        self.user_collection_count() < MAX_COLLECTIONS
    }

    /// Whether `name` may be used for collection `at` (`None` when adding): trimmed,
    /// non-empty, within [`MAX_COLLECTION_NAME`] and unique case-insensitively. What gates
    /// the add/rename confirm button.
    pub fn can_name(&self, at: Option<usize>, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_COLLECTION_NAME {
            return false;
        }
        !self
            .collections()
            .iter()
            .enumerate()
            .any(|(i, c)| Some(i) != at && c.name.eq_ignore_ascii_case(name))
    }

    /// Appends a user collection, returning its index. `None` when the name is refused or
    /// the host is at [`MAX_COLLECTIONS`].
    pub fn add_collection(&mut self, name: &str) -> Option<usize> {
        if !self.can_add_collection() || !self.can_name(None, name) {
            return None;
        }
        let collections = self.collections_mut();
        collections.push(Collection::new(name.trim()));
        Some(collections.len() - 1)
    }

    /// Renames any entry, Library included. `false` when the name is refused.
    pub fn rename_collection(&mut self, at: usize, name: &str) -> bool {
        if !self.can_name(Some(at), name) {
            return false;
        }
        let name = name.trim().to_string();
        match self.collections_mut().get_mut(at) {
            Some(entry) => {
                entry.name = name;
                true
            }
            None => false,
        }
    }

    /// Removes a user collection; its members fall back to Library. Refuses the dynamic
    /// entry — the missing Remove icon on that row is not this rule's only enforcement.
    pub fn remove_collection(&mut self, at: usize) -> bool {
        if self.collections().get(at).is_none_or(|c| c.dynamic) {
            return false;
        }
        self.collections_mut().remove(at);
        true
    }

    /// Moves an entry within the order, the dynamic one included: this vector *is* the grid
    /// section order.
    pub fn reorder_collection(&mut self, from: usize, to: usize) -> bool {
        let collections = self.collections_mut();
        if from >= collections.len() || to >= collections.len() || from == to {
            return false;
        }
        let entry = collections.remove(from);
        collections.insert(to, entry);
        true
    }
}

/// The collections a genuinely new host starts with: "Pinned" holding the Desktop card,
/// then Library — which is what a pre-collections install looked like after its first pair.
pub fn new_host_collections() -> Vec<Collection> {
    let mut pinned = Collection::new(PINNED_COLLECTION);
    pinned.games.push(DESKTOP_PIN_ID.to_string());
    vec![pinned, Collection::library()]
}

/// Upserts by `(host, port)`, keeping the existing fingerprint if the new record is unpaired
/// (a fresh mDNS discovery shouldn't clobber a paired host) — same reasoning for `mac`,
/// learned separately (see `App::drain_discovery`) and not necessarily known again at the
/// point something else re-upserts this host. The same rule preserves `os`. `games` and
/// `wol_auto` are *always* kept from the existing record — as are `collections`: only the
/// collections screen, the per-game settings screen and the Wake screen change them, so no
/// add/edit/re-pair flow may clobber any of it.
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
    if new.os.is_empty() {
        new.os.clone_from(&existing.os);
    }
    new.games.clone_from(&existing.games);
    new.collections.clone_from(&existing.collections);
    new.wol_auto = existing.wol_auto;
    new.exit_action = existing.exit_action;
    *existing = new;
}

/// Which look the menus draw in, picked on the Settings screen — the persisted name of a
/// `ui::theme` preset, and the only part of a theme that belongs to the domain.
///
/// The default is the glass look, which is `ui::theme::PRESETS`' first entry: a fresh
/// install has no `theme` key at all and draws frosted until someone picks otherwise. A
/// document that names the flat look keeps it — the change is to what *absence* means, not
/// to anyone's stored pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    Funk,
    #[default]
    FunkGlass,
}

/// Anything but a name this build knows deserializes to [`ThemeChoice::default`].
///
/// Hand-written rather than derived: a derived enum rejects an unknown string, and since
/// `Settings` is loaded with one `from_value` that error would discard the *whole* document
/// — every real setting lost to a cosmetic field written by a build that had one more look.
impl<'de> Deserialize<'de> for ThemeChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Through `Value` so any JSON shape at all lands here rather than failing to parse.
        let v = serde_json::Value::deserialize(d)?;
        Ok(match v.as_str() {
            // `"default"` is what older documents call the flat look.
            Some("funk" | "default") => Self::Funk,
            _ => Self::default(),
        })
    }
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

/// Where a session's audio is decoded and played — the two routes this client can build,
/// selectable in Settings and swappable without touching the pipeline.
///
/// The routes trade the same way: everything below [`Self::Software`] is a shorter path onto the
/// panel's own clock, and gives up what this client can steer in exchange. Which is actually
/// smoother is hardware-specific (`docs/NOTES.md` § "NDL's audio plane"), hence a setting rather
/// than a fixed choice.
///
/// Not every route carries every layout, and nothing is ever folded down: what the selected route
/// can put on a speaker is what the handshake asks the host to encode (`AudioRoutePref::max_channels`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioRoutePref {
    /// Software Opus decode → the TV's SDL audio device, with NDL's silent clock plane keeping
    /// the picture paced. The longest path, every layout, and the only one whose pacing is proven
    /// on hardware — so, the default.
    #[default]
    Software,
    /// The wire's Opus, decoded by the TV on its audio plane. No local decode at all. Stereo
    /// only — NDL's Opus struct has no multistream mapping field — and some sets accept the load
    /// and then play nothing, which no runtime probe detects.
    NdlOpus,
}

impl AudioRoutePref {
    /// Whether the real stream rides NDL's audio plane — i.e. no SDL device is opened, and the
    /// clock plane is a standing filler rather than the only feed.
    pub fn on_ndl_plane(self) -> bool {
        self != Self::Software
    }

    /// How the stats overlay names this route. Which decoder ran leads the line: the paths fail
    /// differently, and reading the numbers without knowing which produced them has already cost
    /// real debugging time.
    pub fn overlay_tag(self) -> &'static str {
        match self {
            Self::Software => "Opus SW",
            Self::NdlOpus => "Opus HW",
        }
    }

    /// Widest layout this route can put on a speaker, and therefore the ceiling on what the
    /// handshake asks the host to encode. Per route, never global: NDL's plane and the SDL device
    /// have different ceilings, and clamping every session by the narrower one cost users 5.1 on
    /// a route that plays it.
    pub fn max_channels(self, caps: VideoCaps) -> u8 {
        match self {
            // SDL opens whatever the negotiated layout is; nothing folds.
            Self::Software => caps.max_channels,
            Self::NdlOpus => caps.max_channels.min(2),
        }
    }

    /// The routes this device can actually build, in display order — software first, it being
    /// the default and the only one that needs no plane. The offload route needs NDL v2's audio
    /// type (`VideoCaps::audio_plane`); on webOS 4 and below there is no plane to ride and
    /// software is the whole list.
    pub fn available(caps: VideoCaps) -> &'static [Self] {
        if caps.audio_plane {
            &[Self::Software, Self::NdlOpus]
        } else {
            &[Self::Software]
        }
    }
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

/// Slider range, in 5 Mbps steps.
pub const BITRATE_MIN_KBPS: u32 = 10_000;
/// The one bitrate ceiling this client has. It bounds the manual slider AND, through `main`,
/// `punktfunk_core::abr`'s automatic wire-budget climb (`PUNKTFUNK_ABR_MAX_MBPS`) and the startup
/// link-capacity probe's burst target (`PUNKTFUNK_ABR_PROBE_KBPS`) — an Automatic session that
/// could climb past what the slider allows would just be a second, hidden setting.
pub const BITRATE_MAX_KBPS: u32 = 200_000;

/// A slider's discrete positions: a closed range walked in fixed steps.
///
/// One value type for every slider — the three HDR measurements and Bitrate — so the range, the
/// stop count, the value at a stop and the snap back onto the lattice all come from the same
/// numbers. They have to stay inverses of each other, and spelling each one out per slider is
/// how they stop being.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Lattice {
    pub lo: u32,
    pub hi: u32,
    pub step: u32,
}

impl Lattice {
    /// How many positions the slider has.
    #[must_use]
    pub fn stops(self) -> usize {
        ((self.hi - self.lo) / self.step) as usize + 1
    }

    /// Which position `value` sits at.
    #[must_use]
    pub fn index(self, value: u32) -> usize {
        ((value.clamp(self.lo, self.hi) - self.lo) / self.step) as usize
    }

    /// The value at `stop`, which is clamped into range first.
    #[must_use]
    pub fn value(self, stop: i32) -> u32 {
        let stop = stop.clamp(0, self.stops() as i32 - 1) as u32;
        (self.lo + stop * self.step).min(self.hi)
    }

    /// Where `value` sits along the track, as 0..1.
    #[must_use]
    pub fn fraction(self, value: u32) -> f32 {
        let last = self.stops().saturating_sub(1);
        if last == 0 {
            0.0
        } else {
            self.index(value) as f32 / last as f32
        }
    }

    /// The stop nearest a 0..1 position along the track — the inverse of [`Lattice::fraction`].
    #[must_use]
    pub fn stop_at(self, fraction: f32) -> i32 {
        let last = self.stops().saturating_sub(1) as f32;
        (fraction.clamp(0.0, 1.0) * last).round() as i32
    }

    /// Clamps into range, then rounds to the nearest stop.
    #[must_use]
    pub fn snap(self, value: u32) -> u32 {
        let offset = value.clamp(self.lo, self.hi) - self.lo;
        self.value(((offset + self.step / 2) / self.step) as i32)
    }
}

/// Peak-brightness slider, in nits — and, while the calibration screen is up, the mastering
/// maximum the pattern declares. The ceiling has to sit well above any panel the app runs on, or
/// the slider ends before the TV starts compressing and the reading cannot be taken: a CX
/// measures ~790, a G3 with MLA ~1300, a 2025 G5 ~2400.
pub const HDR_PEAK: Lattice = Lattice {
    lo: 300,
    hi: 4_000,
    step: 10,
};
/// Full-field (frame-average) slider, in nits, and the pattern's declared `MaxFALL` while it is
/// being measured. OLEDs hold ~140-180 once ABL settles; backlit LCDs hold far more, so this too
/// has to reach past any of them for the flattening point to land inside the slider.
pub const HDR_FRAME_AVG: Lattice = Lattice {
    lo: 100,
    hi: 1_000,
    step: 10,
};
/// Black-floor slider, as 10-bit narrow-range PQ luma codes — 64 (zero light) up to 160
/// (about 0.4 nits, a poor edge-lit panel).
///
/// In codes rather than in nits because PQ is perceptually uniform and nits are not: the first
/// few codes above black span several decades of luminance, so a slider stepping through nits
/// would sit on one code for most of its travel while the picture never changed. One step here
/// is always one visible step.
pub const HDR_BLACK: Lattice = Lattice {
    lo: 64,
    hi: 160,
    step: 4,
};

/// The panel's HDR colour volume, as measured by the calibration screen.
///
/// These are the three luminances of the CTA-861.3 HDR static-metadata block. They travel to the
/// TV (so its tone map onto this panel becomes an identity) and to the host in
/// `Hello::display_hdr`, where punktfunk codes them into the virtual display's EDID — so the game
/// renders to this volume in the first place and nothing has to remap it later. That single
/// source-side tone map is what `HGiG` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HdrDisplay {
    /// Small-window peak (`MaxCLL`, and the mastering display's maximum).
    pub peak_nits: u16,
    /// Sustained full-field maximum (`MaxFALL`).
    pub frame_avg_nits: u16,
    /// Black floor, as a 10-bit narrow-range PQ luma code — see [`HDR_BLACK`].
    pub black_code: u16,
}

impl HdrDisplay {
    /// The black floor in the 0.0001 cd/m² units the wire carries, never zero: ST.2086 reads a
    /// zero there as "unknown", and a self-emissive panel's real floor is better described by the
    /// smallest luminance the field can express than by no answer at all.
    #[must_use]
    pub fn min_luminance_units(self) -> u32 {
        ((crate::core::pq::pq_nits(self.black_code) * 10_000.0).round() as u32).max(1)
    }

    /// HDR10 mastering metadata describing this panel.
    ///
    /// It goes two places, and both matter. To NDL, where it is the volume the TV tone-maps the
    /// stream into: give it the panel's real numbers and that map becomes an identity. And to the
    /// host in `Hello::display_hdr`, which codes it into the virtual display's CTA-861.3 HDR block,
    /// so the game renders to this volume rather than to a placeholder someone else has to undo.
    /// One tone map, at the source — which is what `HGiG` asks for.
    ///
    /// The defaults are an LG CX's, which is what this client sent to every TV before the
    /// calibration screen existed.
    #[must_use]
    pub fn hdr_meta(self) -> punktfunk_core::quic::HdrMeta {
        punktfunk_core::quic::HdrMeta {
            // G, B, R order (ST.2086), 1/50000 chromaticity units — BT.2020 primaries.
            display_primaries: [[8_500, 39_850], [6_550, 2_300], [35_400, 14_600]],
            white_point: [15_635, 16_450], // D65
            max_display_mastering_luminance: u32::from(self.peak_nits) * 10_000,
            min_display_mastering_luminance: self.min_luminance_units(),
            max_cll: self.peak_nits,
            max_fall: self.frame_avg_nits,
        }
    }
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
    /// Which look the menus draw in — see [`ThemeChoice`]. Cosmetic and purely local, so it
    /// applies the moment it is picked rather than on the next launch.
    pub theme: ThemeChoice,
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

/// When the shared shell takes the menus over. Mirrors Android's `gamepad_ui_mode`, and
/// serializes to the same two strings the shell's own row stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GamepadUiMode {
    /// Only while a real game pad is attached. A Magic Remote is not one — see
    /// `platform::webos::gamepad::any_pad_connected`.
    #[default]
    Connected,
    /// Whenever the switch is on, pad or no pad.
    Always,
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
            // own client-side AIMD controller does — see [`BITRATE_AUTOMATIC`].
            bitrate_kbps: 0,
            hdr_enabled: true,
            hdr_peak_nits: 800,
            hdr_frame_avg_nits: 150,
            hdr_black_code: 68,
            hdr_calibrated: false,
            stats_overlay: false,
            codec: CodecPref::Auto,
            audio_channels: 2,
            log_level_override: LogLevelOverride::Info,
            show_logs: false,
            gamepad_type: GamepadType::Auto,
            pad_haptics: true,
            pad_speaker: true,
            cursor_capture: true,
            game_mode: false,
            audio_route: AudioRoutePref::default(),
            cursor_gestures: false,
            theme: ThemeChoice::default(),
            // On, taking over only while a pad is attached — the cross-client default, and the
            // reason both UIs ship: a pad in hand wants the console, a Magic Remote wants the
            // cursor menus this client was built for.
            gamepad_ui: true,
            gamepad_ui_mode: GamepadUiMode::Connected,
        }
    }
}

impl Settings {
    /// Whether the shared shell should be fronting the app right now.
    ///
    /// The same rule Android's `gamepadUiActive` applies, minus its `tv` term: that term makes
    /// an Android TV console-only whatever the mode says, and here it would defeat the whole
    /// setting — every webOS set is a TV, and the cursor UI is the one a remote wants.
    pub fn gamepad_ui_active(&self, pad_connected: bool) -> bool {
        self.gamepad_ui && (self.gamepad_ui_mode == GamepadUiMode::Always || pad_connected)
    }

    /// Normalise to what the active backend can present (`core::caps`), plus the one
    /// cross-field rule: HDR needs HEVC, so an explicit H.264 pick turns it off. Called on
    /// load, so the document never holds a *set* value whose row the UI has just hidden or
    /// locked. `session::connect` clamps the wire regardless.
    ///
    /// Neither this nor [`Settings::presentable`] ever rewrites an override: one a current
    /// global pick shadows stays in the document, unused, and applies again once it doesn't.
    pub fn clamp_to_caps(&mut self) {
        self.clamp(true);
    }

    fn clamp(&mut self, log: bool) {
        // Only ever narrows, so `log` gating a line can't gate a mutation with it.
        macro_rules! note {
            ($($arg:tt)*) => { if log { tracing::info!($($arg)*); } };
        }
        let caps = crate::core::caps::video_caps();
        // Before the HDR rules below: which codec is in force is what decides them.
        let codecs = caps.codec_prefs();
        if !codecs.contains(&self.codec) {
            note!(
                "settings: {:?} isn't offerable on this video backend — using {:?}",
                self.codec,
                codecs[0],
            );
            self.codec = codecs[0];
        }
        if self.hdr_enabled && !caps.hdr {
            note!("settings: HDR isn't presentable on this video backend — turning it off");
            self.hdr_enabled = false;
        }
        if self.hdr_enabled && self.codec == CodecPref::H264 {
            // Mirrors `session::connect`'s own gate and `menu::RowLock::HdrNeedsHevc`: a
            // session pinned to H.264 never resolves HDR. Reachable through the merge, where
            // a game's HDR override can meet a global codec pick made after it.
            note!("settings: HDR needs HEVC — an explicit H.264 pick turns it off");
            self.hdr_enabled = false;
        }
        if !AudioRoutePref::available(caps).contains(&self.audio_route) {
            note!(
                "settings: {:?} audio needs NDL's audio plane, which this backend has none of — using Software",
                self.audio_route,
            );
            self.audio_route = AudioRoutePref::Software;
        }
        // The decoder-wide ceiling, and nothing else. `audio_channels` is a PREFERENCE — "5.1
        // where it can play" — which the route's own limit and the TV's current Sound Out narrow
        // per session, not in the document (see `session::connect`'s `Negotiated::clamp`).
        // Rewriting it from either would lose the preference the moment a receiver was unplugged.
        if self.audio_channels > caps.max_channels {
            note!(
                "settings: {} audio channels is more than this client can decode ({}) — clamping",
                self.audio_channels,
                caps.max_channels,
            );
            self.audio_channels = caps.max_channels;
        }
        // Snapped rather than merely clamped: the sliders move on a lattice, and a value off it
        // (a hand-edited settings.json, or a range that narrowed between builds) would leave a
        // thumb sitting between two stops.
        self.hdr_peak_nits = HDR_PEAK.snap(u32::from(self.hdr_peak_nits)) as u16;
        self.hdr_frame_avg_nits = HDR_FRAME_AVG.snap(u32::from(self.hdr_frame_avg_nits)) as u16;
        // A full field can never out-run a small window on any panel; believing otherwise would
        // advertise a volume no display has.
        self.hdr_frame_avg_nits = self.hdr_frame_avg_nits.min(self.hdr_peak_nits);
        self.hdr_black_code = HDR_BLACK.snap(u32::from(self.hdr_black_code)) as u16;
    }

    /// The panel volume to advertise — see [`HdrDisplay`].
    #[must_use]
    pub fn hdr_display(&self) -> HdrDisplay {
        HdrDisplay {
            peak_nits: self.hdr_peak_nits,
            frame_avg_nits: self.hdr_frame_avg_nits,
            black_code: self.hdr_black_code,
        }
    }

    /// The one writer of the stored volume: the three measured fields move together with the
    /// flag that says where they came from, so a set can never advertise numbers nothing on
    /// screen would explain.
    pub fn set_hdr_display(&mut self, display: HdrDisplay, calibrated: bool) {
        self.hdr_peak_nits = display.peak_nits;
        self.hdr_frame_avg_nits = display.frame_avg_nits;
        self.hdr_black_code = display.black_code;
        self.hdr_calibrated = calibrated;
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
    /// The settings-profile catalog: named bundles of sparse overrides, the model punktfunk's
    /// other clients keep in `client-profiles.json`. Here it rides the one document this client
    /// writes, for the reason everything else does — one file, one writer, no merge.
    ///
    /// A game's settings ARE a profile ([`GamePrefs::profile`]), which is what lets the shared
    /// shell list them and bind one to a title. A TV with no desktop app beside it can still
    /// fill this: opening a game's settings and changing a row creates the profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<StreamProfile>,
    /// The stored settings object exactly as it was last read, so the fields this client does
    /// not model survive a save.
    ///
    /// The shared schema is much wider than [`Settings`] — the gamepad shell alone writes a
    /// palette, a library view and its own layout prefs, and other clients write more. Writing
    /// the document back from [`Settings`] alone would reset every one of them on the next
    /// save from EITHER UI, which on the shell reads as a setting that will not stick.
    ///
    /// `serde(skip)`: it is not a key of its own, it IS the settings object — reconstructed on
    /// load (`services::store::from_document`) and merged over on save.
    #[serde(skip)]
    pub shared_base: pf_client_core::trust::Settings,
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
    /// Packaged brand mark token, such as `steam`, for art-less launcher cards.
    #[serde(default)]
    pub icon: Option<String>,
}
