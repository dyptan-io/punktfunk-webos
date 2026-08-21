//! The scrolling row lists: Settings (in both scopes) and Collections.
//!
//! Each is a shell tile plus one tile per row, cropped to a viewport that scrolls under edge
//! fades — see `app::view::scrolllist` for the geometry. What differs between them is only
//! which rows they list and what invalidates those rows, so both are values here rather than
//! a `match self.screen` in each of the five places that build, key, measure, hit-test and
//! scroll them.
use crate::app::nav::ScreenKey;
use crate::app::screens::is_scroll_list;
use crate::app::view;
use crate::app::{menu, App};
use crate::core::model::Collection;
use crate::core::screen::Screen;
use crate::ui::cache;
use crate::ui::render::Rect;
use crate::ui::widgets::FocusRow;

impl App {
    /// The rows of whichever scrolling list is open, `None` on any other screen.
    ///
    /// The single table over this family, like `list_modal_rows` is over the plain modals:
    /// the row tiles and the row count both read it, so a screen cannot be listed by one and
    /// missed by the other.
    pub(crate) fn scroll_list_rows(&self) -> Option<Vec<FocusRow>> {
        match self.nav.screen {
            Screen::Settings(_) => Some(self.settings_rows()),
            Screen::Collections => self.collections_rows(),
            _ => None,
        }
    }

    /// How many rows the open list has, without building their labels — the compose,
    /// hit-test and scroll paths all ask per frame.
    pub(crate) fn scroll_list_row_count(&self) -> usize {
        self.scroll_list_row_count_for(self.nav.screen)
    }

    /// [`scroll_list_row_count`](Self::scroll_list_row_count) for an explicit screen: a
    /// closing modal is measured after `nav.screen` has already moved on.
    pub(crate) fn scroll_list_row_count_for(&self, screen: Screen) -> usize {
        match screen {
            Screen::Settings(set) => menu::settings_row_count(set),
            Screen::Collections => self.collections_row_count(),
            _ => 0,
        }
    }

    /// Card and content rects of the open scrolling list, `None` on any other screen.
    pub(crate) fn scroll_list_layout(&self, screen: Screen, screen_w: u32, screen_h: u32) -> Option<(Rect, Rect)> {
        is_scroll_list(screen).then(|| {
            view::scrolllist::layout(
                self.scroll_list_row_count_for(screen),
                screen_w,
                screen_h,
                scroll_list_width_frac(screen),
            )
        })
    }

    /// What the whole row list is derived from. Checked before the list is built at all: the
    /// per-row keys still arbitrate which row rebuilds, but on a pure animation frame — the
    /// common case while one of these is open — this comparison is the entire cost.
    pub(crate) fn scroll_list_rows_version(&self, content_w: u32) -> u64 {
        let screen = self.nav.screen;
        let cursor = self.nav.cursor(ScreenKey::of(screen));
        match screen {
            Screen::Settings(_) => cache::version(&(
                screen,
                *self.settings_target(),
                self.editing_override(),
                self.detected_gamepad_type,
                // The Controller row's caption turns on whether the pad is actually bound to
                // hid-playstation, which a hotplug can change on its own.
                crate::platform::webos::dualsense::hid_playstation_bound(),
                // The focused row carries the override-clear hint (`decorate_override`).
                cursor,
                self.settings_ui.dropdown.as_ref().map(|dd| dd.row),
                content_w,
            )),
            // Names, counts and which row holds the card — everything the rows draw. The
            // counts rather than the member ids: hashing every id of every collection is the
            // whole library, per frame, and no row draws one.
            Screen::Collections => cache::version(&(
                screen,
                self.selected_known_host().map(|host| CollectionShapes(host.collections())),
                self.screens.collections.target.as_deref(),
                content_w,
            )),
            _ => cache::version(&(screen, content_w)),
        }
    }

    /// Scrolls the open list's focused row into view — what every cursor move on one of these
    /// screens ends with.
    pub(crate) fn scroll_list_row_into_view(&mut self, screen_h: u32) {
        let total = self.scroll_list_row_count();
        let visible = view::scrolllist::visible_rows(total, screen_h);
        self.render
            .scroll
            .scroll_into_view(self.nav.cursor(ScreenKey::of(self.nav.screen)), total, visible);
    }
}

/// The part of a host's collections the rows actually draw: each one's name, how many games
/// it holds and whether it is the dynamic entry. Hashed instead of the collections themselves,
/// whose member ids are the whole library and change nothing on screen but the counts.
struct CollectionShapes<'a>(&'a [Collection]);

impl std::hash::Hash for CollectionShapes<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for c in self.0 {
            (c.name.as_str(), c.games.len(), c.dynamic).hash(state);
        }
    }
}

/// Which card width this family member wears. The third value in the table, alongside its rows
/// and its invalidation key — the geometry and the render path both read it, so a screen
/// cannot be measured at one width and drawn at another.
pub(crate) fn scroll_list_width_frac(screen: Screen) -> f32 {
    match screen {
        Screen::Collections => view::scrolllist::COLLECTIONS_WIDTH_FRAC,
        _ => view::scrolllist::SETTINGS_WIDTH_FRAC,
    }
}
