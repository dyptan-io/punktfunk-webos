//! Moving a held card between collections — logic. Rendering lives in
//! `app::view::collections`.
//!
//! Reached from a card's submenu, and left with the card moved or nothing changed. The menu
//! stays up behind it, like the per-game settings screen: this is a step *into* that menu,
//! and collapsing the panel underneath would make going back read as having landed elsewhere.
use crate::app::nav::ScreenKey;
use crate::app::state::textfield::TextField;
use crate::app::view;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::model::MAX_COLLECTION_NAME;
use crate::core::screen::Screen;
use crate::ui::widgets::FocusRow;

/// What [`Screen::Collections`] and the name dialog over it are acting on. Reset by
/// `open_collections`, so a stale target cannot outlive the menu that set it.
pub(crate) struct CollectionsState {
    /// The card being moved (a `GameEntry::id`, or `store::DESKTOP_PIN_ID`).
    pub(crate) target: Option<String>,
    /// That card's title, for the modal heading.
    pub(crate) title: String,
    /// The collection [`Screen::RenameCollection`] is naming — `None` while it is naming one
    /// that does not exist yet.
    pub(crate) index: Option<usize>,
    /// The name being typed there.
    pub(crate) name: TextField,
}

/// Hand-written: the shared [`TextField`] defaults to an address, and this one holds a name.
impl Default for CollectionsState {
    fn default() -> Self {
        Self {
            target: None,
            title: String::new(),
            index: None,
            name: TextField::name(MAX_COLLECTION_NAME, ""),
        }
    }
}

impl App {
    /// Raises the modal over the card `pin_id`, with the cursor on the collection already
    /// holding it.
    pub(crate) fn open_collections(&mut self, pin_id: &str, title: &str) {
        let holding = self.holding_row(pin_id);
        self.screens.collections = CollectionsState {
            target: Some(pin_id.to_string()),
            title: title.to_string(),
            ..CollectionsState::default()
        };
        self.screens.row_button = None;
        self.nav.enter(Screen::Collections, holding);
        self.render.scroll = crate::ui::scroll::ScrollWindow::new();
        self.render.content_window = crate::ui::scroll::ContentWindow::new();
    }

    /// The row index of the collection holding `pin_id` — the Library row when nothing else
    /// claims it, and 0 on a host with no collections at all.
    fn holding_row(&self, pin_id: &str) -> usize {
        let Some(host) = self.selected_known_host() else {
            return 0;
        };
        host.collection_of(pin_id).or_else(|| host.library_index()).unwrap_or(0)
    }

    /// The modal's rows, `None` off the screen or with no host selected.
    pub(crate) fn collections_rows(&self) -> Option<Vec<FocusRow>> {
        let host = self.selected_known_host()?;
        let target = self.screens.collections.target.as_deref()?;
        Some(view::collections::rows(host, host.collection_of(target)))
    }

    pub(crate) fn collections_row_count(&self) -> usize {
        self.selected_known_host().map_or(0, view::collections::row_count)
    }

    /// Handles one menu event on [`Screen::Collections`].
    pub(crate) fn handle_collections_event(&mut self, ev: MenuEvent, screen_w: u32, screen_h: u32) {
        if self.list_nav_event(ev) {
            self.scroll_list_row_into_view(screen_h);
            return;
        }
        match ev {
            // Right/Left step onto the row's rename/remove buttons before leaving it.
            MenuEvent::Right | MenuEvent::Left if self.step_row_button(ev == MenuEvent::Right) => {}
            MenuEvent::Confirm => self.confirm_collections_row(screen_w, screen_h),
            MenuEvent::Back | MenuEvent::Secondary => self.close_collections(),
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Left | MenuEvent::Right => {}
        }
    }

    /// Moves the target card into the focused collection — or out of whatever holds it, on
    /// the Library row — then closes both this modal and the menu it came from.
    pub(crate) fn confirm_collections_row(&mut self, screen_w: u32, screen_h: u32) {
        let row = self.nav.cursor(ScreenKey::Collections);
        // A trailing button acts on the row it is on rather than on the card being moved.
        // Read by icon, not by index: Library carries one button fewer.
        if let Some(button) = self.screens.row_button {
            match self.row_trailing(row).get(button) {
                Some(&view::icons::ICON_EDIT) => self.open_name_collection(Some(row)),
                Some(&view::icons::ICON_DELETE) => self.open_remove_collection(row),
                _ => {}
            }
            return;
        }
        let Some(target) = self.screens.collections.target.clone() else {
            return;
        };
        let Some(host) = self.selected_known_host() else {
            return;
        };
        let Some(collection) = host.collections().get(row) else {
            // Past the last collection is the add row, which names one instead of moving into
            // it — and then moves the card there itself (see `confirm_collection_name`).
            self.open_name_collection(None);
            return;
        };
        // Library is "in no collection", so it commits as `None` rather than as its index.
        let (to, name) = ((!collection.dynamic).then_some(row), collection.name.clone());
        let columns = view::home::grid_columns(screen_w.saturating_sub(crate::ui::widgets::SIDEBAR_W));
        self.close_collections();
        self.move_card(&target, to, columns, screen_w, screen_h);
        self.home_status = Some(format!("Moved to {name}"));
    }

    /// Raises the name dialog over the list: `at` is the collection being renamed, `None` to
    /// name a new one. The list stays behind it, like every other step *into* a screen.
    pub(crate) fn open_name_collection(&mut self, at: Option<usize>) {
        let existing = at
            .and_then(|i| self.selected_known_host()?.collections().get(i))
            .map_or(String::new(), |c| c.name.clone());
        self.screens.collections.index = at;
        self.screens.collections.name = TextField::name(MAX_COLLECTION_NAME, &existing);
        self.nav.screen = Screen::RenameCollection;
    }

    /// Handles one menu event on [`Screen::RenameCollection`]. Left is backspace, as on the
    /// address form — the remote has no delete key.
    pub(crate) fn handle_name_collection_event(&mut self, ev: MenuEvent, screen_w: u32, screen_h: u32) {
        match ev {
            MenuEvent::Left => self.screens.collections.name.backspace(),
            MenuEvent::Confirm => self.confirm_collection_name(screen_w, screen_h),
            MenuEvent::Back | MenuEvent::Secondary => self.nav.resume(Screen::Collections),
            MenuEvent::Right | MenuEvent::Up | MenuEvent::Down => {}
        }
    }

    /// One character from the on-screen keyboard or the remote's number pad.
    pub(crate) fn enter_collection_name_char(&mut self, c: char) {
        self.screens.collections.name.enter_char(c);
    }

    /// Why the typed name is refused, `None` while it is not — what the dialog says instead of
    /// greying its confirm with no explanation.
    pub(crate) fn collection_name_hint(&self) -> Option<&'static str> {
        let host = self.selected_known_host()?;
        view::collections::name_hint(
            host,
            self.screens.collections.index,
            self.screens.collections.name.text(),
        )
    }

    /// Commits the typed name: renames the collection it was opened on, or creates one and —
    /// since the only way here is a card's "Move to…" — moves the held card straight into it,
    /// rather than dropping the user back on the list to pick what they just named.
    pub(crate) fn confirm_collection_name(&mut self, screen_w: u32, screen_h: u32) {
        let typed = self.screens.collections.name.text().trim().to_string();
        let at = self.screens.collections.index;
        let Some(host) = self.selected_known_host_mut() else {
            return;
        };
        if !host.can_name(at, &typed) {
            return;
        }
        match at {
            Some(i) => {
                host.rename_collection(i, &typed);
                self.persist();
                self.regroup_games();
                self.nav.resume(Screen::Collections);
            }
            None => {
                let Some(added) = host.add_collection(&typed) else {
                    return;
                };
                self.persist();
                let Some(target) = self.screens.collections.target.clone() else {
                    self.nav.enter(Screen::Collections, added);
                    return;
                };
                let columns = view::home::grid_columns(screen_w.saturating_sub(crate::ui::widgets::SIDEBAR_W));
                self.close_collections();
                self.move_card(&target, Some(added), columns, screen_w, screen_h);
                self.home_status = Some(format!("Moved to {typed}"));
            }
        }
    }

    /// Raises the remove confirmation over the list, focused on Cancel — the destructive
    /// button is never the one a stray Confirm lands on.
    pub(crate) fn open_remove_collection(&mut self, at: usize) {
        self.screens.collections.index = Some(at);
        self.nav.enter(Screen::RemoveCollection, 1);
    }

    /// The collection the remove dialog is asking about — its name and how many games come
    /// back to Library with it.
    pub(crate) fn removed_collection(&self) -> Option<(&str, usize)> {
        let at = self.screens.collections.index?;
        let collection = self.selected_known_host()?.collections().get(at)?;
        Some((collection.name.as_str(), collection.games.len()))
    }

    /// Handles one menu event on [`Screen::RemoveCollection`].
    pub(crate) fn handle_remove_collection_event(&mut self, ev: MenuEvent) {
        match ev {
            MenuEvent::Left | MenuEvent::Right => {
                self.nav.set_cursor(
                    ScreenKey::RemoveCollection,
                    1 - self.nav.cursor(ScreenKey::RemoveCollection),
                );
                self.render.modal.focus_anim = Some(std::time::Instant::now());
            }
            MenuEvent::Confirm => {
                if self.nav.cursor(ScreenKey::RemoveCollection) == 0 {
                    self.remove_collection();
                }
                self.back_to_collections();
            }
            MenuEvent::Back | MenuEvent::Secondary => self.back_to_collections(),
            MenuEvent::Up | MenuEvent::Down => {}
        }
    }

    /// Removes the collection the dialog was opened on; its games fall back to Library, which
    /// is a change to the grid's sections, so the layout is rebuilt with it.
    fn remove_collection(&mut self) {
        let Some(at) = self.screens.collections.index else {
            return;
        };
        let Some(host) = self.selected_known_host_mut() else {
            return;
        };
        if !host.remove_collection(at) {
            return;
        }
        self.persist();
        self.regroup_games();
    }

    /// Back to the list the dialog was raised over, with its cursor held inside it — the row
    /// that was focused may have just been the one removed.
    fn back_to_collections(&mut self) {
        self.screens.collections.index = None;
        self.screens.row_button = None;
        let last = self.collections_row_count().saturating_sub(1);
        let key = ScreenKey::Collections;
        let at = self.nav.cursor(key).min(last);
        self.nav.enter(Screen::Collections, at);
    }

    /// Leaves the modal and the submenu behind it together: the move it confirms reorders the
    /// grid, and a menu latched to the card's old index would then point at a stranger.
    pub(crate) fn close_collections(&mut self) {
        self.screens.collections = CollectionsState::default();
        self.screens.row_button = None;
        self.close_card_menu();
        self.nav.screen = Screen::Home;
    }
}
