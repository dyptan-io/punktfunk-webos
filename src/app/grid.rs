//! The game grid: its layout vocabulary, its tuning, and the state the render path keeps for it.
//!
//! [`GridLayout`] is the pinned/library shape a given column count implies — pure arithmetic over
//! the games list, so the pointer path and the focus map can both ask it without a rasterizer.
//! [`GridState`] is what `App` holds across frames: which tile each card owns, the pop clocks, the
//! eased scroll, and the dirty flags the event side sets for the render side.
//!
//! Pixel geometry (card rects, the visible band) is `app::view::home`; navigation is
//! `app::state::home`.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::app::render::tile;
use crate::app::spinner::GridReveal;
use crate::app::view;
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

/// Grid card: Desktop or game (both pinnable).
pub(crate) enum GridCard<'a> {
    Desktop,
    Game(&'a GameEntry),
}

/// Grid layout shape: pinned block (owns whole rows) + rest section (padding-aware).
#[derive(Clone, Copy)]
pub(crate) struct GridLayout {
    pub(crate) pinned_count: usize,
    pub(crate) desktop_pinned: bool,
    pub(crate) desktop_in_rest: bool,
    pub(crate) front_count: usize,
    pub(crate) pinned_rows: usize,
    pub(crate) unpinned_start: usize,
}

impl GridLayout {
    /// The vertical section shape this layout implies: the pinned block's row count, and one
    /// heading per section that actually has cards — so neither names an empty block, and the
    /// gap between them only exists when both do.
    pub(crate) fn sections(&self, games: usize) -> view::home::GridSections {
        view::home::GridSections {
            pinned_rows: self.pinned_rows,
            pinned_heading: self.pinned_rows > 0,
            library_heading: self.len(games) > self.unpinned_start,
        }
    }

    pub(crate) fn len(&self, games: usize) -> usize {
        self.unpinned_start + usize::from(self.desktop_in_rest) + games.saturating_sub(self.pinned_count)
    }

    pub(crate) fn card_at<'a>(&self, games: &'a [GameEntry], idx: usize) -> Option<GridCard<'a>> {
        if idx < self.front_count {
            if self.desktop_pinned {
                return if idx == 0 {
                    Some(GridCard::Desktop)
                } else {
                    games.get(idx - 1).map(GridCard::Game)
                };
            }
            return games.get(idx).map(GridCard::Game);
        }
        let rest_pos = idx.checked_sub(self.unpinned_start)?;
        if self.desktop_in_rest {
            return if rest_pos == 0 {
                Some(GridCard::Desktop)
            } else {
                games.get(self.pinned_count + rest_pos - 1).map(GridCard::Game)
            };
        }
        games.get(self.pinned_count + rest_pos).map(GridCard::Game)
    }

    /// Like `card_at` but only games (not Desktop or padding).
    pub(crate) fn game_at<'a>(&self, games: &'a [GameEntry], idx: usize) -> Option<&'a GameEntry> {
        match self.card_at(games, idx)? {
            GridCard::Game(g) => Some(g),
            GridCard::Desktop => None,
        }
    }

    /// The pin id for whatever's at grid index `idx` — a `GameEntry::id`, or
    /// `store::DESKTOP_PIN_ID` for "Desktop" — `None` for the padding after a
    /// partial pinned row. The one place this mapping is spelled out; every
    /// caller (`App::pin_id_at_grid_idx`, tile build/evict, `draw_list`)
    /// delegates here instead of matching `card_at` itself.
    pub(crate) fn pin_id_at<'a>(&self, games: &'a [GameEntry], idx: usize) -> Option<&'a str> {
        match self.card_at(games, idx)? {
            GridCard::Desktop => Some(store::DESKTOP_PIN_ID),
            GridCard::Game(g) => Some(g.id.as_str()),
        }
    }

    pub(crate) fn idx_for_pin_id(&self, games: &[GameEntry], id: &str) -> Option<usize> {
        if id == store::DESKTOP_PIN_ID {
            return Some(if self.desktop_pinned { 0 } else { self.unpinned_start });
        }
        let pos = games.iter().position(|g| g.id == id)?;
        Some(if pos < self.pinned_count {
            usize::from(self.desktop_pinned) + pos
        } else {
            self.unpinned_start + usize::from(self.desktop_in_rest) + (pos - self.pinned_count)
        })
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
    /// Per-card zoom-in start clock, keyed by pin id — set when a card first appears or reveals,
    /// read by `App::card_pop_frac`. Animation state (the event side re-arms it on reorder), kept
    /// off the tile cache so that cache is touched only by the render loop.
    ///
    /// Bounded by the scroll window: eviction drops an entry with its tile, and the reorder replay
    /// only re-arms cards that are actually resident. `App::tick_animations` scans this every
    /// frame, so an entry per game in the library would be a per-frame cost of library size.
    pub card_pop: HashMap<String, Instant>,
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
}

/// Hand-written rather than derived, because two of these fields do not want their type's zero
/// value: the first library load is what fills the grid and has to read as stale to be built at
/// all, and nothing is waiting until a host is picked.
impl Default for GridState {
    fn default() -> Self {
        Self {
            card_ids: tile::CardIds::default(),
            card_pop: HashMap::new(),
            card_size: (0, 0),
            dirty: true,
            cards_dirty: Vec::new(),
            tiles_pending: false,
            scroll: 0,
            scroll_target: 0,
            focus_last: 0,
            reveal: GridReveal::revealed(),
        }
    }
}
