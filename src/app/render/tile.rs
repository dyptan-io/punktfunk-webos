//! This app's tile numbering: which [`TileId`] means what.
//!
//! `ui` treats a tile id as an opaque number (see [`TileId`]'s docs) — the enum that used
//! to name these lived in the library and made it unusable by anything else. The numbering
//! is here, dense and `Copy`, so a draw command carries four bytes instead of a `String`.
//!
//! Ids are assigned in three bands: the fixed singletons below, one slot per spinner frame,
//! and an interned band for grid cards (whose count is the library's, not a constant).
use crate::ui::render::TileId;

/// Focus-free sidebar strip: panel, brand mark, every row unfocused.
pub const SIDEBAR: TileId = TileId(0);
/// The focused sidebar row, composited over [`SIDEBAR`].
pub const FOCUS_ROW: TileId = TileId(1);
/// The shared focus-ring glow (one card size at a time), drawn behind a focused card.
pub const RING: TileId = TileId(2);
/// The focused card's crisp lit edge, composited over its art.
pub const CARD_OUTLINE: TileId = TileId(3);
/// The open modal's shell — chrome, header, every widget drawn unfocused.
pub const MODAL: TileId = TileId(5);
/// The open modal's single focused, zoom-animated widget, composited over [`MODAL`].
pub const MODAL_FOCUS: TileId = TileId(6);
/// An open dropdown's option panel.
pub const DROPDOWN_OVERLAY: TileId = TileId(7);
/// That dropdown's focused option, composited over [`DROPDOWN_OVERLAY`].
pub const DROPDOWN_FOCUS: TileId = TileId(8);
/// Home's status line block.
pub const STATUS: TileId = TileId(9);
/// The static "no host selected" hint.
pub const NO_HOST: TileId = TileId(10);
/// Whichever scrollable modal's scroll indicator — one slot, versioned per screen.
pub const SCROLL_INDICATOR: TileId = TileId(11);
/// About's document, baked at full unscrolled height — one slot, versioned per screen, so
/// scrolling inside the baked window invalidates nothing. Settings does not use it: its rows
/// are a tile each (see [`list_row`]), so one changed value repaints one row.
pub const SCROLL_CONTENT: TileId = TileId(12);
// 13 and 14 were the scroll-edge ramp tiles. The fade now ramps the content's own alpha
// (`compose::push_faded`), so there is no band tile to bake; the ids stay retired rather than
// reused, since the rest of the table is written out by number.
/// The connecting screen's wide backdrop. One slot: only one launch is ever in flight, and
/// a new hero replaces the old texture rather than joining it.
pub const HERO: TileId = TileId(15);
/// In-stream stats overlay.
pub const STATS_OVERLAY: TileId = TileId(16);
/// Transient toast.
pub const NOTIFICATION: TileId = TileId(17);
/// The log-tail overlay, in both the menu and the stream.
pub const LOG_OVERLAY: TileId = TileId(18);
/// The in-stream disconnect confirm dialog, and its focused button.
pub const DISCONNECT_DIALOG: TileId = TileId(19);
pub const DISCONNECT_FOCUS_BUTTON: TileId = TileId(20);
/// The focused card's title strip, wiped up over its bottom edge. One slot: only one
/// card is focused at a time, and it is versioned by that card's identity.
pub const CARD_TITLE: TileId = TileId(21);
/// The card drop shadow, drawn behind every card — identical on all of them, so it is one
/// shared tile rather than a margin baked into each.
pub const CARD_SHADOW: TileId = TileId(22);
/// The modal fading *out* — a snapshot of [`MODAL`] taken the frame it was left, so the
/// entering modal can own [`MODAL`] while this one finishes (see `App::modal_prev`).
pub const MODAL_PREV: TileId = TileId(23);
/// That snapshot's scrolled content, for a leaving modal whose body lives outside its shell
/// — About's [`SCROLL_CONTENT`] frozen, or the settings rows stitched into one buffer.
pub const MODAL_PREV_CONTENT: TileId = TileId(24);
/// The focused card's submenu panel — [`CARD_TITLE`] grown to carry the Pin/Settings rows.
/// Its own slot rather than a second shape for `CARD_TITLE`, so it can be baked ahead of the
/// hold that shows it, along with the rows and title tiles keyed with it. The blur under it
/// is the compositor's (`DrawCmd::Frost`); this tile is the glass tint alone.
pub const CARD_MENU: TileId = TileId(25);
/// That panel's row icons and labels, on their own transparent tile. Composited *after* the
/// selection band, which is a translucent darkening: baked into [`CARD_MENU`] the text would
/// be dimmed along with the frost, and the band is meant to slide under it.
pub const CARD_MENU_ROWS: TileId = TileId(26);
/// That panel's title line, likewise on its own transparent tile — it rides the top edge of
/// the opening window, continuing up from where [`CARD_TITLE`] already had it.
pub const CARD_MENU_TITLE: TileId = TileId(27);
/// That panel's selection band, one row tall with the card's rounded bottom corners — used
/// for the bottom row, whose band ends on that edge; higher rows are a plain square fill.
pub const CARD_MENU_BAND: TileId = TileId(28);
/// The modal card's drop shadow, as a small nine-sliceable atlas rather than a card-sized
/// blit baked into [`MODAL`]. Built once per style epoch and stretched to whatever card is
/// open — see `ui::painter::shadow_atlas` for why the slices are exact, and
/// `compose::push_shadow` for the nine draws that place it.
pub const MODAL_SHADOW: TileId = TileId(29);
/// The same atlas at the smaller `CARD_RADIUS`, for the panels that are not the modal card —
/// currently the dropdown popup, whose own tile is sized to the panel and so could never
/// have held a baked shadow.
pub const PANEL_SHADOW: TileId = TileId(30);

/// First id of the row band — one slot per on-screen row of whichever scrolling list is open
/// (see [`list_row`]). Fixed rather than interned: the lists are short, their rows are
/// addressed by position, and only one is up at a time.
const LIST_ROW_BASE: u32 = 32;
/// Slots in that band. Settings' pages and a host's 21 collections are all far under this;
/// [`list_row`] refuses anything past it rather than colliding with the section band.
pub const LIST_ROW_SLOTS: usize = 32;

/// First id of the grid's section-heading band — one slot per drawn section, addressed by
/// position in grid order (see [`section`]). Not `ensure_static` like the old fixed
/// "Pinned"/"Library" pair: the labels are user-typed collection names now, so each slot is
/// versioned by the text it holds.
const SECTION_BASE: u32 = 64;
/// Slots in that band — every collection a host can have, plus the dynamic Library entry,
/// rounded up so the band's width is a power of two and the spinner's ids stay out of reach.
pub const SECTION_SLOTS: usize = crate::app::grid::MAX_GROUPS.next_power_of_two();

/// First id of the spinner band. One per frame, so animation is a swap not an upload.
const SPINNER_BASE: u32 = 96;
/// First id of the grid-card band, interned by pin id (see [`CardIds`]).
const CARD_BASE: u32 = 256;

/// The tile for on-screen list row `index`, or `None` past the band — a list longer than
/// [`LIST_ROW_SLOTS`] draws its tail unbaked rather than reaching into the next band's ids.
pub fn list_row(index: usize) -> Option<TileId> {
    (index < LIST_ROW_SLOTS).then(|| TileId(LIST_ROW_BASE + index as u32))
}

/// The tile for grid section `index`, or `None` past the band — a grid with more sections
/// than [`SECTION_SLOTS`] draws the tail's headings unbaked rather than reaching into the
/// spinner's ids. [`MAX_GROUPS`](crate::app::grid::MAX_GROUPS) is well under it.
pub fn section(index: usize) -> Option<TileId> {
    (index < SECTION_SLOTS).then(|| TileId(SECTION_BASE + index as u32))
}

/// The tile for spinner frame `idx`.
pub fn spinner(idx: usize) -> TileId {
    TileId(SPINNER_BASE + idx as u32)
}

/// The spinner frame `id` stands for, if it is one — `runtime` uploads these from raw
/// decoded pixels rather than from a rasterized painter, and needs to recognise them.
pub fn spinner_index(id: TileId) -> Option<usize> {
    (SPINNER_BASE..CARD_BASE)
        .contains(&id.0)
        .then(|| (id.0 - SPINNER_BASE) as usize)
}

/// Grid-card ids, interned by pin id.
///
/// Cards are keyed by identity rather than by grid position on purpose: pinning a game
/// reorders the grid, and keying by index would rebuild every tile after the moved one.
/// Slots are recycled, so a library refresh reuses the same small band rather than growing
/// the id space for the life of the process.
pub struct CardIds {
    slots: std::collections::HashMap<String, CardSlot>,
    free: Vec<TileId>,
    next: u32,
}

/// What one resident card holds: its tile, and the arrival it is playing.
///
/// The entrance lives here rather than in a second map keyed by the same string, because
/// `compose_grid` asks for both for every visible card on every frame — two hashes of one
/// game id, where the card is resident or neither answer exists.
#[derive(Clone, Copy)]
pub struct CardSlot {
    pub id: TileId,
    /// `None` for a card that is not animating.
    pub pop: Option<Entrance>,
}

/// [`Entrance::progress`] for a slot that may not be animating: `(1.0, 0.0)` — full size,
/// no scale to apply — when it is not.
pub fn entrance_progress(entrance: Option<Entrance>, now: std::time::Instant) -> (f32, f32) {
    entrance.map_or((1.0, 0.0), |e| e.progress(now))
}

/// A card arriving on screen: when it started, and which of the two arrivals it is.
///
/// Kind and clock together on the slot, rather than a grid-wide "a reveal is running" flag:
/// a card built by a scroll while the reveal is still sweeping gets its own pop either way,
/// and a global mode would compose it on the reveal's curve.
#[derive(Clone, Copy)]
pub struct Entrance {
    pub start: std::time::Instant,
    pub kind: EntranceKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EntranceKind {
    /// A card built into a grid already on screen: a short scale-up.
    Pop,
    /// The post-spinner reveal: a longer fade with no scale at all, staggered along the
    /// grid's diagonal so the screen arrives as one sweep rather than as N separate pops.
    Reveal,
}

impl Entrance {
    pub fn pop(start: std::time::Instant) -> Self {
        Self {
            start,
            kind: EntranceKind::Pop,
        }
    }

    pub fn reveal(start: std::time::Instant) -> Self {
        Self {
            start,
            kind: EntranceKind::Reveal,
        }
    }

    /// How far this arrival has got at `now`, and the shrink its pop-in rides. Off a clock
    /// the caller already read, rather than one `Instant::now()` per card per curve.
    ///
    /// The two arrivals differ only in curve, duration and shrink; smoothstep over the
    /// reveal's longer fade because on a cubic ease-out a fade is near-opaque a sixth of the
    /// way through, which lands as the pop it is meant to replace.
    pub fn progress(self, now: std::time::Instant) -> (f32, f32) {
        let clock = Some(self.start);
        let dur = self.kind.duration();
        let frac = match self.kind {
            EntranceKind::Pop => crate::ui::animation::anim_frac_at(clock, dur, now),
            EntranceKind::Reveal => crate::ui::animation::anim_frac_smooth_at(clock, dur, now),
        };
        (frac, self.kind.shrink())
    }

    /// When this arrival is over — what the redraw loop's deadline has to cover.
    pub fn end(self) -> std::time::Instant {
        self.start + self.kind.duration()
    }
}

impl EntranceKind {
    pub fn duration(self) -> std::time::Duration {
        match self {
            Self::Pop => crate::app::CARD_POP,
            Self::Reveal => crate::app::CARD_REVEAL_FADE,
        }
    }

    /// How far the card is scaled down at the start of this arrival. The reveal is a flat
    /// cross-fade: a diagonal of cards each scaling on its own clock reads as noise, where
    /// the same diagonal fading reads as one diffused sweep.
    pub fn shrink(self) -> f32 {
        match self {
            Self::Pop => crate::app::CARD_POP_SHRINK,
            Self::Reveal => 0.0,
        }
    }
}

/// Hand-written rather than derived: a derived `Default` would start `next` at 0, handing the
/// first card a `TileId` already taken by one of the fixed tiles above.
impl Default for CardIds {
    fn default() -> Self {
        Self {
            slots: std::collections::HashMap::new(),
            free: Vec::new(),
            next: CARD_BASE,
        }
    }
}

impl CardIds {
    /// `pin_id`'s tile, assigning a slot if it has none.
    pub fn id(&mut self, pin_id: &str) -> TileId {
        if let Some(slot) = self.slots.get(pin_id) {
            return slot.id;
        }
        let id = self.free.pop().unwrap_or_else(|| {
            let id = TileId(self.next);
            self.next += 1;
            id
        });
        self.slots.insert(pin_id.to_string(), CardSlot { id, pop: None });
        id
    }

    /// `pin_id`'s tile if it already has one. Read-only, for the paint path.
    pub fn get(&self, pin_id: &str) -> Option<TileId> {
        self.slots.get(pin_id).map(|slot| slot.id)
    }

    /// `pin_id`'s tile *and* pop clock in one lookup — what the per-card composite path
    /// wants, and the reason the two live together.
    pub fn slot(&self, pin_id: &str) -> Option<CardSlot> {
        self.slots.get(pin_id).copied()
    }

    /// Starts `pin_id`'s arrival, reporting whether it took — a card with no slot is not on
    /// screen, and gets its clock from [`id`](Self::id) when it is built.
    pub fn arm(&mut self, pin_id: &str, entrance: Entrance) -> bool {
        match self.slots.get_mut(pin_id) {
            Some(slot) => {
                slot.pop = Some(entrance);
                true
            }
            None => false,
        }
    }

    /// [`arm`](Self::arm) for a card that may already be arriving — the reveal, which must
    /// not restart an arrival already under way.
    pub fn arm_if_idle(&mut self, pin_id: &str, entrance: Entrance) -> bool {
        match self.slots.get_mut(pin_id) {
            Some(slot) if slot.pop.is_none() => {
                slot.pop = Some(entrance);
                true
            }
            _ => false,
        }
    }

    /// Releases `pin_id`'s slot for reuse, returning the tile to drop.
    pub fn release(&mut self, pin_id: &str) -> Option<TileId> {
        let slot = self.slots.remove(pin_id)?;
        self.free.push(slot.id);
        Some(slot.id)
    }

    /// Releases every slot, returning the tiles to drop — a fresh library.
    pub fn release_all(&mut self) -> Vec<TileId> {
        let ids: Vec<TileId> = self.slots.values().map(|slot| slot.id).collect();
        self.slots.clear();
        self.free.extend(ids.iter().copied());
        ids
    }

    /// Every resident card, as `(pin id, tile)` — what the eviction pass walks, so it can
    /// test the tile it already holds instead of re-hashing the id string.
    pub fn entries(&self) -> impl Iterator<Item = (&str, TileId)> {
        self.slots.iter().map(|(id, slot)| (id.as_str(), slot.id))
    }
}
