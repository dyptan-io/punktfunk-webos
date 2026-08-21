//! The "move this card to…" modal — presentation: its rows and its shell. Logic lives in
//! `app::state::collections`.
//!
//! A scrolling row list rather than a plain one: a host may hold every collection
//! `MAX_COLLECTIONS` allows plus the dynamic Library entry, which is a card taller than the
//! screen if baked into a single tile (see `view::scrolllist`).
use crate::app::view::{icons, scrolllist};
use crate::core::model::KnownHost;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) const TITLE: &str = "Move to";

/// One row per entry in grid order, Library included. `holding` is the collection the card
/// being moved is in right now (`None` for Library, its implicit home) — that row wears the
/// mark dot, so the list opens saying where the card already is instead of leaving the user
/// to work it out.
pub(crate) fn rows(host: &KnownHost, holding: Option<usize>) -> Vec<FocusRow> {
    let library = host.library_index();
    host.collections()
        .iter()
        .enumerate()
        .map(|(i, collection)| {
            let count = if collection.dynamic {
                // Library's members are whatever no one else claims, so its count is not in
                // the vector — and saying "0 games" of it would be a lie.
                None
            } else {
                Some(collection.games.len())
            };
            let row = FocusRow::action_with_value(icons::ICON_FOLDER, collection.name.clone(), count_label(count));
            let row = match count {
                // An empty collection is hidden in the grid, which reads as a vanished one
                // unless the row that still lists it says so.
                Some(0) => row.with_subtext(ui::widgets::RowSubtext::hint("Hidden until you add a game")),
                _ => row,
            };
            if holding == Some(i) || (holding.is_none() && library == Some(i)) {
                row.marked(ui::theme::palette().accent)
            } else {
                row
            }
        })
        .collect()
}

fn count_label(count: Option<usize>) -> String {
    match count {
        Some(1) => "1 game".to_string(),
        Some(n) => format!("{n} games"),
        None => String::new(),
    }
}

/// The modal as a [`ModalScreen`] — the shell only; its rows are their own tiles.
pub(crate) struct Modal<'a> {
    pub rows: usize,
    /// The card being moved, named after the heading so the list says what it acts on.
    pub card: Option<&'a str>,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, _fonts: &ui::text::Fonts) -> Rect {
        scrolllist::layout(self.rows, screen_w, screen_h).0
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        scrolllist::render(c, self.rows, TITLE, self.card, hover_close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{new_host_collections, DESKTOP_PIN_ID, LIBRARY_COLLECTION};

    fn host() -> KnownHost {
        let mut h = KnownHost::default();
        h.set_collections(new_host_collections());
        h
    }

    #[test]
    fn the_row_holding_the_card_is_the_marked_one() {
        let host = host();
        let pinned = rows(&host, host.collection_of(DESKTOP_PIN_ID));
        assert!(pinned[0].mark.is_some(), "Desktop starts in Pinned");
        assert!(pinned[1].mark.is_none());
        // A card no collection claims is Library's, so Library's row wears the mark.
        let stranger = rows(&host, host.collection_of("steam:1"));
        assert!(stranger[0].mark.is_none());
        assert!(stranger[1].mark.is_some());
    }

    #[test]
    fn library_states_no_count_and_an_empty_collection_says_it_is_hidden() {
        let mut host = host();
        host.add_collection("Racing").expect("under the limit");
        let rows = rows(&host, None);
        assert_eq!(rows[0].value, "1 game");
        assert_eq!(rows[1].label, LIBRARY_COLLECTION);
        assert!(rows[1].value.is_empty(), "Library's members are not in the vector");
        assert!(
            rows[2].subtext.is_some(),
            "an empty collection says why it is not in the grid"
        );
    }
}
