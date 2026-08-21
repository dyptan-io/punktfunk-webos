//! The held card's submenu — presentation: its row labels, its screen-space geometry and
//! the selection band the compose path lays over the panel. Logic lives in
//! `app::state::cardmenu`.
//!
//! The panel's glass and title are baked without any notion of focus, so moving between rows
//! rebuilds neither; the *selection* — its geometry here, its pixels in
//! `ui::tiles::CardMenuBandTile` — is what a row move costs.
use crate::app::state::cardmenu::CardMenuRow;
use crate::app::{view, App};
use crate::ui;
use crate::ui::render::Rect;

impl App {
    /// Which rows `pin_id`'s card shows. The one table over the submenu's shape: its labels,
    /// its baked height, its tile key, its hit test and its handler all count rows through
    /// here.
    ///
    /// Takes the card rather than reading the open menu: the panel is baked when the card
    /// takes focus, before there is a menu to ask — membership is available there too, so the
    /// count is safe at that point.
    pub(crate) fn card_menu_row_kinds(&self, pin_id: &str) -> &'static [CardMenuRow] {
        // Nothing to remove a Library card from — Library *is* "in no collection".
        if self.collection_of_card(pin_id).is_some() {
            &[CardMenuRow::MoveTo, CardMenuRow::Remove, CardMenuRow::Settings]
        } else {
            &[CardMenuRow::MoveTo, CardMenuRow::Settings]
        }
    }

    /// [`card_menu_row_kinds`](Self::card_menu_row_kinds) with the icon and label each row
    /// draws.
    pub(crate) fn card_menu_rows(&self, pin_id: &str) -> Vec<(&'static str, &'static str)> {
        self.card_menu_row_kinds(pin_id)
            .iter()
            .map(|kind| match kind {
                CardMenuRow::MoveTo => (crate::ui::theme::icons().pin, "Add to\u{2026}"),
                CardMenuRow::Remove => (view::icons::ICON_DELETE, "Remove"),
                CardMenuRow::Settings => (view::icons::ICON_SETTINGS, "Settings"),
            })
            .collect()
    }

    /// How many rows the submenu on `pin_id`'s card has — what its geometry divides by.
    pub(crate) fn card_menu_row_count(&self, pin_id: &str) -> usize {
        2 + usize::from(self.collection_of_card(pin_id).is_some())
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
        // Nothing to hit while the panel is collapsed for a reorder (see `compose_grid`).
        let menu = self.card_menu.as_ref().filter(|m| !m.moved)?;
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        // The latch is only good while the grid still holds that card at that index — a
        // library reload landing under an open menu (`drain_games`) reorders it, and measuring
        // the wrong card's rect would put the rows somewhere the user never sees them.
        if self.pin_id_at_grid_idx(menu.idx, columns) != Some(menu.pin_id.as_str()) {
            return None;
        }
        let card = self.scrolled_card_rect(menu.idx, columns, ui::widgets::SIDEBAR_W as i32, available_w);
        let rows = self.card_menu_row_count(&menu.pin_id);
        let panel_h = ui::widgets::card_menu_strip_h(fonts.raster, fonts.value, card.height(), rows);
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
        let count = self.card_menu_row_count(&self.card_menu.as_ref()?.pin_id);
        // Into the panel's own coordinates first, then split by the same constants the rows
        // are drawn at: the band carries the focused card's scale (see `card_menu_rows_rect`),
        // so on screen every row and both pads are that much taller.
        let local = (y - band.y()) as f32 * ui::widgets::card_menu_rows_h(count) as f32 / band.height().max(1) as f32;
        let row =
            ((local - ui::widgets::CARD_MENU_ROWS_PAD as f32).max(0.0) / ui::widgets::CARD_MENU_ROW_H as f32) as usize;
        // Clamped, not rejected: the block's top and bottom padding belong to the row nearest
        // them, so a click just off a row still picks it rather than dismissing the menu.
        Some(row.min(count.saturating_sub(1)))
    }

    /// The selection band in screen space, clipped to the part of the panel the wipe has
    /// revealed — `None` when no menu is open on this card, or when the rise hasn't reached
    /// the band yet.
    ///
    /// It does not travel between rows: it is drawn on whichever row has focus and *pops*
    /// there, the same zoom `tile::MODAL_FOCUS` plays when a settings row takes focus (see
    /// `compose_card_strip`, which applies it). One focus idiom for every list in the app was
    /// worth more than this list having a motion of its own.
    ///
    /// Held off the card's edges by `CARD_MENU_BAND_INSET`, which is the room that pop grows
    /// into — see `ui::widgets::card_menu_row_rect`.
    ///
    /// `panel_h` is the panel tile's own height, so the panel's local y maps to
    /// `card.bottom() - (panel_h - y)`. `rows_top` is the rows overlay's current panel-local
    /// top — passed in rather than recomputed because the overlay *slides* during the rise
    /// (see `compose_grid`), and the band has to ride with it or it sits on the resting row
    /// while the labels are still travelling.
    pub(crate) fn card_menu_band(&self, card: Rect, panel_h: u32, rows_top: i32) -> Option<Rect> {
        let menu = self.card_menu.as_ref()?;
        // Panel-local: the rows block starts at `rows_top`, and the band is that block's row
        // rect — one function with the tile that draws the labels into it.
        let count = self.card_menu_row_count(&menu.pin_id);
        let block = Rect::new(0, rows_top, card.width(), ui::widgets::card_menu_rows_h(count));
        let row = ui::widgets::card_menu_row_rect(block, menu.focused);
        // Only the bottom can be clipped: the block hangs off the revealed window's top edge,
        // so every row sits below it and a row the rise has not reached yet runs off the
        // panel's bottom. `compose_card_strip` crops the tile from y=0 on that basis.
        let visible_bottom = row.bottom().min(panel_h as i32);
        if visible_bottom <= row.y() {
            return None;
        }
        Some(Rect::new(
            card.x() + row.x(),
            card.bottom() - (panel_h as i32 - row.y()),
            row.width(),
            (visible_bottom - row.y()) as u32,
        ))
    }
}
