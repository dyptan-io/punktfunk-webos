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
use crate::services::store;

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

/// Grid card: Desktop or game.
pub(crate) enum GridCard<'a> {
    Desktop,
    Game(&'a GameEntry),
}

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
    /// Cards in it: its games, plus the Desktop card when [`Self::desktop`].
    pub len: usize,
    /// Index into `Library::games` of this group's first game — the games are laid out
    /// group by group, so each group's are one contiguous run.
    pub games_start: usize,
    /// Slot offset of `DESKTOP_PIN_ID` inside this group, when the group holds it. The
    /// Desktop card is an ordinary member: it sits wherever the collection lists it.
    pub desktop: Option<usize>,
}

impl Group {
    /// Games in this group — [`Self::len`] without the Desktop card.
    pub(crate) fn games(&self) -> usize {
        self.len - usize::from(self.desktop.is_some())
    }
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

    pub(crate) fn card_at(&self, games: &'a [GameEntry], idx: usize) -> Option<GridCard<'a>> {
        let placed = self.at_idx(idx)?;
        let pos = idx - placed.first_idx;
        if placed.group.desktop == Some(pos) {
            return Some(GridCard::Desktop);
        }
        let pos = pos - usize::from(placed.group.desktop.is_some_and(|d| d < pos));
        games.get(placed.group.games_start + pos).map(GridCard::Game)
    }

    /// Like [`Self::card_at`] but only games (not Desktop or padding).
    pub(crate) fn game_at(&self, games: &'a [GameEntry], idx: usize) -> Option<&'a GameEntry> {
        match self.card_at(games, idx)? {
            GridCard::Game(g) => Some(g),
            GridCard::Desktop => None,
        }
    }

    /// The pin id for whatever's at grid index `idx` — a `GameEntry::id`, or
    /// `store::DESKTOP_PIN_ID` for "Desktop" — `None` for the padding after a partial last
    /// row. The one place this mapping is spelled out; every caller (`App::pin_id_at_grid_idx`,
    /// tile build/evict, `draw_list`) delegates here instead of matching `card_at` itself.
    pub(crate) fn pin_id_at(&self, games: &'a [GameEntry], idx: usize) -> Option<&'a str> {
        match self.card_at(games, idx)? {
            GridCard::Desktop => Some(store::DESKTOP_PIN_ID),
            GridCard::Game(g) => Some(g.id.as_str()),
        }
    }

    pub(crate) fn idx_for_pin_id(&self, games: &[GameEntry], id: &str) -> Option<usize> {
        if id == store::DESKTOP_PIN_ID {
            return self.placed().find_map(|p| Some(p.first_idx + p.group.desktop?));
        }
        let pos = games.iter().position(|g| g.id == id)?;
        let placed = self
            .placed()
            .find(|p| (p.group.games_start..p.group.games_start + p.group.games()).contains(&pos))?;
        let off = pos - placed.group.games_start;
        Some(placed.first_idx + off + usize::from(placed.group.desktop.is_some_and(|d| d <= off)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Artwork, GameEntry};

    fn games(n: usize) -> Vec<GameEntry> {
        (0..n)
            .map(|i| GameEntry {
                id: format!("steam:{i}"),
                title: format!("Game {i}"),
                art: Artwork::default(),
            })
            .collect()
    }

    /// One arrangement of the grid: the groups, the column count and the library behind them.
    struct Case {
        groups: Vec<Group>,
        columns: usize,
        games: Vec<GameEntry>,
    }

    impl Case {
        fn layout(&self) -> GridLayout<'_> {
            GridLayout::new(&self.groups, self.columns)
        }

        fn pin_ids(&self) -> Vec<Option<String>> {
            let layout = self.layout();
            (0..layout.len())
                .map(|i| layout.pin_id_at(&self.games, i).map(str::to_owned))
                .collect()
        }

        fn label(&self) -> String {
            let shape: Vec<(usize, Option<usize>)> = self.groups.iter().map(|g| (g.len, g.desktop)).collect();
            format!("cols {} games {} groups {shape:?}", self.columns, self.games.len())
        }
    }

    /// Builds the groups a split of `games` into runs of the given sizes implies, with the
    /// Desktop card heading group `desktop` (`None` for an unloaded library, which has no
    /// Desktop card at all). Empty groups are dropped, exactly as `Library::regroup` drops
    /// them.
    fn case(columns: usize, splits: &[usize], desktop: Option<usize>) -> Case {
        let total: usize = splits.iter().sum();
        let mut groups = Vec::new();
        let mut games_start = 0;
        for (i, &len) in splits.iter().enumerate() {
            let has_desktop = desktop == Some(i);
            if len + usize::from(has_desktop) > 0 {
                groups.push(Group {
                    name: format!("Group {i}"),
                    len: len + usize::from(has_desktop),
                    games_start,
                    desktop: has_desktop.then_some(0),
                });
            }
            games_start += len;
        }
        Case {
            groups,
            columns,
            games: games(total),
        }
    }

    /// Every shape the grid can take: splits either side of a row boundary, the Desktop card
    /// in the first, a middle and the last group, empty groups interleaved, and an unloaded
    /// library with no cards at all.
    fn arrangements() -> Vec<Case> {
        let splits: &[&[usize]] = &[
            &[],
            &[0],
            &[1],
            &[7],
            &[1, 1],
            &[3, 4],
            &[5, 7],
            &[6, 6],
            &[0, 5],
            &[5, 0],
            &[2, 0, 9],
            &[1, 3, 5, 7],
            &[4, 4, 4, 4, 4],
        ];
        let mut out = Vec::new();
        for &columns in &[1usize, 3, 5] {
            for split in splits {
                for desktop in std::iter::once(None).chain((0..split.len()).map(Some)) {
                    out.push(case(columns, split, desktop));
                }
            }
        }
        out
    }

    #[test]
    fn pin_id_round_trips_through_idx_for_every_arrangement() {
        for case in arrangements() {
            let layout = case.layout();
            for idx in 0..layout.len() {
                let Some(id) = layout.pin_id_at(&case.games, idx) else {
                    continue; // padding after a partial last row
                };
                let id = id.to_owned();
                assert_eq!(
                    layout.idx_for_pin_id(&case.games, &id),
                    Some(idx),
                    "idx {idx} id {id} — {}",
                    case.label()
                );
            }
        }
    }

    #[test]
    fn every_card_appears_exactly_once() {
        for case in arrangements() {
            let mut seen: Vec<String> = case.pin_ids().into_iter().flatten().collect();
            let total = seen.len();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), total, "duplicate card — {}", case.label());
            let mut expected: Vec<String> = case.games.iter().map(|g| g.id.clone()).collect();
            if case.groups.iter().any(|g| g.desktop.is_some()) {
                expected.push(store::DESKTOP_PIN_ID.to_owned());
            }
            expected.sort();
            assert_eq!(seen, expected, "{}", case.label());
        }
    }

    #[test]
    fn holes_are_only_the_padding_after_a_partial_group() {
        for case in arrangements() {
            let layout = case.layout();
            let holes: Vec<usize> = case
                .pin_ids()
                .iter()
                .enumerate()
                .filter_map(|(i, id)| id.is_none().then_some(i))
                .collect();
            // A hole is always past its group's last card and inside that group's last row.
            for hole in holes {
                let group = layout
                    .placed()
                    .find(|p| (p.first_idx..p.first_idx + p.rows * case.columns).contains(&hole))
                    .unwrap_or_else(|| panic!("hole {hole} outside every group — {}", case.label()));
                assert!(
                    hole >= group.first_idx + group.group.len,
                    "hole {hole} inside a group's cards — {}",
                    case.label()
                );
            }
        }
    }

    #[test]
    fn a_group_starts_on_its_own_row() {
        for case in arrangements() {
            for placed in case.layout().placed() {
                assert_eq!(placed.first_idx % case.columns, 0, "{}", case.label());
            }
        }
    }

    #[test]
    fn desktop_sits_in_whichever_group_holds_it() {
        let case = case(5, &[2, 3], Some(1));
        let layout = case.layout();
        // Group 0's two games fill slots 0-1, so group 1 starts on the next row.
        assert_eq!(layout.idx_for_pin_id(&case.games, store::DESKTOP_PIN_ID), Some(5));
        assert_eq!(layout.pin_id_at(&case.games, 5), Some(store::DESKTOP_PIN_ID));
        assert_eq!(layout.pin_id_at(&case.games, 6), Some("steam:2"));
    }

    #[test]
    fn desktop_can_sit_anywhere_in_its_group() {
        let mut case = case(5, &[2, 3], Some(1));
        // Third slot of group 1: one game ahead of it, two behind.
        case.groups[1].desktop = Some(1);
        let layout = case.layout();
        assert_eq!(layout.pin_id_at(&case.games, 5), Some("steam:2"));
        assert_eq!(layout.pin_id_at(&case.games, 6), Some(store::DESKTOP_PIN_ID));
        assert_eq!(layout.pin_id_at(&case.games, 7), Some("steam:3"));
        assert_eq!(layout.idx_for_pin_id(&case.games, store::DESKTOP_PIN_ID), Some(6));
        assert_eq!(layout.idx_for_pin_id(&case.games, "steam:2"), Some(5));
        assert_eq!(layout.idx_for_pin_id(&case.games, "steam:3"), Some(7));
    }

    #[test]
    fn an_unloaded_library_has_no_cards_and_no_headings() {
        let layout = GridLayout::new(&[], 5);
        assert_eq!(layout.len(), 0);
        assert_eq!(layout.rows(), 0);
        assert_eq!(layout.total_extra(), 0);
        assert!(layout.pin_id_at(&[], 0).is_none());
        assert_eq!(layout.headings().count(), 0);
    }

    #[test]
    fn each_group_carries_one_more_heading_and_one_more_gap() {
        let case = case(5, &[5, 5, 5], None);
        let offsets: Vec<i32> = case.layout().placed().map(|p| p.y_offset).collect();
        assert_eq!(
            offsets,
            vec![
                SECTION_HEADING_H,
                2 * SECTION_HEADING_H + SECTION_GAP,
                3 * SECTION_HEADING_H + 2 * SECTION_GAP,
            ]
        );
        assert_eq!(case.layout().total_extra(), *offsets.last().expect("three groups"));
    }

    #[test]
    fn row_offsets_never_decrease() {
        for case in arrangements() {
            let layout = case.layout();
            let mut last = 0;
            for row in 0..layout.rows() {
                let offset = layout.row_offset_at(row);
                assert!(offset >= last, "row {row} went backwards — {}", case.label());
                last = offset;
            }
        }
    }

    #[test]
    fn game_at_skips_the_desktop_card() {
        let case = case(5, &[3], Some(0));
        let layout = case.layout();
        assert!(layout.game_at(&case.games, 0).is_none());
        assert_eq!(layout.game_at(&case.games, 1).map(|g| g.id.as_str()), Some("steam:0"));
    }
}
