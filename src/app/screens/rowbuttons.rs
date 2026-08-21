//! The icon buttons a row can carry, across both row-list families.
//!
//! A row's own action is one press; anything else it offers is a button at one of its ends.
//! Right steps out along the trailing ones (the host menu's ⋯, a collection's rename and
//! remove) and Left onto the single leading one, which is the row's own icon slot (a
//! collection's drag handle). Which one has focus is `ScreenSlots::row_button` — one field,
//! because focus is on one row of one list at a time — and the two geometries are
//! `ui::widgets::{leading,trailing}_button_rect`, which the painter draws from and the
//! pointer hit-tests against.
use crate::app::nav::ScreenKey;
use crate::app::state::hostmenu::host_menu_trailing;
use crate::app::view;
use crate::app::App;
use crate::core::screen::Screen;
use crate::ui::render::Rect;
use crate::ui::widgets::{leading_button_rect, trailing_button_rect};
use std::time::Instant;

/// Which of a row's buttons the cursor is on. `None` (the absence of one of these) is the
/// row body — its own action — which is where a list opens and where Confirm means the row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum RowButton {
    /// The row's icon slot, at its left end.
    Leading,
    /// Index into [`FocusRow::trailing`](crate::ui::widgets::FocusRow::trailing).
    Trailing(usize),
}

impl RowButton {
    /// The trailing index, `None` for the leading button — what the row tile's
    /// `trailing_focused` takes.
    pub(crate) fn trailing(self) -> Option<usize> {
        match self {
            Self::Trailing(i) => Some(i),
            Self::Leading => None,
        }
    }
}

impl App {
    /// The buttons row `row` carries on whichever row list is open, as
    /// `(has a leading one, its trailing icons)` — off one of those screens, none.
    ///
    /// Derived from the same tables the rows are built from rather than from the rows
    /// themselves: this answers a pointer motion and a Left/Right press, and building the
    /// list to read one row's ends would format every label on the screen per event
    /// (`docs/COLLECTIONS-PLAN.md` §Risks).
    pub(crate) fn row_buttons(&self, row: usize) -> (bool, &'static [&'static str]) {
        match self.nav.screen {
            Screen::HostMenu => (
                false,
                self.host_menu_actions().get(row).copied().map_or(&[][..], host_menu_trailing),
            ),
            Screen::Collections => self
                .selected_known_host()
                .and_then(|host| host.collections().get(row))
                // Past the last collection is the add row: an action, with no ends.
                .map_or((false, &[][..]), |c| (true, view::collections::trailing(c.dynamic))),
            _ => (false, &[]),
        }
    }

    /// Steps focus along the focused row's buttons, `false` when there is nowhere left to
    /// step — which is what leaves Left/Right free to mean something else on rows without
    /// them. The row body sits between the two ends, so stepping back off the first trailing
    /// button lands on the row itself rather than jumping straight to the leading one.
    pub(crate) fn step_row_button(&mut self, forward: bool) -> bool {
        let row = self.nav.cursor(ScreenKey::of(self.nav.screen));
        let (leading, trailing) = self.row_buttons(row);
        let trailing = trailing.len();
        let next = match (self.screens.row_button, forward) {
            (None, true) if trailing > 0 => Some(RowButton::Trailing(0)),
            (None, false) if leading => Some(RowButton::Leading),
            (Some(RowButton::Trailing(i)), true) if i + 1 < trailing => Some(RowButton::Trailing(i + 1)),
            (Some(RowButton::Trailing(0)), false) | (Some(RowButton::Leading), true) => None,
            (Some(RowButton::Trailing(i)), false) => Some(RowButton::Trailing(i - 1)),
            // Off the end in either direction: the row keeps the focus it has.
            _ => return false,
        };
        self.screens.row_button = next;
        self.render.modal.focus_anim = Some(Instant::now());
        true
    }

    /// Which button of row `row` the pointer at `(x, y)` is over, given that row's on-screen
    /// rect. `None` between them, or on a row with none — which reads as the row body,
    /// exactly as a click that misses the ⋯ always has.
    pub(crate) fn row_button_at(&self, row: usize, row_rect: Rect, x: i32, y: i32) -> Option<RowButton> {
        let (leading, trailing) = self.row_buttons(row);
        if leading && leading_button_rect(row_rect).contains_point((x, y)) {
            return Some(RowButton::Leading);
        }
        (0..trailing.len())
            .find(|&i| trailing_button_rect(row_rect, trailing.len(), i).contains_point((x, y)))
            .map(RowButton::Trailing)
    }
}
