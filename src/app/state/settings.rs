//! The settings modal's logic: row navigation, dropdown, persistence. Rendering
//! (row list layout, dropdown overlay geometry) lives in `app::view::settings`.
//!
//! Shared by both settings screens. `Screen::Settings` edits the global document;
//! `Screen::GameSettings` edits a scratch copy for one game and records each touched row
//! into that game's sparse override (`app::state::gamesettings`). Which rows exist is
//! `menu::SettingsScope`'s answer, and every mutator below is indexed by logical `ROW_*`, so the
//! two screens can't drift apart.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::{App, DropdownState};
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use std::time::Instant;

impl App {
    /// Handles one menu event on the settings modal. `screen_h` is only used by
    /// `Up`/`Down` to keep `self.scroll` following `settings_focused`.
    pub fn handle_settings_event(&mut self, ev: MenuEvent, screen_h: u32) {
        let set = self.settings_scope();
        // An open Resolution/Frame rate dropdown intercepts all input until it's
        // closed (by picking an option or backing out) — it's a modal overlay on
        // top of the settings row list.
        if let Some(dd) = self.dropdown.as_mut() {
            // `dd.row` is the display position; setting lookups need the logical row.
            let row = dd.row;
            let logical = menu::settings_logical_row(set, row);
            let len = logical.map_or(1, |row| menu::dropdown_option_count(row).max(1));
            match ev {
                MenuEvent::Up | MenuEvent::Down => {
                    crate::ui::widgets::list_nav(&mut dd.focused, len, menu::nav_dir(ev));
                }
                // Applied after the borrow ends: the pick has to reach `self` beyond
                // `self.dropdown`, and the fade needs `dd.focused` either way.
                MenuEvent::Confirm | MenuEvent::Back => {
                    let choice = (ev == MenuEvent::Confirm).then_some(dd.focused);
                    self.dropdown_fade.close((row, dd.focused));
                    self.dropdown = None;
                    if let (Some(choice), Some(logical)) = (choice, logical) {
                        let detected = self.detected_gamepad_type;
                        // Not persisted here — `MenuEvent::Back` on the row list (leaving
                        // the whole screen) saves once for every change made during this
                        // visit, not per-row.
                        menu::apply_dropdown_choice(self.settings_target_mut(), logical, choice, detected);
                        self.capture_game_override(logical);
                    }
                }
                MenuEvent::Left | MenuEvent::Right | MenuEvent::Secondary => {}
            }
            return;
        }
        let total = menu::settings_row_count(set);
        match ev {
            // No wraparound here (unlike most other row lists) — wrapping a scrolled
            // list would silently jump the scroll position across the whole card.
            MenuEvent::Up => {
                if self.nav.cursor(ScreenKey::Settings) > 0 {
                    *self.nav.cursor_mut(ScreenKey::Settings) -= 1;
                    self.modal.focus_anim = Some(Instant::now());
                    self.scroll_settings_into_view(screen_h);
                }
            }
            MenuEvent::Down => {
                if self.nav.cursor(ScreenKey::Settings) + 1 < total {
                    *self.nav.cursor_mut(ScreenKey::Settings) += 1;
                    self.modal.focus_anim = Some(Instant::now());
                    self.scroll_settings_into_view(screen_h);
                }
            }
            MenuEvent::Left => self.apply_setting_adjust(self.nav.cursor(ScreenKey::Settings), false),
            MenuEvent::Right => self.apply_setting_adjust(self.nav.cursor(ScreenKey::Settings), true),
            MenuEvent::Confirm => match menu::settings_logical_row(set, self.nav.cursor(ScreenKey::Settings)) {
                // Focus past the end of the list: nothing to confirm.
                None => {}
                // Not a setting — a link out to the About screen (see `menu::SettingsRow::About`).
                // Settings are saved on the way out so the visit's changes aren't lost
                // behind the navigation.
                Some(menu::SettingsRow::About) => {
                    self.persist();
                    self.open_about();
                }
                // No save on the way in for the per-game flow: its copy is written once,
                // when its own screen is left (see `persist_game_settings`).
                Some(menu::SettingsRow::Cursor) => {
                    if set == menu::SettingsScope::Global {
                        self.persist();
                    }
                    self.open_cursor_settings(set);
                }
                Some(menu::SettingsRow::Experimental) => {
                    self.persist();
                    self.open_experimental();
                }
                Some(menu::SettingsRow::Diagnostics) => {
                    self.persist();
                    self.open_diagnostics();
                }
                // Puts the whole screen back: defaults on the global one, "inherit
                // everything" on the per-game one. Both keep the row list and the focus
                // where they are — nothing is shown or hidden by this (see `row_shown`).
                Some(menu::SettingsRow::Reset) => self.reset_settings(),
                // A locked row (see `menu::row_lock`) never opens its dropdown — there is
                // nothing to pick, which is exactly what the greyed row already says.
                Some(
                    logical @ (menu::SettingsRow::Resolution
                    | menu::SettingsRow::Framerate
                    | menu::SettingsRow::VideoBackend
                    | menu::SettingsRow::Codec
                    | menu::SettingsRow::Audio
                    | menu::SettingsRow::Gamepad),
                ) if menu::row_lock(logical, self.settings_target(), self.detected_gamepad_type).is_none() => {
                    let focused = menu::dropdown_current_index(self.settings_target(), logical);
                    // `row` is the display position (what the overlay is drawn against);
                    // the logical row is recovered on lookup via `settings_logical_row`.
                    self.dropdown = Some(DropdownState {
                        row: self.nav.cursor(ScreenKey::Settings),
                        focused,
                    });
                    self.dropdown_fade.reopen();
                }
                Some(_) => self.apply_setting_adjust(self.nav.cursor(ScreenKey::Settings), true),
            },
            // Leaving Settings (Back key or the modal's close-X, both funnel
            // through `App::back`) — save once for whatever changed during
            // this visit instead of once per row/keystroke. `StateWriter`
            // still queues the write on a background thread either way (see
            // its docs), but there's no reason to touch disk at all more than
            // once per Settings visit.
            MenuEvent::Back => {
                match set {
                    menu::SettingsScope::Global => self.persist(),
                    menu::SettingsScope::Game => self.persist_game_settings(),
                }
                self.nav.screen = Screen::Home;
            }
            // Per-game only — there is nothing above the global document to fall back to,
            // and `clear_focused_override` gates on the scope anyway.
            MenuEvent::Secondary => self.clear_focused_override(),
        }
    }

    /// The foot-of-the-list reset row (`menu::SettingsRow::Reset`). Not persisted here — like every
    /// other edit on these screens, it lands on the way out.
    ///
    /// Per-game only: the row appears on `SettingsScope::Game` alone (see
    /// `menu::settings_visible_logical_rows`), so a global screen can never reach here.
    fn reset_settings(&mut self) {
        let inherited = self.settings.presentable();
        if let Some(gs) = self.editing_game_mut() {
            gs.over = crate::services::store::SettingsOverride::default();
            gs.merged = inherited;
        }
    }

    /// Adjusts row in memory; persisted on `Back` (not per-keystroke). Starts `switch_anim` for toggle slides.
    /// `display_row` is the on-screen position; resolved to a logical `ROW_*` first.
    ///
    /// No focus re-anchoring afterwards: which rows are shown depends on the environment only
    /// (see `menu::row_shown`), so no adjustment can renumber the list under the cursor.
    pub(crate) fn apply_setting_adjust(&mut self, display_row: usize, forward: bool) {
        let Some(row) = menu::settings_logical_row(self.settings_scope(), display_row) else {
            return;
        };
        let toggled_from = menu::toggle_value(self.settings_target(), row);
        let detected = self.detected_gamepad_type;
        if menu::adjust_setting(self.settings_target_mut(), row, forward, detected) {
            self.capture_game_override(row);
            if let Some(from) = toggled_from {
                // Scope the slide to the display row being rendered (see `toggle_frac`).
                self.modal.switch_anim = Some((Instant::now(), from, display_row));
            }
        }
    }
}
