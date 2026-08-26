//! The game grid: its layout vocabulary, its tuning, and the state the render path keeps for it.
//!
//! [`GridLayout`] is the shape a given column count gives the library's [`Group`]s — pure
//! arithmetic over a bounded run list, so the pointer path and the focus map can both ask it
//! without a rasterizer. [`GridState`] is what `App` holds across frames: which tile each card
//! owns, the pop clocks, the eased scroll, and the dirty flags the event side sets for the
//! render side.
//!
//! Pixel geometry (card rects, the visible band) is `app::view::home`; navigation is
//! `app::state::home`; the groups themselves are built in `app::library`.
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::app::render::tile;
use crate::app::spinner::GridReveal;
use crate::app::view::home::{SECTION_GAP, SECTION_HEADING_H};
use crate::core::model::GameEntry;

/// Rows beyond viewport kept rasterized (prevents scroll stalls).
pub(crate) const CARD_PREFETCH_ROWS: i32 = 2;
/// Rows beyond which tiles are dropped. Hysteresis prevents eviction oscillation.
pub(crate) const CARD_KEEP_ROWS: i32 = 5;
/// Rasterization time one frame will spend on card tiles before deferring the rest to the
/// next. Checked *after* each card, so one is always built however slow the device — the
/// window fills at whatever rate the hardware allows instead of at a rate picked for the
/// slowest one. A fixed count of 1 (what this was) meant a 5x4 viewport took twenty frames
/// to fill everywhere, which on a fast host is a third of a second of blank cards for no
/// reason; on armv7 softfloat, where one card's text rasterization alone can cost most of a
/// tick, this degrades back to exactly that one card.
pub(crate) const CARD_BUILD_BUDGET: Duration = Duration::from_millis(6);
/// Hard ceiling on cards per frame regardless of the clock: a host fast enough never to trip
/// [`CARD_BUILD_BUDGET`] must still not rasterize *and upload* an unbounded batch in one tick,
/// since the upload is charged to a later stage that the budget above cannot see.
pub(crate) const CARD_BUILD_BURST: usize = 8;

/// Upper bound on grid sections: [`MAX_COLLECTIONS`](crate::core::model::MAX_COLLECTIONS)
/// plus the dynamic Library entry. Every geometry query below scans the group list, so this
/// is what makes them O(1) in the library's size.
pub(crate) const MAX_GROUPS: usize = crate::core::model::MAX_COLLECTIONS + 1;

/// One grid section: a collection's cards, laid out as a run of whole rows. Built once per
/// change by [`Library::regroup`](crate::app::library::Library::regroup) and read by every
/// geometry path; deliberately column-independent, so a width change needs no rebuild.
pub(crate) struct Group {
    /// The collection's name, as the section heading draws it.
    pub name: String,
    /// Cards in it.
    pub len: usize,
    /// Index into `Library::games` of this group's first game — the games are laid out
    /// group by group, so each group's are one contiguous run.
    pub games_start: usize,
}

/// A [`Group`] with the geometry `columns` gives it. Yielded by [`GridLayout::placed`]; never
/// stored, so a column change cannot leave a stale copy behind.
#[derive(Clone, Copy)]
pub(crate) struct Placed<'a> {
    pub group: &'a Group,
    /// First grid slot. Whole rows: a partial last row pads rather than letting the next
    /// group's first card share it.
    pub first_idx: usize,
    pub first_row: usize,
    pub rows: usize,
    /// Heading and gap pixels stacked above this group's first row.
    pub y_offset: i32,
}

impl Placed<'_> {
    /// The slots this group actually fills — [`Self::first_idx`] plus its cards, so the
    /// padding at the end of a partial last row is outside it.
    pub(crate) fn slots(&self) -> Range<usize> {
        self.first_idx..self.first_idx + self.group.len
    }

    fn rows_range(&self) -> Range<usize> {
        self.first_row..self.first_row + self.rows
    }
}

/// The grid's shape at one column count: the library's groups plus the arithmetic that turns
/// them into slots, rows and vertical offsets.
///
/// `Copy` and borrowing only the group list, so a caller can hold one and still mutate the
/// rest of `App` — which is why the accessors that build one live on
/// [`Library`](crate::app::library::Library) rather than on `App`.
#[derive(Clone, Copy)]
pub(crate) struct GridLayout<'a> {
    groups: &'a [Group],
    columns: usize,
}

impl<'a> GridLayout<'a> {
    pub(crate) fn new(groups: &'a [Group], columns: usize) -> Self {
        Self {
            groups,
            columns: columns.max(1),
        }
    }

    pub(crate) fn columns(&self) -> usize {
        self.columns
    }

    /// Every group with its geometry, in grid order. One pass of integer adds over at most
    /// [`MAX_GROUPS`] entries and no allocation — the whole reason the derived fields are not
    /// stored: nothing can go stale when the column count changes.
    pub(crate) fn placed(&self) -> impl Iterator<Item = Placed<'a>> + '_ {
        let columns = self.columns;
        let mut first_row = 0;
        let mut y_offset = 0;
        self.groups.iter().enumerate().map(move |(i, group)| {
            let rows = group.len.div_ceil(columns);
            // Every heading takes a line; every one after the first also takes the gap that
            // makes the block above it read as finished.
            y_offset += SECTION_HEADING_H + if i == 0 { 0 } else { SECTION_GAP };
            let placed = Placed {
                group,
                first_idx: first_row * columns,
                first_row,
                rows,
                y_offset,
            };
            first_row += rows;
            placed
        })
    }

    /// The group holding grid slot `idx`, skipping the padding after a partial last row.
    fn at_idx(&self, idx: usize) -> Option<Placed<'a>> {
        self.placed().find(|p| p.slots().contains(&idx))
    }

    /// Grid slots in total, padding between groups included but none after the last.
    pub(crate) fn len(&self) -> usize {
        self.placed().last().map_or(0, |p| p.slots().end)
    }

    pub(crate) fn rows(&self) -> usize {
        self.len().div_ceil(self.columns)
    }

    /// Section headings to draw: each group's name and the slot it sits above. Empty groups
    /// never reach here — [`Library::regroup`](crate::app::library::Library::regroup) drops
    /// them, since a zero-row section has nothing to hang a heading on.
    pub(crate) fn headings(&self) -> impl Iterator<Item = (usize, &'a Group)> + '_ {
        self.placed().map(|p| (p.first_idx, p.group))
    }

    /// The card at grid index `idx` — `None` for the padding after a group's partial last row.
    pub(crate) fn card_at(&self, games: &'a [GameEntry], idx: usize) -> Option<&'a GameEntry> {
        let placed = self.at_idx(idx)?;
        games.get(placed.group.games_start + (idx - placed.first_idx))
    }

    /// The pin id for whatever's at grid index `idx` — a `GameEntry::id`, `None` for the
    /// padding after a partial last row.
    pub(crate) fn pin_id_at(&self, games: &'a [GameEntry], idx: usize) -> Option<&'a str> {
        Some(self.card_at(games, idx)?.id.as_str())
    }

    pub(crate) fn idx_for_pin_id(&self, games: &[GameEntry], id: &str) -> Option<usize> {
        let pos = games.iter().position(|g| g.id == id)?;
        let placed = self
            .placed()
            .find(|p| (p.group.games_start..p.group.games_start + p.group.len).contains(&pos))?;
        Some(placed.first_idx + (pos - placed.group.games_start))
    }

    /// The grid's rows split into the bands that share a vertical offset — one per group, each
    /// uniformly spaced inside itself, so a band's visible rows are a closed-form range.
    pub(crate) fn row_bands(&self) -> impl Iterator<Item = (Range<usize>, i32)> + '_ {
        self.placed().map(|p| (p.rows_range(), p.y_offset))
    }

    /// Extra vertical offset carried by grid row `row`: whatever headings and gaps stack above
    /// it. Non-decreasing in `row`, which is what makes a card's y monotone in its index — see
    /// [`visible_cards`](crate::app::view::home::visible_cards).
    pub(crate) fn row_offset_at(&self, row: usize) -> i32 {
        self.placed()
            .take_while(|p| p.first_row <= row)
            .last()
            .map_or(0, |p| p.y_offset)
    }

    /// [`Self::row_offset_at`] for the row grid index `idx` sits in.
    pub(crate) fn row_offset(&self, idx: usize) -> i32 {
        self.row_offset_at(idx / self.columns)
    }

    /// What the sections add to the grid's total height — the offset its last row carries.
    pub(crate) fn total_extra(&self) -> i32 {
        self.placed().last().map_or(0, |p| p.y_offset)
    }
}

/// What `App` keeps for the grid between frames.
///
/// The dirty flags are the contract between the two halves: the event side sets them when it
/// changes something the render side cannot see, and `prepare_grid` clears them as it acts.
pub(crate) struct GridState {
    /// Which `TileId` each card's pin id holds (see [`tile::CardIds`]). Keyed by identity rather
    /// than by grid position because pinning a game reorders the grid, and keying by index would
    /// rebuild every tile after the moved one.
    pub card_ids: tile::CardIds,
    /// When the last-armed card pop finishes — the one comparison `App::tick_animations`
    /// needs, instead of walking every resident card's clock each frame to ask the same
    /// question.
    /// Never lowered by an eviction: a deadline that is merely late costs one extra frame
    /// of the redraw loop, where one that is early would freeze a zoom mid-way.
    pub card_pop_until: Option<Instant>,
    /// Current card size, derived from screen width in `App::advance_frame`. Screen geometry (the
    /// event side reads it to size cover-art requests), not a rasterized tile.
    pub card_size: (u32, u32),
    /// All card tiles are stale — the games list or the host changed. A fresh library load, so
    /// `prepare_grid` also stands the reveal spinner back up.
    pub dirty: bool,
    /// Individual card tiles stale (cover art arrived), by pin id — cheaper than [`Self::dirty`]
    /// when the layout is unchanged.
    pub cards_dirty: Vec<String>,
    /// Card tiles still waiting to be rasterized inside the prefetch window. Keeps the main loop
    /// ticking until the window is filled — without it the redraw-on-change loop would go idle
    /// mid-build and leave blank cards on screen.
    pub tiles_pending: bool,
    /// Scroll offset actually rendered this frame (px; 0 = row 0 at `view::home::GRID_TOP_Y`) —
    /// eases toward [`Self::scroll_target`] each tick.
    pub scroll: i32,
    pub scroll_target: i32,
    /// Where the grid is re-entered from the sidebar. Only ever consulted through the focus map,
    /// which drops it when it no longer names a real card, so reorders and library reloads need no
    /// invalidation of their own.
    pub focus_last: usize,
    /// Whether the initial build for the current library has finished, and the spinner shown until
    /// it has.
    pub reveal: GridReveal,
    /// `prepare_grid`'s per-frame working lists, kept across frames and cleared on entry.
    /// All three are window-bounded, so this is not a scaling fix — it is the armv7 softfloat
    /// allocator not being asked for three fresh collections on every frame of every scroll.
    pub scratch: GridScratch,
    /// Card pixmaps freed by eviction, held for the next card built this frame. A scroll
    /// evicts and builds in the same frame (see `prepare_grid`), and every card is exactly
    /// [`Self::card_size`], so a recycled buffer never needs resizing — at 1080p each one is
    /// ~360KB that would otherwise be freed and immediately reallocated.
    pub free_cards: Vec<crate::ui::Painter>,
}

/// The lists `prepare_grid` refills each frame. Indices and ids only — nothing here borrows
/// `games`, since it lives on `App` alongside it.
#[derive(Default)]
pub(crate) struct GridScratch {
    /// Tiles inside the keep window, sorted, for the eviction test.
    pub keep: Vec<crate::ui::render::TileId>,
    /// Pin ids evicted this frame — owned, because releasing them mutates the map they
    /// were read from.
    pub dropped: Vec<String>,
    /// Build candidates whose cover art has already arrived, and those still waiting.
    pub ready: Vec<usize>,
    pub waiting: Vec<usize>,
}

impl GridState {
    /// Starts (or restarts) `pin_id`'s zoom at `at`. A card with no tile is not on screen
    /// and has no pop to show; it gets its clock when `prepare_grid` builds it.
    pub fn arm_card_pop(&mut self, pin_id: &str, at: Instant) {
        if self.card_ids.arm_pop(pin_id, at) {
            self.extend_pop_deadline(at);
        }
    }

    /// [`arm_card_pop`](Self::arm_card_pop) for a card that may already be popping — a
    /// reveal, which must not restart a zoom already under way.
    pub fn arm_card_pop_if_idle(&mut self, pin_id: &str, at: Instant) {
        if self.card_ids.arm_pop_if_idle(pin_id, at) {
            self.extend_pop_deadline(at);
        }
    }

    fn extend_pop_deadline(&mut self, at: Instant) {
        let end = at + crate::app::CARD_POP;
        if self.card_pop_until.is_none_or(|until| until < end) {
            self.card_pop_until = Some(end);
        }
    }

    /// Whether any card is still zooming in.
    pub fn card_pops_running(&self) -> bool {
        self.card_pop_until.is_some_and(|until| Instant::now() < until)
    }
}

/// Hand-written rather than derived, because two of these fields do not want their type's zero
/// value: the first library load is what fills the grid and has to read as stale to be built at
/// all, and nothing is waiting until a host is picked.
impl Default for GridState {
    fn default() -> Self {
        Self {
            card_ids: tile::CardIds::default(),
            card_pop_until: None,
            card_size: (0, 0),
            dirty: true,
            cards_dirty: Vec::new(),
            tiles_pending: false,
            scroll: 0,
            scroll_target: 0,
            focus_last: 0,
            reveal: GridReveal::revealed(),
            scratch: GridScratch::default(),
            free_cards: Vec::new(),
        }
    }
}
