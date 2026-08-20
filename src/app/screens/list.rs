//! The plain list modals: Host menu, Wake settings, Diagnostics, Experimental, Cursor.
//!
//! Each is a card holding one `FocusRow` per line, focused by row index. Their handlers all
//! opened the same way — count the rows, move the cursor, arm the focus pop, return — and
//! their tiles were built by a `match self.screen` picking which `rows()` to call. Both of
//! those are here now, so a screen's own module holds only what it actually does differently:
//! which rows it lists, and what a press on one means.
use crate::app::nav::ScreenKey;
use crate::app::view;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::ui::widgets::FocusRow;
use std::time::Instant;

impl App {
    /// The rows of whichever plain list modal is open, `None` on any other screen.
    ///
    /// The single table over this family: the row *tiles*, the focused-row tile and the
    /// keyboard's row count all read it, so a screen cannot be listed by one and missed by
    /// another (see `docs/APP-REWORK-PLAN.md` §1, P3).
    pub(crate) fn list_modal_rows(&self) -> Option<Vec<FocusRow>> {
        Some(match self.nav.screen {
            Screen::HostMenu => self.host_menu_rows(),
            Screen::WakeSettings => view::wakesettings::rows(self.wake_settings_host().is_some_and(|h| h.wol_auto)),
            Screen::Diagnostics => view::diagnostics::rows(&self.settings_ui.settings),
            Screen::Experimental => view::experimental::rows(&self.settings_ui.settings, self.hosts.rooted),
            Screen::CursorSettings(_) => view::cursorsettings::rows(
                self.settings_target(),
                &self.editing_override(),
                Some(self.nav.cursor(ScreenKey::CursorSettings)),
            ),
            _ => return None,
        })
    }

    /// [`list_modal_rows`](Self::list_modal_rows) as the *focused-row* tile wants them: the
    /// focused row's ⋯ button lit, where it has one. The only place that highlight is drawn —
    /// the shell underneath must not bake one in, or it would outlive the focus that put it
    /// there (see `App::host_menu_actions`).
    pub(crate) fn list_focus_rows(&self) -> Option<Vec<FocusRow>> {
        let mut rows = self.list_modal_rows()?;
        if matches!(self.nav.screen, Screen::HostMenu) {
            if let Some(row) = rows.get_mut(self.nav.cursor(ScreenKey::HostMenu)) {
                row.menu = row.menu.map(|_| self.screens.host_menu_dots);
            }
        }
        Some(rows)
    }

    /// How many rows the open list modal has — the count without the labels, for the paths
    /// that only navigate (see `app::view::hostmenu::Metrics` for why that matters).
    pub(crate) fn list_modal_row_count(&self) -> usize {
        match self.nav.screen {
            Screen::HostMenu => self.host_menu_actions().len(),
            Screen::WakeSettings => view::wakesettings::ROW_COUNT,
            Screen::Diagnostics => crate::app::menu::DIAGNOSTICS_ROW_COUNT,
            Screen::Experimental => crate::app::menu::EXP_ROWS.len(),
            Screen::CursorSettings(_) => crate::app::menu::CURSOR_ROWS.len(),
            _ => 0,
        }
    }

    /// The nav half of a list screen's event handling: moves the cursor and arms the focus
    /// pop, reporting whether the event was spent doing so. Every list handler starts here,
    /// and none of them counts its own rows any more.
    pub(crate) fn list_nav_event(&mut self, ev: MenuEvent) -> bool {
        let len = self.list_modal_row_count();
        let key = ScreenKey::of(self.nav.screen);
        if crate::ui::widgets::list_nav(self.nav.cursor_mut(key), len, crate::app::menu::nav_dir(ev)) {
            self.modal.focus_anim = Some(Instant::now());
            return true;
        }
        false
    }

    /// Starts the focused row's switch slide from the value it is leaving, so the knob slides
    /// rather than snapping — the shared tail of every toggle row on every list screen.
    pub(crate) fn arm_switch_anim(&mut self, from: bool) {
        let row = self.nav.cursor(ScreenKey::of(self.nav.screen));
        self.modal.switch_anim = Some((Instant::now(), from, row));
    }

    /// The options the open dropdown lists, on whichever screen owns one.
    ///
    /// Two screens have dropdowns and they read different tables — Diagnostics' log level,
    /// and every other pick on the settings list. One exhaustive match rather than a `_ =>`
    /// arm per caller: the overlay's drawn height, its hit test and its focused-option tile
    /// all measure against this, so a screen absorbed into the wrong table by a fallback
    /// would draw options it cannot land on. `display_row` is the on-screen row the dropdown
    /// hangs off.
    pub(crate) fn dropdown_options(&self, display_row: usize) -> Vec<String> {
        match self.nav.screen {
            Screen::Diagnostics => crate::app::menu::log_level_dropdown_options(),
            Screen::Settings(set) => crate::app::menu::settings_logical_row(set, display_row)
                .map_or_else(Vec::new, |row| {
                    crate::app::menu::dropdown_options(row, self.detected_gamepad_type)
                }),
            // No dropdowns: nothing on these screens opens one, so nothing here should be
            // drawn or hit-tested as if it had.
            Screen::Home
            | Screen::Pairing
            | Screen::AddHost
            | Screen::Wake
            | Screen::ForgetHost
            | Screen::HostMenu
            | Screen::EditHost
            | Screen::About
            | Screen::SpeedTest
            | Screen::WakeSettings
            | Screen::PinLimit
            | Screen::Experimental
            | Screen::CursorSettings(_)
            | Screen::SendLogs => Vec::new(),
        }
    }

    /// How many options the open dropdown lists, without building their labels — the compose
    /// and hit-test paths ask per frame, and [`dropdown_options`](Self::dropdown_options)
    /// allocates a `String` per entry.
    pub(crate) fn dropdown_len(&self, display_row: usize) -> usize {
        match self.nav.screen {
            Screen::Diagnostics => crate::app::menu::LOG_LEVEL_OPTIONS.len(),
            Screen::Settings(set) => crate::app::menu::settings_logical_row(set, display_row)
                .map_or(0, crate::app::menu::dropdown_option_count),
            _ => 0,
        }
    }
}
