//! The held card's submenu — presentation: its row labels, its screen-space geometry and
//! the selection band the compose path lays over the panel. Logic lives in
//! `app::state::cardmenu`.
//!
//! The panel itself is baked into one tile (`ui::tiles::render_card_menu_tile`) without any
//! notion of focus, so everything the *selection* needs is here and nothing of it is in that
//! tile's cache key: moving between rows rebuilds nothing.
use crate::app::state::cardmenu::{MENU_FOCUS_SLIDE, ROW_COUNT};
use crate::app::{view, App};
use crate::ui;
use crate::ui::render::Rect;

impl App {
    /// The submenu's rows for `pin_id`'s card. Takes the card rather than reading the open
    /// menu: the panel is baked when the card takes focus, before there is a menu to ask.
    /// Pin's label is the action it performs, not the state it reads.
    pub(crate) fn card_menu_rows(&self, pin_id: &str) -> [(&'static str, &'static str); ROW_COUNT] {
        let pinned = self.selected_known_host().is_some_and(|h| h.is_pinned(pin_id));
        [
            (view::icons::ICON_PIN, if pinned { "Unpin" } else { "Pin" }),
            (view::icons::ICON_SETTINGS, "Settings"),
        ]
    }

    /// The open submenu's rows band in screen space, and the card it hangs off — the
    /// pointer's counterpart to the panel `render_card_menu_tile` bakes. Derived from the
    /// same two heights the tile is, so a click lands on the row it can see.
    ///
    /// Scaled about the card exactly as `compose_grid` scales what it draws: a focused card
    /// sits permanently at `1 + CARD_GROWTH`, so an unscaled band would sit several pixels
    /// above the rows on screen — enough to mispick at a row boundary, and to drop clicks in
    /// the panel's bottom edge (which read as "clicked outside" and dismiss the menu).
    pub(crate) fn card_menu_rows_rect(&self, screen_w: u32, fonts: &ui::text::Fonts) -> Option<Rect> {
        let menu = self.card_menu.as_ref()?;
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        // The latch is only good while the grid still holds that card at that index — a
        // library reload landing under an open menu (`drain_games`) reorders it, and measuring
        // the wrong card's rect would put the rows somewhere the user never sees them.
        if self.pin_id_at_grid_idx(menu.idx, columns) != Some(menu.pin_id.as_str()) {
            return None;
        }
        let card = self.scrolled_card_rect(menu.idx, columns, ui::widgets::SIDEBAR_W as i32, available_w);
        let panel_h = ui::widgets::card_menu_strip_h(fonts.raster, fonts.value, card.height(), ROW_COUNT);
        let title_h = ui::widgets::title_strip_h(fonts.raster, fonts.value, card.height());
        let top = card.bottom() - panel_h as i32 + title_h as i32;
        let band = Rect::new(card.x(), top, card.width(), panel_h.saturating_sub(title_h));
        Some(ui::animation::scale_about(
            band,
            card,
            self.focused_card_scale(&menu.pin_id),
        ))
    }

    /// The transform `compose_grid` composites the focused card and everything on it with:
    /// the focus zoom and the appear pop, both about the card's own centre.
    pub(crate) fn focused_card_scale(&self, pin_id: &str) -> f32 {
        let f = ui::animation::anim_frac_smooth(self.render.focus_anim, ui::animation::CARD_FOCUS_POP);
        ui::animation::zoom_scale(f, crate::app::CARD_GROWTH)
            * ui::animation::pop_in_scale(self.card_pop_frac(pin_id), crate::app::CARD_POP_SHRINK)
    }

    /// The submenu row under the pointer, if any.
    pub(crate) fn card_menu_row_at(&self, x: i32, y: i32, screen_w: u32, fonts: &ui::text::Fonts) -> Option<usize> {
        let band = self.card_menu_rows_rect(screen_w, fonts)?;
        if !band.contains_point((x, y)) {
            return None;
        }
        // Proportional, not by `CARD_MENU_ROW_H`: the band carries the focused card's scale
        // (see `card_menu_rows_rect`), so its rows are that much taller on screen too.
        let row = ((y - band.y()) as f32 / band.height().max(1) as f32 * ROW_COUNT as f32) as usize;
        (row < ROW_COUNT).then_some(row)
    }

    /// The selection band in screen space, clipped to the part of the panel the wipe has
    /// revealed — `None` when no menu is open on this card, or when the rise hasn't reached
    /// the band yet.
    ///
    /// One band that *moves*, not one per row: on a focus change its top slides from the row
    /// being left to the row arriving, so the selection reads as a single object travelling
    /// the list rather than two darkenings trading places.
    ///
    /// The band is square-cornered as a rect; the compose path rounds its bottom corners when
    /// it ends on the card's edge (see `ui::tiles::render_card_menu_band_tile`).
    ///
    /// `panel_h` is the panel tile's own height and `shown` how many of its bottom rows are
    /// on screen, so the panel's local y maps to `card.bottom() - (panel_h - y)`. `rows_top`
    /// is the rows overlay's current panel-local top — passed in rather than recomputed
    /// because the overlay *slides* during the rise (see `compose_grid`), and the band has to
    /// ride with it or it sits on the resting row while the labels are still travelling.
    pub(crate) fn card_menu_band(&self, card: Rect, panel_h: u32, shown: u32, rows_top: i32) -> Option<Rect> {
        let menu = self.card_menu.as_ref()?;
        let row_h = ui::widgets::CARD_MENU_ROW_H as i32;
        let row_top = |row: usize| (rows_top + row as i32 * row_h) as f32;
        // Smoothstep, not the cubic ease-out: eased at both ends, a short travel of one row
        // height reads as one motion instead of a jump that drifts to a stop.
        let frac = ui::animation::anim_frac_smooth(menu.leaving.map(|(_, t)| t), MENU_FOCUS_SLIDE);
        let from = menu
            .leaving
            .map_or_else(|| row_top(menu.focused), |(row, _)| row_top(row));
        let top = (from + (row_top(menu.focused) - from) * frac) as i32;
        let visible_top = top.max(panel_h as i32 - shown as i32);
        let visible_bottom = (top + row_h).min(panel_h as i32);
        if visible_bottom <= visible_top {
            return None;
        }
        Some(Rect::new(
            card.x(),
            card.bottom() - (panel_h as i32 - visible_top),
            card.width(),
            (visible_bottom - visible_top) as u32,
        ))
    }
}
