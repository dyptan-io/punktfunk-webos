//! Pointer input: what the Magic Remote's cursor is over, and what a click there does.
//!
//! The webOS remote is a pointer as well as a d-pad, so every focusable widget needs a
//! hit test beside its `MenuEvent` handler. Both entry points ([`App::handle_mouse_motion`]
//! and [`App::handle_mouse_click`]) are one `match self.nav.screen`, and each arm measures
//! against the same `app::view` geometry the painter draws with — hover previews exactly
//! what a click will hit because neither side derives the rects itself.
//!
//! Split out of `app/mod.rs`, which held this next to the state machine and the whole
//! render path.

use std::time::Instant;

use crate::app::nav::ScreenKey;
use crate::app::screens::rowbuttons::RowButton;
use crate::app::{view, App, ConnectTarget, HomeFocus, PairingFocus, Screen};
use crate::ui;
use crate::ui::render::Rect;

/// What a hover moved. `any` is whether anything visible changed at all; `row` is whether
/// the *row* under the pointer changed. Stepping onto a button a row carries (its ⋯, a
/// collection's drag/rename/remove) leaves the row itself put, and a row that keeps focus
/// must not replay its focus pop — the same rule the D-pad's `step_row_button` follows, and
/// the sidebar follows when focus moves between a host row and its own ⋯ button.
#[derive(Clone, Copy)]
struct HoverChange {
    any: bool,
    row: bool,
}

impl HoverChange {
    const NONE: Self = Self { any: false, row: false };

    /// A hover that either moved the row or moved nothing.
    const fn row(changed: bool) -> Self {
        Self {
            any: changed,
            row: changed,
        }
    }

    /// A hover on a row list, where the row and the button it carries move independently.
    const fn split(row: bool, button: bool) -> Self {
        Self {
            any: row || button,
            row,
        }
    }
}

impl App {
    /// Handed to `SDL_SetTextInputRect` by the render loop. `None` off the text forms, which
    /// are the only screens that take text input at all.
    pub fn address_field_rect(&self, screen_w: u32, screen_h: u32, _fonts: &ui::text::Fonts) -> Option<Rect> {
        let form = self.text_form()?;
        let l = crate::app::draw::form::layout(
            &self.fonts,
            screen_w as f32,
            screen_h as f32,
            crate::app::draw::scale(screen_h),
            &form.subtitle,
            form.hint.is_some(),
            self.keyboard_shown,
        );
        Some(l.field_rect())
    }

    /// Updates focus/hover to whatever the Magic Remote's pointer is over, returning
    /// whether that changed anything visible — Magic Remote pointer mode fires
    /// `MouseMotion` continuously while moving, so callers redraw only when this is
    /// `true` rather than on every event.
    pub fn handle_mouse_motion(
        &mut self,
        x: i32,
        y: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> bool {
        // A press already landed on a slider track (see `handle_mouse_click`) — every motion
        // until release drags that thumb, rather than re-hit-testing the row list under a
        // pointer that may have wandered off it.
        if self.settings_ui.slider_drag {
            self.drag_slider(x, screen_w, screen_h, fonts);
            return true;
        }
        let focus = self.hover_focus_at(x, y, screen_w, screen_h, fonts);
        // Parity with the D-pad: a hover that moves modal focus to another row replays the
        // focus-pop zoom (and shows the new row's caption). Home drives its own `focus_anim`
        // instead, so it's excluded. An open dropdown is excluded too — hover there only
        // moves the option cursor, so popping the parent row (as the D-pad also declines to)
        // is wrong.
        if focus.row && !matches!(self.nav.screen, Screen::Home) {
            self.render.modal.focus_anim = Some(Instant::now());
        }
        let close_changed = self.hover_close_at(x, y, screen_w, screen_h, fonts);
        focus.any || close_changed
    }

    /// Button index under `(x, y)` for a two-button confirm modal with `subtitle`, or
    /// `None` off both buttons — every confirm modal's hover arm shares this, against the
    /// same `confirm_dialog_layout` geometry the modal is drawn with.
    pub(crate) fn confirm_button_at(
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
        subtitle: &str,
        x: i32,
        y: i32,
    ) -> Option<usize> {
        let (_, content) = ui::tiles::confirm_dialog_layout(screen_w, screen_h, fonts, subtitle);
        ui::tiles::confirm_button_at(content, x, y)
    }

    /// Rect of confirm button `index` for a two-button modal with `subtitle` — the shared
    /// geometry the focused-button tile and its hit-rect are positioned against.
    pub(crate) fn confirm_focus_button_rect(
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
        subtitle: &str,
        index: usize,
    ) -> Rect {
        let (_, content) = ui::tiles::confirm_dialog_layout(screen_w, screen_h, fonts, subtitle);
        ui::widgets::confirm_button_rect(content, index)
    }

    /// Moves the positional focus/selection onto whatever interactive element sits
    /// under the pointer, so the Magic Remote's pointer highlights elements on hover
    /// exactly where a click would land. Returns whether the selection actually
    /// moved. Hovering empty space (gaps, row padding, the area between rows) leaves
    /// the current selection put rather than clearing it, so a resting pointer never
    /// fights the D-pad.
    fn hover_focus_at(&mut self, x: i32, y: i32, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> HoverChange {
        match self.nav.screen {
            // The held card's submenu takes hover whole, like an open dropdown does — the
            // grid behind it must not steal focus out from under an open menu.
            Screen::Home if self.card_menu.is_some() => {
                let Some(row) = self.card_menu_row_at(x, y, screen_w, screen_h) else {
                    return HoverChange::NONE;
                };
                let menu = self.card_menu.as_mut().expect("guarded by the arm");
                let changed = menu.focused != row;
                menu.focus(row);
                HoverChange::row(changed)
            }
            Screen::Home => {
                // The ⋯ button sits inside its row, so it's tested first — same order
                // as `handle_mouse_click`, so hover previews exactly what a click hits.
                if let Some(idx) =
                    view::sidebar::hit_test_menu_button(x, y, self.hosts.entries.len(), self.sidebar_len(), screen_h)
                {
                    return HoverChange::row(self.set_home_focus(HomeFocus::SidebarMenu(idx)));
                }
                if let Some(idx) = view::sidebar::hit_test_row(x, y, self.sidebar_len(), screen_h) {
                    return HoverChange::row(self.set_home_focus(HomeFocus::Sidebar(idx)));
                }
                let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
                let columns = view::home::grid_columns(available_w);
                if let Some(idx) = self.hit_test_grid_card(x, y, columns, available_w) {
                    // Padding after a partial pinned row isn't a real card — nothing to land on.
                    if self.is_grid_card(idx, columns) {
                        return HoverChange::row(self.set_home_focus(HomeFocus::Grid(idx)));
                    }
                }
                HoverChange::NONE
            }
            // Dropdown case already handled above.
            // The two scrolling lists share one hit test and one cursor lookup.
            Screen::Collections => {
                // A held row follows the d-pad, not the pointer: hovering elsewhere must not
                // drag the cursor out from under it.
                if self.screens.collections.dragging.is_some() {
                    return HoverChange::NONE;
                }
                let Some(row) = self.scroll_list_row_at(x, y, screen_w, screen_h) else {
                    return HoverChange::NONE;
                };
                let button = self.scroll_list_row_button_at(x, y, screen_w, screen_h);
                let key = ScreenKey::Collections;
                let row_changed = self.nav.cursor(key) != row;
                let button_changed = self.screens.row_button != button;
                self.nav.set_cursor(key, row);
                self.screens.row_button = button;
                HoverChange::split(row_changed, button_changed)
            }
            Screen::SettingsPage => {
                let l = crate::app::draw::settings::layout(
                    screen_w as f32,
                    screen_h as f32,
                    crate::app::draw::scale(screen_h),
                );
                if let Some(i) = l.entry_at(x, y) {
                    let was = (self.screens.settings_page.page, self.screens.settings_page.column);
                    self.screens.settings_page.column = true;
                    self.show_page(crate::app::state::settingspage::Page::ALL[i]);
                    return HoverChange::row(was != (self.screens.settings_page.page, true));
                }
                let Some(i) = self.kit_list_row_at(x, y) else {
                    return HoverChange::NONE;
                };
                let changed = self.nav.cursor(ScreenKey::SettingsPage) != i || self.screens.settings_page.column;
                self.screens.settings_page.column = false;
                self.nav.set_cursor(ScreenKey::SettingsPage, i);
                HoverChange::row(changed)
            }
            // A list drawn on the kit: hover focuses the row under the pointer, through the
            // list's own last-drawn rects (`app::draw::list`).
            screen @ (Screen::HostMenu | Screen::HostPower) => {
                let Some(i) = self.kit_list_row_at(x, y) else {
                    return HoverChange::NONE;
                };
                let key = ScreenKey::of(screen);
                let changed = self.nav.cursor(key) != i;
                self.nav.set_cursor(key, i);
                HoverChange::row(changed)
            }
            // Identical row-list geometry; only which focus field they carry differs.
            Screen::HdrCalibration => {
                let Some((row, button)) = self.list_modal_row_button_at(x, y, screen_w, screen_h, fonts) else {
                    return HoverChange::NONE;
                };
                // Same per-screen field table the keyboard path indexes, so hover and
                // D-pad focus can never name different fields.
                let Some(focused) = self.list_modal_focused_mut() else {
                    return HoverChange::NONE;
                };
                let row_changed = *focused != row;
                *focused = row;
                let button_changed = self.screens.row_button != button;
                self.screens.row_button = button;
                HoverChange::split(row_changed, button_changed)
            }
            Screen::Pairing => {
                let l = self.pair_layout(screen_w, screen_h);
                if l.on_button(x, y) {
                    let changed = self.screens.pairing_focus != PairingFocus::RequestAccess;
                    self.screens.pairing_focus = PairingFocus::RequestAccess;
                    HoverChange::row(changed)
                } else if let Some(i) = l.digit_at(x, y) {
                    let changed = self.screens.pairing_focus != PairingFocus::Pin || self.screens.pin_digit_index != i;
                    self.screens.pairing_focus = PairingFocus::Pin;
                    self.screens.pin_digit_index = i;
                    HoverChange::row(changed)
                } else {
                    HoverChange::NONE
                }
            }
            // Two-button confirm modals (Forget/SendLogs/Wake/finished SpeedTest — the same
            // modal type as the in-stream Disconnect dialog): hovering a button focuses it,
            // so the pointer can pick action-vs-Cancel, not just confirm whatever the D-pad
            // last focused. `confirm_subtitle` is `None` for the variants with no buttons up
            // (a Wake with no MAC, a test still running), which reads as nothing to hover.
            screen if crate::app::draw::ported(screen) => {
                let Some(i) = self.dialog_layout(screen_w, screen_h).and_then(|l| l.button_at(x, y)) else {
                    return HoverChange::NONE;
                };
                HoverChange::row(self.set_confirm_focused(i))
            }
            Screen::ForgetHost
            | Screen::SendLogs
            | Screen::Wake
            | Screen::SpeedTest
            | Screen::RemoveCollection
            | Screen::ResetHdrCalibration => {
                let Some(subtitle) = self.confirm_subtitle() else {
                    return HoverChange::NONE;
                };
                let Some(i) = Self::confirm_button_at(screen_w, screen_h, fonts, &subtitle, x, y) else {
                    return HoverChange::NONE;
                };
                HoverChange::row(self.set_confirm_focused(i))
            }
            // No positional focus to move: single-card info/entry modals (AddHost,
            // EditHost, About) and Settings with a dropdown open.
            _ => HoverChange::NONE,
        }
    }

    /// The open scrolling list's display-row index under the pointer, using the same animated
    /// `modal_scroll_px` the rows render with — a fixed-offset hit-test drifts a row off once
    /// the list has scrolled. `None` outside the viewport, in a row gap, or off the family.
    pub(crate) fn scroll_list_row_at(&self, x: i32, y: i32, screen_w: u32, screen_h: u32) -> Option<usize> {
        let (content, scroll_px) = self.scroll_list_content_scroll(screen_w, screen_h)?;
        Self::row_at(content, self.scroll_list_row_count(), scroll_px, x, y)
    }

    /// The open scrolling list's content viewport and its current animated scroll offset —
    /// the shared geometry the hit test and `scroll_list_row_rect`'s lookup both index into,
    /// so a scrolled list can't put them at odds.
    fn scroll_list_content_scroll(&self, screen_w: u32, screen_h: u32) -> Option<(Rect, i32)> {
        let (_, content) = self.scroll_list_layout(self.nav.screen, screen_w, screen_h)?;
        let stride = ui::widgets::focus_row_stride() as i32;
        let total = self.scroll_list_row_count();
        Some((content, self.clamped_scroll_px(total, stride, content.height())))
    }

    /// The Bitrate row's rect and track — `settings_focused` is already that row (set by
    /// whatever press started the drag), so this is the one geometry lookup both the arming
    /// click and every later drag motion need.
    /// The calibration row's rect and slider track, taken from the row itself so they are the
    /// rects the renderer drew and not a second guess at them. Its buttons come from
    /// `row_button_at`, like every other row's.
    fn hdr_row_and_track(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<(Rect, Rect)> {
        let rows = self.list_modal_rows()?;
        let row = rows.get(view::hdrcalibration::ROW_SLIDER)?;
        let rect = ui::widgets::focus_row_rect(
            self.modal_list_content(screen_w, screen_h, fonts),
            view::hdrcalibration::ROW_SLIDER,
        );
        Some((rect, ui::widgets::row_geom(rect, row).track))
    }

    /// Drags whichever slider the armed press landed on to `x` — the one place that knows which
    /// screen's slider that is, shared by the arming press and every motion after it.
    fn drag_slider(&mut self, x: i32, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) {
        if self.nav.screen == Screen::HdrCalibration {
            if let Some((_, track)) = self.hdr_row_and_track(screen_w, screen_h, fonts) {
                self.set_hdr_fraction(track_fraction(x, track));
            }
        }
    }

    /// `(row index, which of its trailing buttons)` under the pointer on the host menu.
    /// Hover and click both go through this, so hovering previews exactly what clicking will
    /// do — a click on a row's ⋯ opens that instead of the row's own action, the same split
    /// as a sidebar host row's button.
    fn list_modal_row_button_at(
        &self,
        x: i32,
        y: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<(usize, Option<RowButton>)> {
        let row = self.modal_list_row_at(x, y, screen_w, screen_h, fonts)?;
        let (_, content) = self.modal_list_geometry(screen_w, screen_h, fonts)?;
        let button = self.row_button_at(row, ui::widgets::focus_row_rect(content, row), x, y);
        Some((row, button))
    }

    /// The same, on a scrolling list — measured at the animated scroll offset the rows are
    /// drawn at, so a button is clickable exactly where it looks.
    fn scroll_list_row_button_at(&self, x: i32, y: i32, screen_w: u32, screen_h: u32) -> Option<RowButton> {
        let (content, scroll_px) = self.scroll_list_content_scroll(screen_w, screen_h)?;
        let row = self.scroll_list_row_at(x, y, screen_w, screen_h)?;
        self.row_button_at(row, ui::widgets::focus_row_rect_at_px(content, row, scroll_px), x, y)
    }

    /// The list-modal row index under the pointer. `None` on a screen with no row list, or
    /// when the pointer misses every row. Measured off `modal_list_geometry` — the viewport
    /// the painter draws into — so hover and click land on the rows that are on screen.
    pub(crate) fn modal_list_row_at(
        &self,
        x: i32,
        y: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<usize> {
        let (_, content) = self.modal_list_geometry(screen_w, screen_h, fonts)?;
        // A non-scrolling list modal is its rows' exact height, so the count comes off the
        // viewport rather than a per-screen table — a second table is a second thing to keep
        // in step — and its scroll offset is always zero.
        let rows = (content.height() / ui::widgets::focus_row_stride()) as usize;
        Self::row_at(content, rows, 0, x, y)
    }

    /// Which of `rows` rows in a `content` viewport `(x, y)` is on at `scroll_px`, if any.
    /// One hit test for both list families: the plain modals pass a zero offset, the
    /// scrolling ones the animated one their rows are drawn at — a fixed-offset test on a
    /// scrolled list picks the wrong row.
    fn row_at(content: Rect, rows: usize, scroll_px: i32, x: i32, y: i32) -> Option<usize> {
        if !content.contains_point((x, y)) {
            return None;
        }
        (0..rows).find(|&r| {
            let rect = ui::widgets::focus_row_rect_at_px(content, r, scroll_px);
            // Clipped edge rows aren't hoverable: a focused row composites on its own
            // unclipped tile, so hovering one would pop it outside the card.
            rect.y() >= content.y() && rect.bottom() <= content.bottom() && rect.contains_point((x, y))
        })
    }

    fn hover_close_at(&mut self, x: i32, y: i32, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> bool {
        if let Some(on_close) = self.ported_close_hit(x, y, screen_w, screen_h) {
            return self.set_hover_close(on_close);
        }
        let Some(card) = self.modal_card_rect(screen_w, screen_h, fonts) else {
            // Home draws no close button, but `hover_close` is only ever set true by a
            // modal branch — without clearing it on the way back to Home it stayed stuck
            // `true` forever (nothing on Home reset it), and `handle_mouse_click`'s
            // `if self.render.hover_close { return self.back() }` then swallowed every Home
            // click. Not reported as a visible change: Home draws no close button.
            self.render.hover_close = false;
            return false;
        };
        self.set_hover_close(ui::widgets::modal_close_rect(card).contains_point((x, y)))
    }

    /// Updates `hover_close` and reports whether it actually changed — every modal
    /// screen's close-button hover check in `handle_mouse_motion` follows this same
    /// shape.
    pub(crate) fn set_hover_close(&mut self, hover_close: bool) -> bool {
        let changed = hover_close != self.render.hover_close;
        self.render.hover_close = hover_close;
        changed
    }

    /// A pointer click confirms whatever's currently hovered/focused, or triggers
    /// Back if the modal's close (X) button itself is what's hovered.
    pub fn handle_mouse_click(
        &mut self,
        x: i32,
        y: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<ConnectTarget> {
        // Re-sync the close-button hover to the click's own position first — a
        // MouseButtonDown can carry a slightly different (x, y) than the last
        // MouseMotion (the physical button press can jostle the remote a little).
        self.handle_mouse_motion(x, y, screen_w, screen_h, fonts);
        if self.render.hover_close {
            // Same "what Back means here" as everywhere else — see `back`'s docs.
            return self.back(screen_w, screen_h, fonts);
        }
        // Unlike hover, a click DOES move `home_focus`/`settings_focused` — fresh at
        // the click's own position, so it confirms what was actually clicked rather
        // than whatever the keyboard/remote last focused elsewhere. Each arm only
        // *places* focus; the shared `press` below is what confirms it, so a click and an OK
        // press act alike.
        //
        // A click that lands on no row of an open list leaves the focus where it is and
        // confirms *that* — the pointer is often nowhere near what the user is looking at
        // (the wheel scrolls without the cursor following, and a hand holding the remote
        // drifts off the panel), so a press off the rows means "take the highlighted one".
        // Dismissing is Back's job, and the modal's close button's.
        match self.nav.screen {
            Screen::Home if self.card_menu.is_some() => {
                // The held card's submenu is over the grid: a click either picks one of its
                // rows or dismisses it. Nothing underneath is reachable while it is up.
                // Mid-reorder the panel is collapsed and there are no rows: the click means
                // "leave it there", exactly as an OK press does.
                if self.fix_card_position() {
                    return None;
                }
                if let Some(row) = self.card_menu_row_at(x, y, screen_w, screen_h) {
                    if let Some(menu) = self.card_menu.as_mut() {
                        menu.focus(row);
                    }
                }
            }
            Screen::Home => {
                // The ⋯ button sits inside its row, so it has to be tested first or the
                // click just reads as a click on the host.
                if let Some(idx) =
                    view::sidebar::hit_test_menu_button(x, y, self.hosts.entries.len(), self.sidebar_len(), screen_h)
                {
                    self.set_home_focus(HomeFocus::SidebarMenu(idx));
                    self.open_host_menu(idx);
                    return None;
                }
                if let Some(idx) = view::sidebar::hit_test_row(x, y, self.sidebar_len(), screen_h) {
                    self.set_home_focus(HomeFocus::Sidebar(idx));
                } else {
                    let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
                    let columns = view::home::grid_columns(available_w);
                    // Clicked empty space — either between cards (`?`'s early
                    // `None`) or the padding after a partial pinned row.
                    let idx = self.hit_test_grid_card(x, y, columns, available_w)?;
                    if !self.is_grid_card(idx, columns) {
                        return None;
                    }
                    self.set_home_focus(HomeFocus::Grid(idx));
                }
            }
            Screen::Collections => {
                // A click is one of the inputs that drops a held row — and only that.
                if self.screens.collections.dragging.is_some() {
                    self.commit_collection_drag();
                    return None;
                }
                if let Some(row) = self.scroll_list_row_at(x, y, screen_w, screen_h) {
                    self.nav.set_cursor(ScreenKey::Collections, row);
                    self.screens.row_button = self.scroll_list_row_button_at(x, y, screen_w, screen_h);
                } else {
                    // No row under the pointer, so no trailing button either: the press is on
                    // the focused row itself.
                    self.screens.row_button = None;
                }
            }
            Screen::Pairing => {
                // The Magic Remote pointer is the most reliable input on this TV, so the
                // "Request access" button is clickable directly: focus it and confirm. A
                // digit box focuses that digit; a press then steps it, as OK does.
                let l = self.pair_layout(screen_w, screen_h);
                if l.on_button(x, y) {
                    self.screens.pairing_focus = PairingFocus::RequestAccess;
                } else {
                    let i = l.digit_at(x, y)?;
                    self.screens.pairing_focus = PairingFocus::Pin;
                    self.screens.pin_digit_index = i;
                }
            }
            Screen::SettingsPage => {
                let l = crate::app::draw::settings::layout(
                    screen_w as f32,
                    screen_h as f32,
                    crate::app::draw::scale(screen_h),
                );
                if let Some(i) = l.entry_at(x, y) {
                    self.show_page(crate::app::state::settingspage::Page::ALL[i]);
                    self.screens.settings_page.column = false;
                    return None;
                }
                let row = self.kit_list_row_at(x, y)?;
                self.screens.settings_page.column = false;
                self.nav.set_cursor(ScreenKey::SettingsPage, row);
            }
            // A click on a kit list picks the row under it; off the rows it confirms the
            // focused one, as an OK press does.
            screen @ (Screen::HostMenu | Screen::HostPower) => {
                if let Some(row) = self.kit_list_row_at(x, y) {
                    self.nav.set_cursor(ScreenKey::of(screen), row);
                }
            }
            // The one row is a track and a button, so a press is one or the other. Only the
            // button falls through to `press` below, which is what advances the step.
            Screen::HdrCalibration => {
                let (_, button) = self.list_modal_row_button_at(x, y, screen_w, screen_h, fonts)?;
                // Focus follows the click, exactly as it does on a collection row's buttons.
                self.screens.row_button = button;
                if button.is_none() {
                    let (row_rect, track) = self.hdr_row_and_track(screen_w, screen_h, fonts)?;
                    if on_track(x, y, track, row_rect) {
                        self.settings_ui.slider_drag = true;
                        self.drag_slider(x, screen_w, screen_h, fonts);
                    }
                    return None;
                }
            }
            // Nothing positional to hit: the confirm dialogs confirm whichever button
            // already has focus.
            Screen::Wake
            | Screen::ForgetHost
            | Screen::SpeedTest
            | Screen::SendLogs
            | Screen::RemoveCollection
            | Screen::ResetHdrCalibration
            | Screen::DeleteProfile => {}
            // Nothing clickable but the close button (handled above).
            Screen::AddHost | Screen::EditHost | Screen::RenameCollection | Screen::RenameProfile | Screen::About => {
                return None
            }
        }
        self.press(screen_w, screen_h, fonts)
    }
}

/// Whether a press at `(x, y)` counts as landing on `track`. The full row height counts, not
/// just the thin track: vertical precision on a slider isn't worth demanding of a Magic Remote
/// pointer.
fn on_track(x: i32, y: i32, track: Rect, row_rect: Rect) -> bool {
    x >= track.x() && x < track.right() && y >= row_rect.y() && y < row_rect.bottom()
}

/// Where `x` sits along `track`, as 0..1. Shared by the press that arms a drag and every
/// motion after it, so the thumb lands under the cursor exactly where the track was drawn.
fn track_fraction(x: i32, track: Rect) -> f32 {
    ((x - track.x()) as f32 / track.width().max(1) as f32).clamp(0.0, 1.0)
}
