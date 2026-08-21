//! The selected host's library: its games, their art, and the grid grouping read every frame.
//!
//! Grouped so the grid's `&self` geometry queries can borrow the library disjointly from the
//! `&mut self` paths that mutate it. Everything here is per-host state and is replaced wholesale
//! on a host switch (`App::clear_selected_host`).

use std::cmp::Reverse;
use std::collections::HashMap;

use tiny_skia::Pixmap;

use crate::app::grid::{GridCard, GridLayout, Group};
use crate::core::model::{Collection, KnownHost, DESKTOP_PIN_ID};
use crate::services::library::GameEntry;
use crate::services::recents::HostRecents;

#[derive(Default)]
pub(crate) struct Library {
    pub(crate) selected_host: Option<(String, u16)>,
    /// Every game, laid out group by group: each [`Group`]'s games are one contiguous run
    /// starting at its `games_start`. [`Self::regroup`] is what maintains that.
    pub(crate) games: Vec<GameEntry>,
    /// The grid's sections, in grid order. Empty collections are dropped here rather than
    /// rendered as a heading over nothing; the collections modal still lists them.
    ///
    /// Kept rather than derived on demand because `layout` is asked for a card rect on every
    /// frame and every pointer motion, and deriving it meant scanning `known_hosts` per call.
    /// Every path that changes a collection, the selected host or the library goes through
    /// [`Self::regroup`] (or drops the grid via [`Self::clear_groups`]).
    pub(crate) groups: Vec<Group>,
    /// Host answered library fetch (gates the Desktop card).
    pub(crate) games_loaded: bool,
    /// Cover art pixmaps by game id.
    pub(crate) art: HashMap<String, Pixmap>,
}

impl Library {
    /// The grid's shape at `columns` columns. `Copy` and borrowing only `groups`, so a caller
    /// can hold one and go on mutating the rest of `App` — the disjointness this module exists
    /// for.
    pub(crate) fn layout(&self, columns: usize) -> GridLayout<'_> {
        GridLayout::new(&self.groups, columns)
    }

    pub(crate) fn grid_len(&self, columns: usize) -> usize {
        self.layout(columns).len()
    }

    pub(crate) fn card_at(&self, idx: usize, columns: usize) -> Option<GridCard<'_>> {
        self.layout(columns).card_at(&self.games, idx)
    }

    pub(crate) fn pin_id_at(&self, idx: usize, columns: usize) -> Option<&str> {
        self.layout(columns).pin_id_at(&self.games, idx)
    }

    pub(crate) fn idx_for_pin_id(&self, id: &str, columns: usize) -> Option<usize> {
        self.layout(columns).idx_for_pin_id(&self.games, id)
    }

    /// Re-lays the library out over `host`'s collections: each collection's members in its own
    /// order, the dynamic Library entry taking whatever is left — sorted by `recents`, wherever
    /// in the order it sits.
    /// One pass over the games, so it costs the same whether one card moved or the whole
    /// library arrived.
    ///
    /// A member the host no longer lists just doesn't place — it is *not* dropped here, since
    /// this also runs while `games` is empty (an offline host). Dropping is
    /// `KnownHost::prune_games`' job.
    pub(crate) fn regroup(&mut self, host: &KnownHost, recents: &HostRecents) {
        let collections = host.collections();
        if collections.is_empty() {
            self.clear_groups();
            return;
        }
        // Taken by value so a game can only be placed once; whatever survives is Library's.
        let mut remaining: Vec<Option<GameEntry>> = std::mem::take(&mut self.games).into_iter().map(Some).collect();
        let by_id: HashMap<&str, usize> = remaining
            .iter()
            .enumerate()
            .filter_map(|(i, g)| g.as_ref().map(|g| (g.id.as_str(), i)))
            .collect();

        // Where each collection's games land, resolved before anything is moved out of
        // `remaining` — `by_id` borrows it, and the placement below consumes it.
        let placement: Vec<Vec<usize>> = collections
            .iter()
            .map(|c| {
                c.games
                    .iter()
                    .filter_map(|id| by_id.get(id.as_str()).copied())
                    .collect()
            })
            .collect();

        let desktop = self.games_loaded.then(|| desktop_group(host, collections));
        let mut games = Vec::with_capacity(remaining.len());
        let mut groups = Vec::with_capacity(collections.len());
        for (i, collection) in collections.iter().enumerate() {
            let games_start = games.len();
            if collection.dynamic {
                games.extend(remaining.iter_mut().filter_map(Option::take));
                // Library is ordered by play, not by the host's listing: most recent first,
                // the never-played after them. `sort_by_key` is stable, so those keep the
                // order the host gave — and a zero key sorts them last under `Reverse`.
                games[games_start..].sort_by_key(|g| Reverse(recents.get(&g.id).copied().unwrap_or(0)));
            } else {
                games.extend(placement[i].iter().filter_map(|&at| remaining[at].take()));
            }
            let has_desktop = desktop == Some(i);
            let len = games.len() - games_start + usize::from(has_desktop);
            if len == 0 {
                continue;
            }
            groups.push(Group {
                name: collection.name.clone(),
                len,
                games_start,
                desktop: has_desktop,
            });
        }
        self.games = games;
        self.groups = groups;
    }

    /// Exchanges two games in place, keeping the grid's order in step with the collection's
    /// without a regroup. Both are members of one collection, so they sit in one group's run
    /// and no group's bounds move — the cards trade slots and everything else keeps its own.
    /// Tiles are keyed by pin id, so each card carries its pixels across with it.
    pub(crate) fn swap_games(&mut self, a: &str, b: &str) {
        let (Some(i), Some(j)) = (self.position(a), self.position(b)) else {
            return;
        };
        self.games.swap(i, j);
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.games.iter().position(|g| g.id == id)
    }

    /// Forgets the grouping the grid is drawn from — for the paths that drop the library
    /// itself, where there is no host left to recompute it from.
    pub(crate) fn clear_groups(&mut self) {
        self.groups.clear();
    }
}

/// Which collection heads the Desktop card. It is a member like any other, so it follows
/// whatever collection names it, and falls back to the dynamic entry when none does.
fn desktop_group(host: &KnownHost, collections: &[Collection]) -> usize {
    host.collection_of(DESKTOP_PIN_ID)
        .or_else(|| collections.iter().position(|c| c.dynamic))
        .unwrap_or(0)
}
