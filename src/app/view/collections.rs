//! The "add this card to…" modal — presentation: its rows and its shell. Logic lives in
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

const TITLE: &str = "Add to";
const MOVE_TITLE: &str = "Move to";
/// What the list is doing, by whether a collection already holds the card: one that sits in
/// Library can only be gained by a collection, one that is held leaves its own behind. The
/// card menu's row ([`menu_row_label`]) opens this modal, so both read it from here and the
/// two can't drift apart.
pub(crate) fn heading(held: bool) -> &'static str {
    if held {
        MOVE_TITLE
    } else {
        TITLE
    }
}

/// [`heading`] as the card menu's row reads it — the same words, with the ellipsis that says
/// it opens something.
pub(crate) fn menu_row_label(held: bool) -> &'static str {
    if held {
        "Move to\u{2026}"
    } else {
        "Add to\u{2026}"
    }
}

pub(crate) const ADD_ROW: &str = "Add collection";
pub(crate) const REMOVE_TITLE: &str = "Remove collection?";
/// Replaces the card's name in the heading while a row is being dragged: the list is doing
/// something else for the moment, and the only slot that says so is already drawn per frame.
pub(crate) const DRAG_HINT: &str = "Up/Down to move, OK to drop";
pub(crate) const ADD_TITLE: &str = "New collection";
pub(crate) const RENAME_TITLE: &str = "Rename collection";

/// The name dialog's subtitle. Adding says where the card is about to land, because the add
/// row moves it in one go rather than dropping the user back on the list to pick what they
/// just named.
pub(crate) fn name_subtitle(renaming: Option<&str>, card: &str) -> String {
    match renaming {
        Some(old) => format!("A new name for {old}."),
        None => format!("Name it, and {card} moves into it."),
    }
}

/// What a collection row's trailing buttons are — the reorder grip is not among them: it is
/// the row's leading button, in the icon slot (see [`rows`]). Library keeps Rename but has no
/// Remove at all — `KnownHost::remove_collection` refuses it too, so the missing icon is the affordance
/// rather than the rule.
pub(crate) fn trailing(dynamic: bool) -> &'static [&'static str] {
    if dynamic {
        &[icons::ICON_EDIT]
    } else {
        &[icons::ICON_EDIT, icons::ICON_DELETE]
    }
}

/// The remove dialog's subtitle: what happens to the cards it holds, which is the whole
/// question — nothing is deleted, the games come back to Library.
pub(crate) fn remove_subtitle(name: &str, games: usize) -> String {
    let games = match games {
        0 => "It holds no games".to_string(),
        1 => "Its 1 game returns to Library".to_string(),
        n => format!("Its {n} games return to Library"),
    };
    format!("{name} will be removed. {games}.")
}

/// Why the typed name cannot be committed — `None` while it can. Blank reads as unfinished
/// rather than as an error, so only a name that is taken says anything.
pub(crate) fn name_hint(host: &KnownHost, at: Option<usize>, typed: &str) -> Option<&'static str> {
    let typed = typed.trim();
    (!typed.is_empty() && !host.can_name(at, typed)).then_some("Already used")
}

/// One row per entry in grid order, Library included. `holding` is the collection the card
/// being moved is in right now (`None` for Library, its implicit home) — that row wears the
/// mark dot, so the list opens saying where the card already is instead of leaving the user
/// to work it out.
pub(crate) fn rows(host: &KnownHost, holding: Option<usize>) -> Vec<FocusRow> {
    let library = host.library_index();
    let mut rows: Vec<FocusRow> = host
        .collections()
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
            // The grip stands in for the folder pictogram rather than sitting beside it: a
            // row this wide with an icon at each end reads as two controls and a label
            // between them, and the drag handle is the one worth pointing at.
            let row = FocusRow::action_with_value(icons::ICON_REORDER, collection.name.clone(), count_label(count))
                .with_trailing(trailing(collection.dynamic))
                .with_leading_button();
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
        .collect();
    // Last, and only while there is room: a row that would refuse the dialog it opens is
    // worse than no row (`MAX_COLLECTIONS`).
    if host.can_add_collection() {
        rows.push(FocusRow::action(icons::ICON_ADD, ADD_ROW.to_string()));
    }
    // Library has no Remove and one row wears the mark dot, both of which would otherwise
    // shift that row's count.
    ui::widgets::align_values(&mut rows);
    rows
}

/// How many rows [`rows`] builds — the count without their labels, which the compose,
/// hit-test and scroll paths ask for per frame.
pub(crate) fn row_count(host: &KnownHost) -> usize {
    host.collections().len() + usize::from(host.can_add_collection())
}

fn count_label(count: Option<usize>) -> String {
    match count {
        Some(1) => "1 game".to_string(),
        // "0 games" is noise next to the subtext that already says the row is hidden, and
        // Library has no count at all.
        Some(0) | None => String::new(),
        Some(n) => format!("{n} games"),
    }
}

/// The modal as a [`ModalScreen`] — the shell only; its rows are their own tiles.
pub(crate) struct Modal<'a> {
    pub rows: usize,
    /// From [`heading`].
    pub title: &'static str,
    /// The card being moved, named after the heading so the list says what it acts on.
    pub card: Option<&'a str>,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, _fonts: &ui::text::Fonts) -> Rect {
        scrolllist::layout(self.rows, screen_w, screen_h, scrolllist::COLLECTIONS_WIDTH_FRAC).0
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        scrolllist::render(
            c,
            self.rows,
            scrolllist::COLLECTIONS_WIDTH_FRAC,
            self.title,
            self.card,
            hover_close,
        )
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
    fn the_add_row_is_last_and_only_while_there_is_room() {
        let mut host = host();
        assert_eq!(rows(&host, None).last().map(|r| r.label.as_str()), Some(ADD_ROW));
        assert_eq!(row_count(&host), host.collections().len() + 1);
        for i in 0..crate::core::model::MAX_COLLECTIONS - 1 {
            host.add_collection(&format!("C{i}")).expect("under the limit");
        }
        assert_eq!(
            row_count(&host),
            host.collections().len(),
            "no row that would refuse itself"
        );
        assert!(rows(&host, None).iter().all(|r| r.label != ADD_ROW));
    }

    #[test]
    fn library_offers_no_remove_button() {
        let host = host();
        let rows = rows(&host, None);
        assert_eq!(rows[0].trailing, [icons::ICON_EDIT, icons::ICON_DELETE]);
        assert!(rows[0].leading_button, "the grip is the row's own icon");
        assert_eq!(rows[1].label, LIBRARY_COLLECTION);
        assert_eq!(
            rows[1].trailing,
            vec![icons::ICON_EDIT],
            "Library reorders and renames, but cannot be removed"
        );
        assert!(rows[2].trailing.is_empty(), "nor can the add row be acted on");
        assert!(!rows[2].leading_button, "nor dragged");
    }

    #[test]
    fn only_a_taken_name_is_refused_out_loud() {
        let host = host();
        assert_eq!(name_hint(&host, None, ""), None, "blank is unfinished, not wrong");
        assert_eq!(name_hint(&host, None, " pinned "), Some("Already used"));
        assert_eq!(name_hint(&host, Some(0), "Pinned"), None, "its own name, renaming it");
        assert_eq!(name_hint(&host, None, "Racing"), None);
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
