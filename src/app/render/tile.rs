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
/// are a tile each (see [`settings_row`]), so one changed value repaints one row.
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

/// First id of the settings-row band — one slot per on-screen row of the open settings
/// list (see [`settings_row`]). Fixed rather than interned: the list is short, its rows are
/// addressed by position, and every screen that uses it shows one list at a time.
const SETTINGS_ROW_BASE: u32 = 32;
/// Slots in that band. `menu::GLOBAL_ROWS` and its sub-pages are all far under this;
/// [`settings_row`] refuses anything past it rather than colliding with the spinner band.
pub const SETTINGS_ROW_SLOTS: usize = 32;

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

/// The tile for on-screen settings row `index`, or `None` past the band — a list longer
/// than [`SETTINGS_ROW_SLOTS`] draws its tail unbaked rather than reaching into the
/// spinner's ids.
pub fn settings_row(index: usize) -> Option<TileId> {
    (index < SETTINGS_ROW_SLOTS).then(|| TileId(SETTINGS_ROW_BASE + index as u32))
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

/// What one resident card holds: its tile, and when its zoom-in started.
///
/// The pop clock lives here rather than in a second map keyed by the same string, because
/// `compose_grid` asks for both for every visible card on every frame — two hashes of one
/// game id, where the card is resident or neither answer exists.
#[derive(Clone, Copy)]
pub struct CardSlot {
    pub id: TileId,
    /// `None` for a card that is not animating; see `App::card_pop_frac`.
    pub pop: Option<std::time::Instant>,
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

    /// Starts `pin_id`'s zoom at `at`, reporting whether it took — a card with no slot is
    /// not on screen, and gets its clock from [`id`](Self::id) when it is built.
    pub fn arm_pop(&mut self, pin_id: &str, at: std::time::Instant) -> bool {
        match self.slots.get_mut(pin_id) {
            Some(slot) => {
                slot.pop = Some(at);
                true
            }
            None => false,
        }
    }

    /// [`arm_pop`](Self::arm_pop) for a card that may already be popping — the reveal, which
    /// must not restart a zoom already under way.
    pub fn arm_pop_if_idle(&mut self, pin_id: &str, at: std::time::Instant) -> bool {
        match self.slots.get_mut(pin_id) {
            Some(slot) if slot.pop.is_none() => {
                slot.pop = Some(at);
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

    /// Every pin id with a slot.
    pub fn pin_ids(&self) -> impl Iterator<Item = &str> {
        self.slots.keys().map(String::as_str)
    }

    /// Every resident card, as `(pin id, tile)` — what the eviction pass walks, so it can
    /// test the tile it already holds instead of re-hashing the id string.
    pub fn entries(&self) -> impl Iterator<Item = (&str, TileId)> {
        self.slots.iter().map(|(id, slot)| (id.as_str(), slot.id))
    }
}
