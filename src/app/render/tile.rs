//! This app's tile numbering: which [`TileId`] means what.
//!
//! `ui` treats a tile id as an opaque number (see [`TileId`]'s docs) — the enum that used
//! to name these lived in the library and made it unusable by anything else. The numbering
//! is here, dense and `Copy`, so a draw command carries four bytes instead of a `String`.
//!
//! Ids are assigned in three bands: the fixed singletons below, one slot per spinner frame,
//! and an interned band for grid cards (whose count is the library's, not a constant).
use crate::ui::render::TileId;

/// Unfocused sidebar.
pub const SIDEBAR: TileId = TileId(0);
/// Focused sidebar row.
pub const FOCUS_ROW: TileId = TileId(1);
/// Shared focus-ring glow (one card size at a time).
pub const RING: TileId = TileId(2);
/// Focused card's lit edge.
pub const CARD_OUTLINE: TileId = TileId(3);
/// Open modal shell (unfocused).
pub const MODAL: TileId = TileId(5);
/// Open modal focused widget (zoom-animated).
pub const MODAL_FOCUS: TileId = TileId(6);
/// Home status line.
pub const STATUS: TileId = TileId(9);
/// "No host selected" hint.
pub const NO_HOST: TileId = TileId(10);
/// Scrollable modal scroll indicator.
pub const SCROLL_INDICATOR: TileId = TileId(11);
/// About document (baked full-height, scrolling inside doesn't invalidate).
pub const SCROLL_CONTENT: TileId = TileId(12);
// 13 and 14 were the scroll-edge ramp tiles. The fade now ramps the content's own alpha
// (`compose::push_faded`), so there is no band tile to bake; the ids stay retired rather than
// reused, since the rest of the table is written out by number.
/// Connecting screen backdrop. One slot (only one launch in flight).
pub const HERO: TileId = TileId(15);
/// In-stream stats overlay.
pub const STATS_OVERLAY: TileId = TileId(16);
/// Transient toast.
pub const NOTIFICATION: TileId = TileId(17);
/// Log-tail overlay (menu and stream).
pub const LOG_OVERLAY: TileId = TileId(18);
/// Disconnect confirm dialog and focused button.
pub const DISCONNECT_DIALOG: TileId = TileId(19);
pub const DISCONNECT_FOCUS_BUTTON: TileId = TileId(20);
/// Focused card's title strip (wiped). One slot (only one card focused).
pub const CARD_TITLE: TileId = TileId(21);
/// Card drop shadow (shared, not baked into each).
pub const CARD_SHADOW: TileId = TileId(22);
/// Modal fading out (snapshot for cross-fade).
pub const MODAL_PREV: TileId = TileId(23);
/// Leaving modal's scrolled content (frozen).
pub const MODAL_PREV_CONTENT: TileId = TileId(24);
/// Focused card submenu panel (grown `CARD_TITLE`). Own slot to bake ahead.
pub const CARD_MENU: TileId = TileId(25);
/// Submenu panel rows (transparent, under selection band).
pub const CARD_MENU_ROWS: TileId = TileId(26);
/// Submenu panel title line (transparent, rides opening window top).
pub const CARD_MENU_TITLE: TileId = TileId(27);
/// Submenu panel selection band (one row tall).
pub const CARD_MENU_BAND: TileId = TileId(28);
/// Modal card drop shadow (nine-sliceable atlas, not baked).
pub const MODAL_SHADOW: TileId = TileId(29);
/// Smaller atlas for non-modal panels (dropdown popup).
pub const PANEL_SHADOW: TileId = TileId(30);
/// Hero dissolve mask (alpha ramp, subtractive erase).
pub const HERO_MASK: TileId = TileId(31);
/// Grid dissolve mask (background cover, alpha falls away as wave passes).
pub const GRID_REVEAL_MASK: TileId = TileId(32);

/// Row band base (one slot per on-screen row, fixed not interned).
const LIST_ROW_BASE: u32 = 33;
pub const LIST_ROW_SLOTS: usize = 32;
/// Section-heading band base (one slot per drawn section, versioned by text).
const SECTION_BASE: u32 = 65;
pub const SECTION_SLOTS: usize = crate::app::grid::MAX_GROUPS.next_power_of_two();
/// Spinner band base (one per frame, swap not upload).
const SPINNER_BASE: u32 = 97;
/// Grid-card band base (interned by pin id).
const CARD_BASE: u32 = 256;

/// Tile for on-screen list row (None past band).
pub fn list_row(index: usize) -> Option<TileId> {
    (index < LIST_ROW_SLOTS).then(|| TileId(LIST_ROW_BASE + index as u32))
}
/// Tile for grid section (None past band).
pub fn section(index: usize) -> Option<TileId> {
    (index < SECTION_SLOTS).then(|| TileId(SECTION_BASE + index as u32))
}
/// Tile for spinner frame.
pub fn spinner(idx: usize) -> TileId {
    TileId(SPINNER_BASE + idx as u32)
}
/// Spinner frame index from id (runtime recognizes for raw pixel uploads).
pub fn spinner_index(id: TileId) -> Option<usize> {
    (SPINNER_BASE..CARD_BASE)
        .contains(&id.0)
        .then(|| (id.0 - SPINNER_BASE) as usize)
}

/// Grid-card ids (interned by pin id, not position, so pinning doesn't rebuild tail).
/// Slots recycled on refresh (no id space growth).
pub struct CardIds {
    slots: std::collections::HashMap<String, CardSlot>,
    free: Vec<TileId>,
    next: u32,
}

/// Resident card tile and entrance. Both here to avoid double hash per frame.
#[derive(Clone, Copy)]
pub struct CardSlot {
    pub id: TileId,
    /// None if not animating.
    pub pop: Option<Entrance>,
}
/// Progress for possibly-non-animating slot: (1.0, 0.0) when not animating.
pub fn entrance_progress(entrance: Option<Entrance>, now: std::time::Instant) -> (f32, f32) {
    entrance.map_or((1.0, 0.0), |e| e.progress(now))
}

/// Card arrival: scale-up on grid. Not first appearance (that's `GridReveal` mask).
#[derive(Clone, Copy)]
pub struct Entrance {
    pub start: std::time::Instant,
}

impl Entrance {
    pub fn pop(start: std::time::Instant) -> Self {
        Self { start }
    }
    /// Arrival progress and pop shrink. Uses caller's clock (no per-card `now()`).
    pub fn progress(self, now: std::time::Instant) -> (f32, f32) {
        let frac = crate::ui::animation::anim_frac_at(Some(self.start), crate::app::CARD_POP, now);
        (frac, crate::app::CARD_POP_SHRINK)
    }
    /// When arrival finishes (redraw loop deadline).
    pub fn end(self) -> std::time::Instant {
        self.start + crate::app::CARD_POP
    }
}

/// Hand-written (derived would start next at 0, collision with fixed tiles).
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
    /// Get/assign `pin_id`'s tile and report if new (build path knows if re-rasterizing).
    pub fn id_new(&mut self, pin_id: &str) -> (TileId, bool) {
        if let Some(slot) = self.slots.get(pin_id) {
            return (slot.id, false);
        }
        let id = self.free.pop().unwrap_or_else(|| {
            let id = TileId(self.next);
            self.next += 1;
            id
        });
        self.slots.insert(pin_id.to_string(), CardSlot { id, pop: None });
        (id, true)
    }

    /// Get `pin_id`'s tile (read-only, paint path).
    pub fn get(&self, pin_id: &str) -> Option<TileId> {
        self.slots.get(pin_id).map(|slot| slot.id)
    }
    /// Get `pin_id`'s tile and pop clock (why they live together).
    pub fn slot(&self, pin_id: &str) -> Option<CardSlot> {
        self.slots.get(pin_id).copied()
    }
    /// Start `pin_id`'s arrival (false if no slot = not on screen yet).
    pub fn arm(&mut self, pin_id: &str, entrance: Entrance) -> bool {
        match self.slots.get_mut(pin_id) {
            Some(slot) => {
                slot.pop = Some(entrance);
                true
            }
            None => false,
        }
    }

    /// Release `pin_id`'s slot for reuse.
    pub fn release(&mut self, pin_id: &str) -> Option<TileId> {
        let slot = self.slots.remove(pin_id)?;
        self.free.push(slot.id);
        Some(slot.id)
    }
    /// Release all slots (fresh library).
    pub fn release_all(&mut self) -> Vec<TileId> {
        let ids: Vec<TileId> = self.slots.values().map(|slot| slot.id).collect();
        self.slots.clear();
        self.free.extend(ids.iter().copied());
        ids
    }
    /// Every resident card as (pin id, tile) (eviction pass tests without re-hash).
    pub fn entries(&self) -> impl Iterator<Item = (&str, TileId)> {
        self.slots.iter().map(|(id, slot)| (id.as_str(), slot.id))
    }
}
