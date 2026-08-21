//! Moving a held card between collections — logic. Rendering lives in
//! `app::view::collections`.
//!
//! Reached from a card's submenu, and left with the card moved or nothing changed. The menu
//! stays up behind it, like the per-game settings screen: this is a step *into* that menu,
//! and collapsing the panel underneath would make going back read as having landed elsewhere.
use crate::app::nav::ScreenKey;
use crate::app::view;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::ui::widgets::FocusRow;

/// What [`Screen::Collections`] is acting on. Reset by `open_collections`, so a stale target
/// cannot outlive the menu that set it.
#[derive(Default)]
pub(crate) struct CollectionsState {
    /// The card being moved (a `GameEntry::id`, or `store::DESKTOP_PIN_ID`).
    pub(crate) target: Option<String>,
    /// That card's title, for the modal heading.
    pub(crate) title: String,
}

impl App {
    /// Raises the modal over the card `pin_id`, with the cursor on the collection already
    /// holding it.
    pub(crate) fn open_collections(&mut self, pin_id: &str, title: &str) {
        let holding = self.holding_row(pin_id);
        self.screens.collections = CollectionsState {
            target: Some(pin_id.to_string()),
            title: title.to_string(),
        };
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
        self.selected_known_host().map_or(0, |h| h.collections().len())
    }

    /// Handles one menu event on [`Screen::Collections`].
    pub(crate) fn handle_collections_event(&mut self, ev: MenuEvent, screen_w: u32, screen_h: u32) {
        if self.list_nav_event(ev) {
            self.scroll_list_row_into_view(screen_h);
            return;
        }
        match ev {
            MenuEvent::Confirm => self.confirm_collections_row(screen_w, screen_h),
            MenuEvent::Back | MenuEvent::Secondary => self.close_collections(),
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Left | MenuEvent::Right => {}
        }
    }

    /// Moves the target card into the focused collection — or out of whatever holds it, on
    /// the Library row — then closes both this modal and the menu it came from.
    pub(crate) fn confirm_collections_row(&mut self, screen_w: u32, screen_h: u32) {
        let row = self.nav.cursor(ScreenKey::Collections);
        let Some(target) = self.screens.collections.target.clone() else {
            return;
        };
        let Some(host) = self.selected_known_host() else {
            return;
        };
        let Some(collection) = host.collections().get(row) else {
            return;
        };
        // Library is "in no collection", so it commits as `None` rather than as its index.
        let (to, name) = ((!collection.dynamic).then_some(row), collection.name.clone());
        let columns = view::home::grid_columns(screen_w.saturating_sub(crate::ui::widgets::SIDEBAR_W));
        self.close_collections();
        self.move_card(&target, to, columns, screen_w, screen_h);
        self.home_status = Some(format!("Moved to {name}"));
    }

    /// Leaves the modal and the submenu behind it together: the move it confirms reorders the
    /// grid, and a menu latched to the card's old index would then point at a stranger.
    pub(crate) fn close_collections(&mut self) {
        self.screens.collections = CollectionsState::default();
        self.close_card_menu();
        self.nav.screen = Screen::Home;
    }
}
