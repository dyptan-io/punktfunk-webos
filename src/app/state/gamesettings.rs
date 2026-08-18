//! The per-game settings screen's logic: opening it against one game, and folding an edit
//! back into that game's sparse override. The rows themselves, their navigation and their
//! dropdowns are the global Settings screen's — see `app::state::settings`, which both
//! screens share via `menu::SettingsScope`.
use crate::app::menu;
use crate::app::App;
use crate::core::screen::Screen;
use crate::services::store::{Settings, SettingsOverride};

/// What the per-game settings screen is editing. Set alongside `Screen::Settings(Game)`
/// and cleared on the way out, so `Some` and the screen being up mean the same thing.
pub(crate) struct GameSettingsState {
    /// The `KnownHost::games` key: a `GameEntry::id`, or `store::DESKTOP_PIN_ID`.
    pub pin_id: String,
    /// The game's name, shown dimmer after "Settings" in the title.
    pub title: String,
    /// The global settings with `over` applied — what every row renders from and what the
    /// shared `menu::*` mutators edit. Only the edited row is copied back into `over`.
    pub merged: Settings,
    pub over: SettingsOverride,
}

impl App {
    /// Opens the per-game screen for `pin_id`. Only the card submenu calls this.
    pub(crate) fn open_game_settings(&mut self, pin_id: &str, title: &str) {
        let over = self
            .selected_known_host()
            .map(|h| h.overrides(pin_id))
            .unwrap_or_default();
        self.game_settings = Some(GameSettingsState {
            pin_id: pin_id.to_string(),
            title: title.to_string(),
            merged: over.merge_into(self.settings),
            over,
        });
        self.settings_focused = 0;
        self.dropdown = None;
        // Its own scroll position, like every other modal list — and Settings' is stashed
        // the same way About stashes it.
        self.scroll = crate::ui::scroll::ScrollWindow::new();
        self.screen = Screen::Settings(menu::SettingsScope::Game);
    }

    /// Records `row`'s freshly edited value into the override, then re-derives `merged` so a
    /// row whose value the merge would clamp (HDR under an H.264 pick) shows what will
    /// actually be used.
    ///
    /// A value set back to what the global screen says stops being an override here rather
    /// than needing a separate "use global" gesture: `drop_matching` clears it, the row's dot
    /// goes out, and once nothing differs the game's whole record leaves the document (see
    /// `KnownHost::edit_overrides`). There is deliberately no other way to clear one row.
    pub(crate) fn capture_game_override(&mut self, row: usize) {
        let global = self.settings;
        let Some(gs) = self.game_settings.as_mut() else {
            return;
        };
        menu::override_capture(&mut gs.over, row, &gs.merged);
        gs.over.drop_matching(&global);
        gs.merged = gs.over.merge_into(global);
    }

    /// Writes the edited override back onto the host record and persists. Called on the way
    /// out, like the global screen's single save per visit.
    pub(crate) fn persist_game_settings(&mut self) {
        let Some(gs) = self.game_settings.take() else {
            return;
        };
        let Some(known) = self.selected_known_host_mut() else {
            return;
        };
        known.edit_overrides(&gs.pin_id, |over| *over = gs.over);
        self.persist();
    }
}

impl App {
    /// Whether `pin_id` carries any settings override on the selected host — what puts the
    /// amber dot in front of its card title, before anything is held.
    pub(crate) fn game_has_overrides(&self, pin_id: &str) -> bool {
        self.selected_known_host()
            .is_some_and(|h| !h.overrides(pin_id).is_empty())
    }
}
