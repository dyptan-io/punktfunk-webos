//! The trailing icon buttons a row can carry, across both row-list families.
//!
//! A row's own action is one press; anything else it offers (the host menu's ⋯, a
//! collection's rename and remove) is a button at its right end, reached with Right and left
//! with Left. Which one has focus is `ScreenSlots::row_button` — one field, because focus is
//! on one row of one list at a time — and the geometry is `ui::widgets::trailing_button_rect`,
//! which the painter draws from and the pointer hit-tests against.
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::ui::render::Rect;
use crate::ui::widgets::trailing_button_rect;
use std::time::Instant;

impl App {
    /// The trailing icons of row `row` on whichever row list is open — empty off one, and on
    /// a row that carries none.
    pub(crate) fn row_trailing(&self, row: usize) -> Vec<&'static str> {
        self.list_modal_rows()
            .or_else(|| self.scroll_list_rows())
            .and_then(|rows| rows.get(row).map(|r| r.trailing.clone()))
            .unwrap_or_default()
    }

    /// The trailing icons of the *focused* row — what Right steps into and what Confirm on a
    /// button means.
    pub(crate) fn focused_row_trailing(&self) -> Vec<&'static str> {
        self.row_trailing(self.nav.cursor(ScreenKey::of(self.nav.screen)))
    }

    /// Steps focus through the focused row's trailing buttons, `false` when there is nowhere
    /// left to step — which is what leaves Left/Right free to mean something else on rows
    /// without them.
    pub(crate) fn step_row_button(&mut self, forward: bool) -> bool {
        let count = self.focused_row_trailing().len();
        let next = match (self.screens.row_button, forward) {
            (_, true) if count == 0 => return false,
            (None, true) => Some(0),
            (Some(i), true) if i + 1 < count => Some(i + 1),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
            // Off the end in either direction: the row keeps the focus it has.
            _ => return false,
        };
        self.screens.row_button = next;
        self.render.modal.focus_anim = Some(Instant::now());
        true
    }

    /// Which trailing button of row `row` the pointer at `(x, y)` is over, given that row's
    /// on-screen rect. `None` between them, or on a row with none — which reads as the row
    /// body, exactly as a click that misses the ⋯ always has.
    pub(crate) fn row_button_at(&self, row: usize, row_rect: Rect, x: i32, y: i32) -> Option<usize> {
        let trailing = self.row_trailing(row);
        (0..trailing.len()).find(|&i| trailing_button_rect(row_rect, trailing.len(), i).contains_point((x, y)))
    }
}
