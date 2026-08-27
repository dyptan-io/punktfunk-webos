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

/// The focusable controls and right-side geometry a row exposes.
struct RowButtons {
    leading: bool,
    trailing: &'static [&'static str],
    mark_reserved: bool,
}

impl RowButtons {
    const fn trailing(trailing: &'static [&'static str]) -> Self {
        Self {
            leading: false,
            trailing,
            mark_reserved: false,
        }
    }

    const fn with_leading(mut self) -> Self {
        self.leading = true;
        self
    }

    const fn reserve_mark(mut self, reserve: bool) -> Self {
        self.mark_reserved = reserve;
        self
    }
}

impl App {
    /// The buttons and end geometry row `row` carries on whichever row list is open.
    ///
    /// Derived from the same tables the rows are built from rather than from the rows
    /// themselves: this answers a pointer motion and a Left/Right press, and building the
    /// list to read one row's ends would format every label on the screen per event
    /// (`docs/COLLECTIONS-PLAN.md` §Risks).
    fn row_buttons(&self, row: usize) -> RowButtons {
        match self.nav.screen {
            Screen::HostMenu => RowButtons::trailing(
                self.host_menu_actions()
                    .get(row)
                    .copied()
                    .map_or(&[][..], host_menu_trailing),
            ),
            Screen::Experimental => crate::app::menu::EXP_ROWS.get(row).map_or_else(
                || RowButtons::trailing(&[]),
                |&r| {
                    RowButtons::trailing(view::experimental::trailing(r, &self.settings_ui.settings)).reserve_mark(
                        view::experimental::trailing_mark_reserved(r, &self.settings_ui.settings),
                    )
                },
            ),
            // One row, one button: the tick that finishes the measurement.
            Screen::HdrCalibration => RowButtons::trailing(view::hdrcalibration::ACTION_ICONS),
            Screen::Collections => self
                .selected_known_host()
                .and_then(|host| host.collections().get(row))
                // Past the last collection is the add row: an action, with no ends.
                .map_or_else(
                    || RowButtons::trailing(&[]),
                    |c| {
                        RowButtons::trailing(view::collections::trailing(c.dynamic))
                            .with_leading()
                            .reserve_mark(true)
                    },
                ),
            _ => RowButtons::trailing(&[]),
        }
    }

    /// The icon identifying one trailing action on `row`.
    pub(crate) fn row_trailing_button(&self, row: usize, index: usize) -> Option<&'static str> {
        self.row_buttons(row).trailing.get(index).copied()
    }

    /// Steps focus along the focused row's buttons, `false` when there is nowhere left to
    /// step — which is what leaves Left/Right free to mean something else on rows without
    /// them. The row body sits between the two ends, so stepping back off the first trailing
    /// button lands on the row itself rather than jumping straight to the leading one.
    pub(crate) fn step_row_button(&mut self, forward: bool) -> bool {
        let row = self.nav.cursor(ScreenKey::of(self.nav.screen));
        let buttons = self.row_buttons(row);
        let leading = buttons.leading;
        let trailing = buttons.trailing.len();
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
        let buttons = self.row_buttons(row);
        if buttons.leading && leading_button_rect(row_rect).contains_point((x, y)) {
            return Some(RowButton::Leading);
        }
        (0..buttons.trailing.len())
            .find(|&i| {
                trailing_button_rect(row_rect, buttons.trailing.len(), i, buttons.mark_reserved).contains_point((x, y))
            })
            .map(RowButton::Trailing)
    }
}
