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
        // Library takes what no collection claims — and a collection ordered *after* it is
        // still a claim. Without this, a card moved into a freshly added collection (which
        // `add_collection` pushes last) was swallowed by the dynamic run before its own
        // group was reached, and the group placed empty and was dropped.
        let mut claimed = vec![false; remaining.len()];
        for at in placement.iter().flatten() {
            claimed[*at] = true;
        }

        // Desktop is a member like any other, so its slot is where its collection lists it —
        // counting only the members that actually placed, since the group is drawn from those.
        let desktop = self.games_loaded.then(|| desktop_slot(host, collections, &by_id));
        let mut games = Vec::with_capacity(remaining.len());
        let mut groups = Vec::with_capacity(collections.len());
        for (i, collection) in collections.iter().enumerate() {
            let games_start = games.len();
            if collection.dynamic {
                games.extend(
                    remaining
                        .iter_mut()
                        .zip(&claimed)
                        .filter(|(_, claimed)| !**claimed)
                        .filter_map(|(g, _)| g.take()),
                );
                // Library is ordered by play, not by the host's listing: most recent first,
                // the never-played after them. `sort_by_key` is stable, so those keep the
                // order the host gave — and a zero key sorts them last under `Reverse`.
                games[games_start..].sort_by_key(|g| Reverse(recents.get(&g.id).copied().unwrap_or(0)));
            } else {
                games.extend(placement[i].iter().filter_map(|&at| remaining[at].take()));
            }
            let has_desktop = desktop.filter(|&(at, _)| at == i).map(|(_, slot)| slot);
            let len = games.len() - games_start + usize::from(has_desktop.is_some());
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
        if a == DESKTOP_PIN_ID || b == DESKTOP_PIN_ID {
            let other = if a == DESKTOP_PIN_ID { b } else { a };
            self.swap_desktop_with(other);
            return;
        }
        let (Some(i), Some(j)) = (self.position(a), self.position(b)) else {
            return;
        };
        self.games.swap(i, j);
    }

    /// The Desktop half of [`Self::swap_games`]. Desktop is not in `games`, so the two only
    /// trade slot offsets: the games keep their order among themselves, and the card that
    /// moves is the one the group's `desktop` offset names.
    fn swap_desktop_with(&mut self, other: &str) {
        let Some(pos) = self.position(other) else {
            return;
        };
        let Some(group) = self.groups.iter_mut().find(|g| g.desktop.is_some()) else {
            return;
        };
        let Some(off) = pos.checked_sub(group.games_start).filter(|off| *off < group.games()) else {
            return;
        };
        // `other` sits one past `desktop` when it is behind it, so its slot offset is the
        // one Desktop takes over.
        group.desktop = Some(off + usize::from(group.desktop.is_some_and(|d| d <= off)));
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

/// Which collection holds the Desktop card, and the slot it takes inside that group. It is a
/// member like any other, so it follows whatever collection names it and sits where that
/// collection lists it; an unnamed Desktop falls back to the head of the dynamic entry, whose
/// order is recency and not the user's to arrange.
fn desktop_slot(host: &KnownHost, collections: &[Collection], placed: &HashMap<&str, usize>) -> (usize, usize) {
    let Some(at) = host.collection_of(DESKTOP_PIN_ID) else {
        return (collections.iter().position(|c| c.dynamic).unwrap_or(0), 0);
    };
    let members = collections.get(at).map(|c| c.games.as_slice()).unwrap_or_default();
    let slot = members
        .iter()
        .take_while(|id| id.as_str() != DESKTOP_PIN_ID)
        .filter(|id| placed.contains_key(id.as_str()))
        .count();
    (at, slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{new_host_collections, Artwork, GameEntry};
    use crate::services::recents::HostRecents;

    fn library(n: usize) -> Library {
        Library {
            games: (0..n)
                .map(|i| GameEntry {
                    id: format!("steam:{i}"),
                    title: format!("Game {i}"),
                    art: Artwork::default(),
                })
                .collect(),
            games_loaded: true,
            ..Library::default()
        }
    }

    fn host() -> KnownHost {
        let mut h = KnownHost::default();
        h.set_collections(new_host_collections());
        h
    }

    /// The groups by name, and the ids each one draws in order.
    fn shape(lib: &Library) -> Vec<(String, Vec<&str>)> {
        let layout = lib.layout(4);
        layout
            .placed()
            .map(|p| {
                let ids = p.slots().filter_map(|i| layout.pin_id_at(&lib.games, i)).collect();
                (p.group.name.clone(), ids)
            })
            .collect()
    }

    #[test]
    fn a_collection_after_library_still_gets_its_cards() {
        let mut host = host();
        // `add_collection` pushes last, so this one is ordered *after* the dynamic entry.
        let added = host.add_collection("New").expect("under the limit");
        host.move_to("steam:1", Some(added));
        let mut lib = library(3);
        lib.regroup(&host, &HostRecents::default());
        assert_eq!(
            shape(&lib),
            vec![
                ("Pinned".to_string(), vec![DESKTOP_PIN_ID]),
                ("Library".to_string(), vec!["steam:0", "steam:2"]),
                ("New".to_string(), vec!["steam:1"]),
            ]
        );
    }

    #[test]
    fn desktop_sits_where_its_collection_lists_it() {
        let mut host = host();
        host.move_to("steam:0", Some(0));
        host.move_to("steam:1", Some(0));
        assert!(host.swap_within_collection(DESKTOP_PIN_ID, true));
        let mut lib = library(2);
        lib.regroup(&host, &HostRecents::default());
        assert_eq!(lib.groups[0].desktop, Some(1));
        assert_eq!(shape(&lib)[0].1, vec!["steam:0", DESKTOP_PIN_ID, "steam:1"]);
    }

    #[test]
    fn swapping_desktop_moves_only_its_slot() {
        let mut host = host();
        host.move_to("steam:0", Some(0));
        host.move_to("steam:1", Some(0));
        let mut lib = library(2);
        lib.regroup(&host, &HostRecents::default());
        lib.swap_games(DESKTOP_PIN_ID, "steam:0");
        assert_eq!(shape(&lib)[0].1, vec!["steam:0", DESKTOP_PIN_ID, "steam:1"]);
        lib.swap_games("steam:1", DESKTOP_PIN_ID);
        assert_eq!(shape(&lib)[0].1, vec!["steam:0", "steam:1", DESKTOP_PIN_ID]);
    }
}
