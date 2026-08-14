//! The settings modal's logic: row navigation, dropdown, persistence. Rendering
//! (row list layout, dropdown overlay geometry) lives in `app::view::settings`.
use crate::app::menu;
use crate::app::{App, DropdownState};
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use std::time::Instant;

impl App {
    /// Handles one menu event on the settings modal. `screen_h` is only used by
    /// `Up`/`Down` to keep `self.scroll` following `settings_focused`.
    pub fn handle_settings_event(&mut self, ev: MenuEvent, screen_h: u32) {
        // An open Resolution/Frame rate dropdown intercepts all input until it's
        // closed (by picking an option or backing out) — it's a modal overlay on
        // top of the settings row list.
        if let Some(dd) = self.dropdown.as_mut() {
            // `dd.row` is the display position; setting lookups need the logical row.
            let row = dd.row;
            let logical = menu::settings_logical_row(&self.settings, row);
            let len = menu::dropdown_option_count(logical).max(1);
            match ev {
                MenuEvent::Up => dd.focused = if dd.focused == 0 { len - 1 } else { dd.focused - 1 },
                MenuEvent::Down => dd.focused = (dd.focused + 1) % len,
                MenuEvent::Confirm => {
                    let choice = dd.focused;
                    // Not persisted here — `MenuEvent::Back` below (leaving the
                    // whole Settings screen) saves once for every change made
                    // during this visit, not per-row.
                    menu::apply_dropdown_choice(&mut self.settings, logical, choice);
                    self.dropdown_fade.close((row, dd.focused));
                    self.dropdown = None;
                    // A codec change hides/shows the HDR row above; keep focus on the
                    // row just edited rather than letting the shift slide it away.
                    self.refocus_logical(logical);
                }
                MenuEvent::Back => {
                    self.dropdown_fade.close((row, dd.focused));
                    self.dropdown = None;
                }
                MenuEvent::Left | MenuEvent::Right | MenuEvent::Secondary => {}
            }
            return;
        }
        let total = menu::settings_row_count(&self.settings);
        match ev {
            // No wraparound here (unlike most other row lists) — wrapping a scrolled
            // list would silently jump the scroll position across the whole card.
            MenuEvent::Up => {
                if self.settings_focused > 0 {
                    self.settings_focused -= 1;
                    self.modal_focus_anim = Some(Instant::now());
                    self.scroll_settings_into_view(screen_h);
                }
            }
            MenuEvent::Down => {
                if self.settings_focused + 1 < total {
                    self.settings_focused += 1;
                    self.modal_focus_anim = Some(Instant::now());
                    self.scroll_settings_into_view(screen_h);
                }
            }
            MenuEvent::Left => self.apply_setting_adjust(self.settings_focused, false),
            MenuEvent::Right => self.apply_setting_adjust(self.settings_focused, true),
            MenuEvent::Confirm => match menu::settings_logical_row(&self.settings, self.settings_focused) {
                // Not a setting — a link out to the About screen (see `menu::ROW_ABOUT`).
                // Settings are saved on the way out so the visit's changes aren't lost
                // behind the navigation.
                menu::ROW_ABOUT => {
                    self.persist();
                    self.open_about();
                }
                menu::ROW_CURSOR => {
                    self.persist();
                    self.open_cursor_settings();
                }
                menu::ROW_EXPERIMENTAL => {
                    self.persist();
                    self.open_experimental();
                }
                menu::ROW_DIAGNOSTICS => {
                    self.persist();
                    self.open_diagnostics();
                }
                logical @ (menu::ROW_RESOLUTION
                | menu::ROW_FRAMERATE
                | menu::ROW_VIDEO_BACKEND
                | menu::ROW_CODEC
                | menu::ROW_AUDIO
                | menu::ROW_GAMEPAD) => {
                    let focused = menu::dropdown_current_index(&self.settings, logical);
                    // `row` is the display position (what the overlay is drawn against);
                    // the logical row is recovered on lookup via `settings_logical_row`.
                    self.dropdown = Some(DropdownState {
                        row: self.settings_focused,
                        focused,
                    });
                    self.dropdown_fade.reopen();
                }
                _ => self.apply_setting_adjust(self.settings_focused, true),
            },
            // Leaving Settings (Back key or the modal's close-X, both funnel
            // through `App::back`) — save once for whatever changed during
            // this visit instead of once per row/keystroke. `StateWriter`
            // still queues the write on a background thread either way (see
            // its docs), but there's no reason to touch disk at all more than
            // once per Settings visit.
            MenuEvent::Back => {
                self.persist();
                self.screen = Screen::Home;
            }
            MenuEvent::Secondary => {}
        }
    }

    /// Adjusts row in memory; persisted on `Back` (not per-keystroke). Starts `switch_anim` for toggle slides.
    /// `display_row` is the on-screen position; resolved to a logical `ROW_*` first.
    pub(crate) fn apply_setting_adjust(&mut self, display_row: usize, forward: bool) {
        let row = menu::settings_logical_row(&self.settings, display_row);
        let toggled_from = match row {
            menu::ROW_HDR => Some(self.settings.hdr_enabled),
            _ => None,
        };
        if menu::adjust_setting(&mut self.settings, row, forward) {
            if let Some(from) = toggled_from {
                // Scope the slide to the display row being rendered (see `toggle_frac`).
                self.switch_anim = Some((Instant::now(), from, display_row));
            }
        }
        // Cycling the codec can hide/show the HDR row above; keep focus on `row`.
        self.refocus_logical(row);
    }

    /// After a mutation that may have shown or hidden rows (a codec change toggles the
    /// HDR row's visibility), re-derive the display index of `logical` so focus stays on
    /// the same setting instead of sliding to whatever now occupies its old slot.
    fn refocus_logical(&mut self, logical: usize) {
        let position = menu::settings_visible_logical_rows(&self.settings).position(|r| r == logical);
        self.settings_focused = position.unwrap_or_else(|| {
            let count = menu::settings_row_count(&self.settings);
            self.settings_focused.min(count.saturating_sub(1))
        });
    }
}
