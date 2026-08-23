//! Plain domain data. No I/O — persistence lives in `crate::services`.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    /// Auto-wake on unreachable (per-host, off by default; lives in Wake settings).
    pub wol_auto: bool,
    /// Per-game state for this host, keyed by `GameEntry::id` (or [`DESKTOP_PIN_ID`]) — pins
    /// and settings overrides together, so one prune drops both when a game leaves the library.
    /// A `BTreeMap` so the file's key order is stable and diffable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub games: BTreeMap<String, GamePrefs>,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamePrefs {
    /// Pre-collections pin slot, read once by the migration in `services::store::load` and
    /// never written again — collections carry the ordering now.
    #[serde(rename = "pin", skip_serializing)]
    pub legacy_pin: Option<u32>,
    /// Settings this game overrides; every unset field falls through to the global [`Settings`].
    #[serde(skip_serializing_if = "SettingsOverride::is_empty")]
    pub over: SettingsOverride,
}

impl GamePrefs {
    /// Nothing worth persisting — the prune drops these. `legacy_pin` never serializes, so
    /// it does not keep an entry alive.
    fn is_empty(&self) -> bool {
        self.over.is_empty()
    }
}

/// The one table every per-game override derives from — the struct, [`OverrideField`] and all
/// the merge/capture/clear logic are generated from it, so a new overridable setting is one
/// line here plus one row mapping in `app::menu::row_fields`.
macro_rules! settings_override {
    ($(
        $(#[$attr:meta])*
        $field:ident: $ty:ty as $variant:ident,
        |$get:ident| $read:expr,
        |$set:ident, $val:ident| $write:expr;
    )*) => {
        /// A sparse [`Settings`] diff: `Some` overrides the global value for one game, `None`
        /// inherits it.
        ///
        /// A field starts overriding when the user picks a value the global doesn't have, and
        /// stops two ways, both done *on this screen*: picking the global's own value back
        /// ([`SettingsOverride::capture`] stores nothing) or clearing the row
        /// ([`SettingsOverride::clear`]). The global later drifting onto the value does
        /// **not** clear it — "pinned to 60 Hz" must survive the global moving away again.
        ///
        /// Deliberately *not* every field. Experimental and diagnostics toggles are
        /// device-wide, and `video_backend` is a process-global (`core::caps::set_backend`,
        /// read by `session::connect`'s clamp and the row locks), so a per-game value would
        /// need an apply/restore around every launch.
        ///
        /// `Copy + Hash + Eq` because it rides the render cache keys next to `Settings`.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(default)]
        pub struct SettingsOverride {
            $(
                $(#[$attr])*
                #[serde(skip_serializing_if = "Option::is_none")]
                pub $field: Option<$ty>,
            )*
        }

        /// One overridable field, as a value — what `app::menu` keys its row mapping by, so
        /// "which rows carry a mark" and "what an edited row records" read the same table.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum OverrideField {
            $($variant,)*
        }

        impl SettingsOverride {
            /// Whether this field overrides the global — what puts the mark on its row.
            pub fn is_set(&self, field: OverrideField) -> bool {
                match field {
                    $(OverrideField::$variant => self.$field.is_some(),)*
                }
            }

            /// Records `field` from `edited` unless that is what `global` says right now, in
            /// which case it goes back to inheriting (see [`SettingsOverride`]).
            ///
            /// Only the named field: editing Bitrate must neither pin Resolution to whatever
            /// the global happened to be, nor clear an override the global has drifted onto.
            pub fn capture(&mut self, field: OverrideField, edited: &Settings, global: &Settings) {
                match field {
                    $(OverrideField::$variant => {
                        let value = { let $get = edited; $read };
                        let inherited = { let $get = global; $read };
                        self.$field = (value != inherited).then_some(value);
                    })*
                }
            }

            /// Drops `field` back to inheriting the global, for a value that genuinely
            /// differs — the other way an override ends (see [`SettingsOverride`]).
            pub fn clear(&mut self, field: OverrideField) {
                match field {
                    $(OverrideField::$variant => self.$field = None,)*
                }
            }

            /// `base` with every set field applied. The result still needs
            /// [`Settings::clamp_to_caps`] before it reaches the wire, exactly like a
            /// global value.
            #[must_use]
            pub fn merge_into(&self, mut base: Settings) -> Settings {
                $(
                    if let Some($val) = self.$field {
                        let $set = &mut base;
                        $write;
                    }
                )*
                base
            }
        }
    };
}

settings_override! {
    /// Width and height move together — they are one Resolution row.
    mode: (u32, u32) as Mode,
        |s| (s.width, s.height),
        |s, v| { s.width = v.0; s.height = v.1; };
    refresh_hz: u32 as RefreshHz, |s| s.refresh_hz, |s, v| s.refresh_hz = v;
    bitrate_kbps: u32 as BitrateKbps, |s| s.bitrate_kbps, |s, v| s.bitrate_kbps = v;
    hdr_enabled: bool as HdrEnabled, |s| s.hdr_enabled, |s, v| s.hdr_enabled = v;
    codec: CodecPref as Codec, |s| s.codec, |s, v| s.codec = v;
    audio_channels: u8 as AudioChannels, |s| s.audio_channels, |s, v| s.audio_channels = v;
    gamepad_type: GamepadType as GamepadKind, |s| s.gamepad_type, |s, v| s.gamepad_type = v;
    cursor_capture: bool as CursorCapture, |s| s.cursor_capture, |s, v| s.cursor_capture = v;
    cursor_gestures: bool as CursorGestures, |s| s.cursor_gestures, |s, v| s.cursor_gestures = v;
}

impl SettingsOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
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

/// The cursor-capture override the Desktop card carries: *off*, since capture is on globally
/// for the games that are the common case and the desktop is the one card where the host's own
/// pointer should stay visible. Doubles as the shipped demo of per-game overrides.
///
/// `None` when the global is already off: a seeded default has no business pinning a value the
/// user would then have to find and clear (a *user-set* override equal to the global is kept —
/// see [`SettingsOverride`]). The one place this default lives: new hosts get it from
/// [`new_host_games`], existing ones from `store`'s version bootstrap.
pub fn desktop_capture_override(global: &Settings) -> Option<bool> {
    global.cursor_capture.then_some(false)
}

/// The `games` map a genuinely new host starts with: the Desktop card wearing
/// [`desktop_capture_override`]. Its *placement* is [`new_host_collections`]'s job.
pub fn new_host_games(global: &Settings) -> BTreeMap<String, GamePrefs> {
    let mut games = BTreeMap::new();
    if let Some(capture) = desktop_capture_override(global) {
        games
            .entry(DESKTOP_PIN_ID.to_string())
            .or_insert_with(GamePrefs::default)
            .over
            .cursor_capture = Some(capture);
    }
    games
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
/// point something else re-upserts this host. `games` and `wol_auto` are *always* kept from the
/// existing record — as are `collections`: only the collections screen, the per-game settings
/// screen and the Wake screen change them, so no add/edit/re-pair flow may clobber any of it.
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
    new.collections.clone_from(&existing.collections);
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
    /// type (`VideoCaps::audio_plane`); on webOS 4 and below, and under SMP, there is no plane
    /// to ride and software is the whole list.
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
/// `punktfunk_core::abr`'s automatic climb (`PUNKTFUNK_ABR_MAX_MBPS`) and the startup
/// link-capacity probe's burst target (`PUNKTFUNK_ABR_PROBE_KBPS`) — an Automatic session that
/// could climb past what the slider allows would just be a second, hidden setting.
pub const BITRATE_MAX_KBPS: u32 = 200_000;
/// Slider granularity — also the lattice of valid fixed-bitrate values.
pub const BITRATE_STEP_KBPS: u32 = 5_000;
/// Sentinel one notch below `BITRATE_MIN_KBPS` on the slider: `punktfunk_core::client::NativeClient`
/// arms its own client-side AIMD bitrate controller (`punktfunk_core::abr`) precisely when it's
/// asked to connect with `bitrate_kbps == 0` — it reacts to unrecoverable frames, heavy loss,
/// one-way-delay rise, and (via `session.rs`'s `report_decode_us` call) decode latency, backing off
/// or climbing every ~750ms. A fixed Mbps number, however carefully picked, never adapts to a link
/// that degrades mid-session — this does.
pub const BITRATE_AUTOMATIC: u32 = 0;

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
    /// Where this session's audio is decoded and played — see [`AudioRoutePref`]. Takes effect on
    /// the next stream, and caps the channel layouts the Audio row offers.
    ///
    /// **No route decides whether NDL's audio plane exists** — every accepted V2 load has one,
    /// since NDL only paces the picture against a fed plane. The routes differ in what RIDES it:
    /// `run_clock_plane`'s silent metronome, or the host's Opus.
    pub audio_route: AudioRoutePref,
    /// Stamp frames from the fixed anchor instead of playing them out on the host's cadence —
    /// see `session::timeline::Pacing`. Off by default: the cadence loop holds each frame by a
    /// cushion sized to the link's *measured* jitter (at most one frame interval, and its 0.5 ms
    /// floor on a link with nothing wrong), which is what buys a cadence that doesn't beat against
    /// the panel. On gives that cushion back and takes the judder with it — the ONE gate on every
    /// latency-adding measure in the video path, which is why it lives on the Experimental screen
    /// rather than beside Resolution. Takes effect on the next stream.
    pub direct_playback: bool,
    /// Resolve the Magic Remote's OK button into left click / right click / drag by how long
    /// it's held (see `platform::webos::mouse::RemoteButtons`). Off by default — with it
    /// off, OK stays the plain immediate left click it has always been, since a remote with
    /// no working Red button then has no other way to left-click. Off also means no added
    /// wait on the release.
    pub cursor_gestures: bool,
    /// Which look the menus draw in — see [`ThemeChoice`]. Cosmetic and purely local, so it
    /// applies the moment it is picked rather than on the next launch.
    pub theme: ThemeChoice,
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
            stats_overlay: false,
            codec: CodecPref::Auto,
            video_backend: VideoBackend::Ndl,
            audio_channels: 2,
            log_level_override: LogLevelOverride::Info,
            show_logs: false,
            gamepad_type: GamepadType::Auto,
            cursor_capture: true,
            game_mode: false,
            audio_route: AudioRoutePref::default(),
            direct_playback: false,
            cursor_gestures: false,
            theme: ThemeChoice::default(),
        }
    }
}

impl Settings {
    /// Normalise to what the active backend can present (`core::caps`), plus the one
    /// cross-field rule: HDR needs HEVC, so an explicit H.264 pick turns it off. Called on
    /// load and on every backend change, so the document never holds a *set* value whose row
    /// the UI has just hidden or locked. `session::connect` clamps the wire regardless.
    ///
    /// Neither this nor [`Settings::presentable`] ever rewrites an override: one a current
    /// global pick shadows stays in the document, unused, and applies again once it doesn't.
    pub fn clamp_to_caps(&mut self) {
        self.clamp(true);
    }

    /// [`Settings::clamp_to_caps`] as a value, and silent — the per-game screen re-derives its
    /// merged copy on every keystroke, which is no place to log the same clamp repeatedly.
    #[must_use]
    pub fn presentable(mut self) -> Self {
        self.clamp(false);
        self
    }

    fn clamp(&mut self, log: bool) {
        // Only ever narrows, so `log` gating a line can't gate a mutation with it.
        macro_rules! note {
            ($($arg:tt)*) => { if log { tracing::info!($($arg)*); } };
        }
        let backend = crate::core::caps::effective_backend(self.video_backend);
        if backend != self.video_backend {
            note!("settings: SMP isn't offerable on this TV — using NDL");
            self.video_backend = backend;
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
        let mut h = KnownHost {
            games: new_host_games(&Settings::default()),
            ..KnownHost::default()
        };
        h.set_collections(new_host_collections());
        h
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
    fn picking_the_globals_own_value_stores_no_override() {
        let global = Settings::default();
        let mut over = SettingsOverride::default();
        over.capture(
            OverrideField::RefreshHz,
            &Settings {
                refresh_hz: 120,
                ..global
            },
            &global,
        );
        assert_eq!(
            over.refresh_hz,
            Some(120),
            "differs from the global, so it is an override"
        );
        // Picking the global's own value back is the "use global" gesture.
        over.capture(OverrideField::RefreshHz, &global, &global);
        assert!(over.is_empty(), "nothing differs, so nothing is stored");
    }

    #[test]
    fn an_override_survives_the_global_moving_onto_it_and_past_it() {
        let mut global = Settings::default();
        let mut over = SettingsOverride::default();
        over.capture(
            OverrideField::RefreshHz,
            &Settings {
                refresh_hz: 120,
                ..global
            },
            &global,
        );
        // The global drifts onto the same value — a coincidence, not a gesture.
        global.refresh_hz = 120;
        assert_eq!(
            over.refresh_hz,
            Some(120),
            "an override the user never touched is left alone"
        );
        global.refresh_hz = 30;
        assert_eq!(over.merge_into(global).refresh_hz, 120);
        // Only an explicit clear inherits again.
        over.clear(OverrideField::RefreshHz);
        assert!(over.is_empty());
        assert_eq!(over.merge_into(global).refresh_hz, 30);
    }

    #[test]
    fn editing_one_row_leaves_an_unrelated_override_alone() {
        let global = Settings::default();
        let mut over = SettingsOverride::default();
        over.capture(
            OverrideField::BitrateKbps,
            &Settings {
                bitrate_kbps: 50_000,
                ..global
            },
            &global,
        );
        // A second row set to the global's own value must not drag the first one out with it.
        over.capture(OverrideField::RefreshHz, &global, &global);
        assert_eq!(over.bitrate_kbps, Some(50_000));
        assert_eq!(over.refresh_hz, None);
    }

    #[test]
    fn an_edit_that_undoes_itself_leaves_no_entry_behind() {
        let mut h = host();
        h.edit_overrides("steam:1", |o| o.refresh_hz = Some(120));
        assert!(h.games.contains_key("steam:1"));
        h.edit_overrides("steam:1", |o| o.clear(OverrideField::RefreshHz));
        assert!(!h.games.contains_key("steam:1"), "an empty record is dropped");
    }

    #[test]
    fn a_game_lives_in_at_most_one_collection() {
        let mut h = host();
        let other = h.add_collection("Racing").expect("under the limit");
        h.move_to("g0", Some(0));
        h.move_to("g0", Some(other));
        assert_eq!(h.collection_of("g0"), Some(other));
        assert_eq!(h.collections()[0].games, [DESKTOP_PIN_ID]);
        // Back to Library: named by no collection at all.
        h.move_to("g0", None);
        assert_eq!(h.collection_of("g0"), None);
    }

    #[test]
    fn moving_appends_so_a_just_moved_card_is_last() {
        let mut h = host();
        h.move_to("g0", Some(0));
        h.move_to("g1", Some(0));
        assert_eq!(h.collections()[0].games, [DESKTOP_PIN_ID, "g0", "g1"]);
    }

    #[test]
    fn names_are_trimmed_bounded_and_unique_case_insensitively() {
        let mut h = host();
        assert!(!h.can_name(None, "  "));
        assert!(!h.can_name(None, &"x".repeat(MAX_COLLECTION_NAME + 1)));
        assert!(!h.can_name(None, "pinned"), "duplicate of the existing Pinned");
        assert!(h.can_name(Some(0), "PINNED"), "a rename may keep its own name");
        assert_eq!(
            h.add_collection("  Racing  ")
                .and_then(|i| h.collections().get(i))
                .map(|c| c.name.as_str()),
            Some("Racing")
        );
    }

    #[test]
    fn the_library_entry_is_renameable_but_never_removable() {
        let mut h = host();
        let library = h.library_index().expect("bootstrapped");
        assert!(!h.remove_collection(library));
        assert!(h.rename_collection(library, "All games"));
        assert_eq!(h.collections()[library].name, "All games");
    }

    #[test]
    fn removing_a_collection_returns_its_games_to_library() {
        let mut h = host();
        h.move_to("g0", Some(0));
        assert!(h.remove_collection(0));
        assert_eq!(h.collection_of("g0"), None);
        assert_eq!(h.user_collection_count(), 0);
    }

    #[test]
    fn the_collection_limit_counts_only_user_collections() {
        let mut h = host();
        while h.can_add_collection() {
            let n = h.user_collection_count();
            assert!(h.add_collection(&format!("c{n}")).is_some());
        }
        assert_eq!(h.user_collection_count(), MAX_COLLECTIONS);
        assert!(h.add_collection("one too many").is_none());
    }

    #[test]
    fn reordering_moves_the_entry_library_included() {
        let mut h = host();
        assert!(h.reorder_collection(1, 0));
        assert_eq!(h.library_index(), Some(0));
        assert!(!h.reorder_collection(0, 0));
        assert!(!h.reorder_collection(0, 9));
    }

    #[test]
    fn swapping_within_a_collection_stops_at_its_ends() {
        let mut h = host();
        h.move_to("g0", Some(0));
        h.move_to("g1", Some(0));
        assert!(h.swap_within_collection("g1", false));
        assert_eq!(h.collections()[0].games, [DESKTOP_PIN_ID, "g1", "g0"]);
        assert!(h.swap_within_collection("g1", false), "trades with Desktop");
        assert_eq!(h.collections()[0].games, ["g1", DESKTOP_PIN_ID, "g0"]);
        assert!(!h.swap_within_collection("g1", false), "already first");
        assert!(!h.swap_within_collection("g0", true), "already last");
        assert!(!h.swap_within_collection("stranger", true), "in Library, not orderable");
    }

    #[test]
    fn the_desktop_card_reorders_like_any_other() {
        let mut h = host();
        h.move_to("g0", Some(0));
        assert_eq!(h.collections()[0].games, [DESKTOP_PIN_ID, "g0"]);
        assert_eq!(h.collection_neighbour("g0", false), Some(DESKTOP_PIN_ID));
        assert!(h.swap_within_collection(DESKTOP_PIN_ID, true));
        assert_eq!(h.collections()[0].games, ["g0", DESKTOP_PIN_ID]);
        assert!(!h.swap_within_collection(DESKTOP_PIN_ID, true), "already last");
    }

    #[test]
    fn pruning_drops_vanished_games_but_never_desktop() {
        let mut h = host();
        h.move_to("steam:gone", Some(0));
        h.edit_overrides("steam:gone", |o| o.hdr_enabled = Some(true));
        h.edit_overrides("steam:here", |o| o.hdr_enabled = Some(false));
        assert!(h.prune_games(|id| id == "steam:here"));
        assert!(h.games.contains_key(DESKTOP_PIN_ID));
        assert!(h.games.contains_key("steam:here"));
        assert!(!h.games.contains_key("steam:gone"));
        assert_eq!(
            h.collections()[0].games,
            [DESKTOP_PIN_ID],
            "and out of its collection too"
        );
        assert!(!h.prune_games(|id| id == "steam:here"), "a second pass is a no-op");
    }

    #[test]
    fn upsert_keeps_per_game_state() {
        let mut hosts = vec![host()];
        hosts[0].edit_overrides("steam:1", |o| o.codec = Some(CodecPref::Hevc));
        upsert_known_host(&mut hosts, KnownHost::default());
        assert_eq!(hosts[0].overrides("steam:1").codec, Some(CodecPref::Hevc));
        assert_eq!(
            hosts[0].collection_of(DESKTOP_PIN_ID),
            Some(0),
            "collections survive an upsert"
        );
    }
}
