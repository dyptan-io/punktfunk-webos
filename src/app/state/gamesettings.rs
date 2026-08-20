//! The per-game settings screen's logic: opening it against one game, and folding an edit
//! back into that game's sparse override. The rows themselves, their navigation and their
//! dropdowns are the global Settings screen's — see `app::state::settings`, which both
//! screens share via `menu::SettingsScope`.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::core::screen::Screen;
use crate::services::store::{Settings, SettingsOverride};

/// What the per-game settings screen is editing. Set alongside `Screen::Settings(Game)`
/// and cleared on the way out, so `Some` and the screen being up mean the same thing.
pub(crate) struct GameSettingsState {
    /// The write-back key, pinned at open time rather than re-read from `selected_host`: the
    /// selection can move under an open modal (a wake completing), and the edits belong to
    /// the host they were made on.
    pub host: (String, u16),
    /// The `KnownHost::games` key: a `GameEntry::id`, or `store::DESKTOP_PIN_ID`.
    pub pin_id: String,
    /// The game's name, shown dimmer after "Settings" in the title.
    pub title: String,
    /// The global settings with `over` applied, then `Settings::presentable` — what every row
    /// renders from and what the shared `menu::*` mutators edit, so a row shows the value the
    /// stream will actually use. The clamp never reaches `over`.
    pub merged: Settings,
    pub over: SettingsOverride,
}

impl App {
    /// Opens the per-game screen for `pin_id`. Only the card submenu calls this.
    pub(crate) fn open_game_settings(&mut self, pin_id: &str, title: &str) {
        let Some(host) = self.library.selected_host.clone() else {
            return;
        };
        let over = self
            .known_host(&host.0, host.1)
            .map_or_else(SettingsOverride::default, |h| h.overrides(pin_id));
        self.settings_ui.game_settings = Some(GameSettingsState {
            host,
            pin_id: pin_id.to_string(),
            title: title.to_string(),
            merged: over.merge_into(self.settings_ui.settings).presentable(),
            over,
        });
        self.nav.set_cursor(ScreenKey::Settings, 0);
        self.settings_ui.dropdown = None;
        // Its own scroll position, like every other modal list — and Settings' is stashed
        // the same way About stashes it.
        self.scroll = crate::ui::scroll::ScrollWindow::new();
        self.nav.screen = Screen::Settings(menu::SettingsScope::Game);
    }

    /// Records `row`'s freshly edited value into the override, then re-derives `merged` so a
    /// row whose value the merge would clamp (HDR under an H.264 pick) shows what will
    /// actually be used.
    ///
    /// A pick that lands on the global's own current value stores nothing; a row that
    /// genuinely differs is cleared by `clear_game_override` instead. Both rules, and why
    /// only *this* row is judged against the global, are on `store::SettingsOverride`.
    pub(crate) fn capture_game_override(&mut self, row: menu::SettingsRow) {
        let global = self.settings_ui.settings;
        self.edit_game_override(|over, merged| menu::override_capture(over, row, merged, &global));
    }

    /// Drops the focused row back to inheriting the global — the Secondary key, and the only
    /// way a single override goes away (the Reset row drops them all). A no-op outside the
    /// per-game scope and on a row that overrides nothing, so the key is safe to lean on.
    ///
    /// Resolves the row here rather than at each binding: which list the focus indexes is the
    /// only thing the flow's two screens differ in.
    pub(crate) fn clear_focused_override(&mut self) {
        let row = match self.nav.screen {
            Screen::CursorSettings(_) => menu::CURSOR_ROWS
                .get(self.nav.cursor(ScreenKey::CursorSettings))
                .copied(),
            _ => menu::settings_logical_row(self.settings_scope(), self.nav.cursor(ScreenKey::Settings)),
        };
        let Some(row) = row else { return };
        self.edit_game_override(|over, _| menu::override_clear(over, row));
    }

    /// Runs one edit against the open game's override, handing it the scratch `merged` the row
    /// mutators just ran against, then re-derives `merged`. The one place the two halves of
    /// the scratch state are kept in step, and gated by `editing_game_mut`.
    fn edit_game_override(&mut self, edit: impl FnOnce(&mut SettingsOverride, &Settings)) {
        let global = self.settings_ui.settings;
        let Some(gs) = self.editing_game_mut() else {
            return;
        };
        let merged = gs.merged;
        edit(&mut gs.over, &merged);
        gs.merged = gs.over.merge_into(global).presentable();
    }

    /// Writes the edited override back onto the host record and persists. Called on the way
    /// out, like the global screen's single save per visit.
    pub(crate) fn persist_game_settings(&mut self) {
        let Some(gs) = self.settings_ui.game_settings.take() else {
            return;
        };
        let Some(known) = self.known_host_mut(&gs.host.0, gs.host.1) else {
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
