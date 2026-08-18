//! The submenu a held grid card raises over its own title strip: Pin/Unpin, and Settings.
//!
//! It is not a `Screen` — it lives on Home, over one card, and Home's own navigation is
//! simply suspended while it is up (see `App::handle_home_event`). That keeps the card, its
//! focus ring and its frosted title strip on screen underneath, which is the whole point:
//! the menu belongs to *that* card, not to a modal that happens to know its id.
use std::time::{Duration, Instant};

use crate::app::menu::nav_dir;
use crate::app::{view, App};
use crate::core::event::MenuEvent;
use crate::core::screen::HomeFocus;
use crate::ui;

/// How long the selection band takes to travel from one row to the next. Short: the band is
/// the only thing that says which row is picked, so the move has to finish inside a keypress.
pub(crate) const MENU_FOCUS_SLIDE: Duration = Duration::from_millis(120);

/// Rows, in order. Two, and both always shown — an unpinnable card still shows Pin and
/// answers with the pin-limit alert, which says more than a missing row would.
pub(crate) const ROW_PIN: usize = 0;
pub(crate) const ROW_SETTINGS: usize = 1;
pub(crate) const ROW_COUNT: usize = 2;

/// The line shown once, on the release that added this menu, when the first library lands: the
/// feature is behind a hold, which nothing on screen would otherwise reveal. Phrased as what it
/// buys the user, not as a keypress.
pub(crate) const INTRO_HINT: &str =
    "New: hold OK on a card to give that game its own settings — some run smoother with a codec, \
     bitrate or resolution of their own.";

/// The open card submenu. `Some` exactly while it is up.
pub struct CardMenu {
    /// The card it belongs to (a `GameEntry::id`, or `store::DESKTOP_PIN_ID`).
    pub pin_id: String,
    /// That card's title — reused as the per-game settings screen's dim title suffix.
    pub title: String,
    pub focused: usize,
    /// The row focus just left, and when — where the band slides *from* (see
    /// [`MENU_FOCUS_SLIDE`]). `None` while it is settled on `focused`.
    pub leaving: Option<(usize, Instant)>,
    /// When it opened, for the panel's rise. Its own clock, not `focus_anim`: that one is
    /// re-armed by every focus move, and the panel must not restart with the card's zoom.
    pub since: Instant,
    /// The rise has had its final frame drawn (see [`CardMenu::tick`]).
    risen: bool,
}

impl CardMenu {
    /// Moves focus and arms the band's slide. No-op when `row` is already focused, so a
    /// pointer resting on a row can't restart a slide that goes nowhere.
    pub(crate) fn focus(&mut self, row: usize) {
        if row != self.focused {
            self.leaving = Some((self.focused, Instant::now()));
            self.focused = row;
        }
    }

    /// Whether the panel still owes frames — the rise, and the band's slide between rows.
    ///
    /// Both report `true` on the tick their clock *expires*, not just while it runs. That
    /// last tick is the one that draws the animation at its final value: report `false`
    /// there and the render loop parks in `wait_for_event` one frame short, leaving the
    /// panel a percent below its resting place until some unrelated event sets `dirty` —
    /// which, during a hold, is the button coming back up. It reads as the panel stalling
    /// just before the end and then finishing on release. `focus_anim` and friends avoid it
    /// by clearing their `Option` on the expiring tick and still reporting that tick.
    pub fn tick(&mut self) -> bool {
        let mut owed = false;
        if !self.risen {
            self.risen = self.since.elapsed() >= ui::animation::CARD_MENU_RISE;
            owed = true;
        }
        match self.leaving {
            Some((_, t)) if t.elapsed() < MENU_FOCUS_SLIDE => owed = true,
            Some(_) => {
                self.leaving = None;
                owed = true;
            }
            None => {}
        }
        owed
    }
}

impl App {
    /// Raises the submenu over the focused card. The long-hold's whole effect — see
    /// `runtime::ui_flow`.
    pub(crate) fn open_card_menu(&mut self, screen_w: u32) {
        let columns = view::home::grid_columns(screen_w.saturating_sub(ui::widgets::SIDEBAR_W));
        let HomeFocus::Grid(idx) = self.home_focus else {
            return;
        };
        let Some(pin_id) = self.pin_id_at_grid_idx(idx, columns).map(str::to_string) else {
            return;
        };
        let title = self.grid_card_content(idx, columns).0.to_string();
        self.card_menu = Some(CardMenu {
            pin_id,
            title,
            focused: ROW_PIN,
            leaving: None,
            since: Instant::now(),
            risen: false,
        });
    }

    pub(crate) fn close_card_menu(&mut self) {
        self.card_menu = None;
    }

    /// Handles one menu event while the submenu is up. Returns `false` when it isn't, so
    /// Home's own handler runs instead.
    pub(crate) fn handle_card_menu_event(&mut self, ev: MenuEvent, screen_w: u32, screen_h: u32) -> bool {
        let Some(menu) = self.card_menu.as_mut() else {
            return false;
        };
        // Wraps, like every other row list in the app (`list_nav`); `focus` is what arms the
        // band's slide, so the move goes through it rather than writing `focused` directly.
        let mut next = menu.focused;
        if ui::widgets::list_nav(&mut next, ROW_COUNT, nav_dir(ev)) {
            menu.focus(next);
            return true;
        }
        match ev {
            MenuEvent::Confirm => match menu.focused {
                ROW_PIN => {
                    self.close_card_menu();
                    // Keeps the pin-limit alert and the reorder animation it already owns.
                    self.toggle_focused_pin(screen_w, screen_h);
                }
                ROW_SETTINGS => {
                    let (pin_id, title) = (menu.pin_id.clone(), menu.title.clone());
                    self.close_card_menu();
                    self.open_game_settings(&pin_id, &title);
                }
                _ => {}
            },
            // Left/Right have nothing to move to, and Secondary would otherwise forget a
            // host from under an open menu.
            _ => self.close_card_menu(),
        }
        true
    }
}
