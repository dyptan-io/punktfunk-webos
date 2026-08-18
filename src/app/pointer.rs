//! Pointer input: what the Magic Remote's cursor is over, and what a click there does.
//!
//! The webOS remote is a pointer as well as a d-pad, so every focusable widget needs a
//! hit test beside its `MenuEvent` handler. Both entry points ([`App::handle_mouse_motion`]
//! and [`App::handle_mouse_click`]) are one `match self.screen`, and each arm measures
//! against the same `app::view` geometry the painter draws with — hover previews exactly
//! what a click will hit because neither side derives the rects itself.
//!
//! Split out of `app/mod.rs`, which held this next to the state machine and the whole
//! render path.
// A glob for the same reason `app::render`'s modules use one: this is an `impl App` block
// lifted out of `app/mod.rs`.
use crate::app::*;

impl App {
    /// Handed to `SDL_SetTextInputRect` by the render loop.
    pub fn address_field_rect(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Rect {
        let (_, subtitle) = self.address_copy();
        view::addhost::field_rect(screen_w, screen_h, fonts, &subtitle, self.keyboard_shown)
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
        // A press already landed on the Bitrate track (see `handle_mouse_click`) — every
        // motion until release drags the thumb, rather than re-hit-testing the row list
        // under a pointer that may have wandered off it.
        if self.slider_drag {
            self.drag_bitrate_slider(x, screen_w, screen_h);
            return true;
        }
        let focus_changed = self.hover_focus_at(x, y, screen_w, screen_h, fonts);
        // Parity with the D-pad: a hover that moves modal focus replays the focus-pop zoom
        // (and shows the new row's caption). Home drives its own `focus_anim` instead, so
        // it's excluded. An open dropdown is excluded too — hover there only moves the
        // option cursor, so popping the parent row (as the D-pad also declines to) is wrong.
        if focus_changed && self.dropdown.is_none() && !matches!(self.screen, Screen::Home) {
            self.modal_focus_anim = Some(Instant::now());
        }
        let close_changed = self.hover_close_at(x, y, screen_w, screen_h, fonts);
        focus_changed || close_changed
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
    fn hover_focus_at(&mut self, x: i32, y: i32, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> bool {
        // An open dropdown overlays the row list — hover moves its option cursor and
        // nothing behind it. Shared by whichever screen owns the dropdown (Settings or
        // Diagnostics), and uses the same overlay geometry the renderer draws against.
        if let Some(i) = self.dropdown_option_at(x, y, screen_w, screen_h, fonts) {
            let dd = self
                .dropdown
                .as_mut()
                .expect("dropdown_option_at yields Some only when one is open");
            let changed = dd.focused != i;
            dd.focused = i;
            return changed;
        }
        // A dropdown open but not hovered still swallows hover — the row list behind
        // it must not take the selection.
        if self.dropdown.is_some() {
            return false;
        }
        match self.screen {
            // The held card's submenu takes hover whole, like an open dropdown does — the
            // grid behind it must not steal focus out from under an open menu.
            Screen::Home if self.card_menu.is_some() => {
                let Some(row) = self.card_menu_row_at(x, y, screen_w, fonts) else {
                    return false;
                };
                let menu = self.card_menu.as_mut().expect("guarded by the arm");
                let changed = menu.focused != row;
                menu.focus(row);
                changed
            }
            Screen::Home => {
                // The ⋯ button sits inside its row, so it's tested first — same order
                // as `handle_mouse_click`, so hover previews exactly what a click hits.
                if let Some(idx) =
                    view::sidebar::hit_test_menu_button(x, y, self.entries.len(), self.sidebar_len(), screen_h)
                {
                    return self.set_home_focus(HomeFocus::SidebarMenu(idx));
                }
                if let Some(idx) = view::sidebar::hit_test_row(x, y, self.sidebar_len(), screen_h) {
                    return self.set_home_focus(HomeFocus::Sidebar(idx));
                }
                let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
                let columns = view::home::grid_columns(available_w);
                if let Some(idx) = view::home::hit_test_grid_card(
                    x,
                    y,
                    columns,
                    self.grid_len(columns),
                    ui::widgets::SIDEBAR_W as i32,
                    available_w,
                    self.grid_scroll,
                ) {
                    // Padding after a partial pinned row isn't a real card — nothing to land on.
                    if self.is_grid_card(idx, columns) {
                        return self.set_home_focus(HomeFocus::Grid(idx));
                    }
                }
                false
            }
            // Dropdown case already handled above.
            Screen::Settings(_) => {
                let Some(row) = self.settings_row_at(x, y, screen_w, screen_h) else {
                    return false;
                };
                let changed = self.settings_focused != row;
                self.settings_focused = row;
                changed
            }
            Screen::HostMenu => {
                let Some((i, dots)) = self.host_menu_row_at(x, y, screen_w, screen_h, fonts) else {
                    return false;
                };
                let changed = self.menu_focused != i || self.host_menu_dots != dots;
                self.menu_focused = i;
                self.host_menu_dots = dots;
                changed
            }
            // Identical row-list geometry; only which focus field they carry differs.
            Screen::Diagnostics | Screen::Experimental | Screen::CursorSettings => {
                let Some(row) = self.modal_list_row_at(x, y, screen_w, screen_h, fonts) else {
                    return false;
                };
                // Same per-screen field table the keyboard path indexes, so hover and
                // D-pad focus can never name different fields.
                let Some(focused) = self.list_modal_focused_mut() else {
                    return false;
                };
                let changed = *focused != row;
                *focused = row;
                changed
            }
            Screen::Pairing => {
                let card = view::pairing::card_rect(screen_w, screen_h, fonts);
                if view::pairing::request_button_rect(card, fonts).contains_point((x, y)) {
                    let changed = self.pairing_focus != PairingFocus::RequestAccess;
                    self.pairing_focus = PairingFocus::RequestAccess;
                    changed
                } else {
                    false
                }
            }
            // Two-button confirm modals (Forget/SendLogs/Wake/finished SpeedTest — the same
            // modal type as the in-stream Disconnect dialog): hovering a button focuses it,
            // so the pointer can pick action-vs-Cancel, not just confirm whatever the D-pad
            // last focused. `confirm_subtitle` is `None` for the variants with no buttons up
            // (a Wake with no MAC, a test still running), which reads as nothing to hover.
            Screen::ForgetHost | Screen::SendLogs | Screen::Wake | Screen::SpeedTest => {
                let Some(subtitle) = self.confirm_subtitle() else {
                    return false;
                };
                let Some(i) = Self::confirm_button_at(screen_w, screen_h, fonts, &subtitle, x, y) else {
                    return false;
                };
                self.set_confirm_focused(i)
            }
            // No positional focus to move: single-card info/entry modals (AddHost,
            // EditHost, About, WakeSettings, PinLimit) and Settings with a dropdown open.
            _ => false,
        }
    }

    /// Sets `home_focus`, reporting whether it actually moved — the hover/click
    /// helpers redraw only on a real change.
    fn set_home_focus(&mut self, focus: HomeFocus) -> bool {
        let changed = self.home_focus != focus;
        self.home_focus = focus;
        // The card's zoom, glow and title wipe all run off `focus_anim`, which the D-pad
        // arms in `ensure_grid_visible`; armed here for every pointer path at once, or
        // landing on a card with the Magic Remote renders it already finished. Only on a
        // change: the pointer streams motion events and each would restart the clock.
        if changed && matches!(focus, HomeFocus::Grid(_)) {
            self.focus_anim = Some(Instant::now());
        }
        changed
    }

    /// The `(content viewport, pixel scroll offset)` an open dropdown anchors its
    /// option overlay to, matching what `draw_list` renders so hit-testing lands
    /// exactly where options are drawn. `None` for a screen with no dropdown.
    pub(crate) fn dropdown_geom(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<(Rect, i32)> {
        match self.screen {
            // Anchored to the animated offset (`settings_content_scroll`) so an open
            // dropdown stays attached to its row while the list is still settling.
            Screen::Settings(_) => Some(self.settings_content_scroll(screen_w, screen_h)),
            // Diagnostics doesn't scroll, so 0.
            Screen::Diagnostics => Some((self.modal_list_geometry(screen_w, screen_h, fonts)?.1, 0)),
            _ => None,
        }
    }

    /// The Settings display-row index under the pointer, using the same animated
    /// `modal_scroll_px` the rows render with — a fixed-offset hit-test drifts a row
    /// off once the list has scrolled. `None` outside the viewport or in a row gap.
    pub(crate) fn settings_row_at(&self, x: i32, y: i32, screen_w: u32, screen_h: u32) -> Option<usize> {
        let (content, scroll_px) = self.settings_content_scroll(screen_w, screen_h);
        if !content.contains_point((x, y)) {
            return None;
        }
        let total = menu::settings_row_count(self.row_set());
        (0..total).find(|&r| {
            let rect = ui::widgets::focus_row_rect_at_px(content, r, scroll_px);
            // Clipped edge rows aren't hoverable: a focused row composites on its own
            // unclipped tile, so hovering one would pop it outside the card.
            rect.y() >= content.y() && rect.bottom() <= content.bottom() && rect.contains_point((x, y))
        })
    }

    /// Settings' content viewport and its current animated scroll offset — the shared
    /// geometry `settings_row_at`'s hit test and `settings_row_rect`'s lookup both index
    /// into, so a scrolled list can't put them at odds.
    fn settings_content_scroll(&self, screen_w: u32, screen_h: u32) -> (Rect, i32) {
        let set = self.row_set();
        let (_, content) = view::settings::layout(set, screen_w, screen_h);
        let stride = ui::widgets::focus_row_stride() as i32;
        let total = menu::settings_row_count(set);
        let scroll_px = self
            .modal_scroll_px
            .clamp(0, Self::max_scroll_px(total, stride, content.height()));
        (content, scroll_px)
    }

    /// Display row `row`'s on-screen rect, same animated scroll offset `settings_row_at`
    /// hit-tests against — the geometry the Bitrate drag anchors to.
    fn settings_row_rect(&self, row: usize, screen_w: u32, screen_h: u32) -> Rect {
        let (content, scroll_px) = self.settings_content_scroll(screen_w, screen_h);
        ui::widgets::focus_row_rect_at_px(content, row, scroll_px)
    }

    /// The Bitrate row's rect and track — `settings_focused` is already that row (set by
    /// whatever press started the drag), so this is the one geometry lookup both the arming
    /// click and every later drag motion need.
    fn bitrate_row_and_track(&self, screen_w: u32, screen_h: u32) -> (Rect, Rect) {
        let row_rect = self.settings_row_rect(self.settings_focused, screen_w, screen_h);
        // A marked row's track shifts left (see `row_layout`), so the drag reads the same
        // geometry the draw did rather than deriving its own.
        let marked = menu::override_is_set(&self.editing_override(), menu::ROW_BITRATE);
        (row_rect, ui::widgets::row_layout(row_rect, marked).track)
    }

    /// Sets the Bitrate row from the pointer's current x against its track — shared by the
    /// initial press (which also has to decide whether the click landed on the track at
    /// all) and every drag motion after it.
    fn set_bitrate_from_x(&mut self, x: i32, track: Rect) {
        let fraction = (x - track.x()) as f32 / track.width() as f32;
        menu::set_bitrate_fraction(self.settings_target_mut(), fraction);
        self.capture_game_override(menu::ROW_BITRATE);
    }

    /// Drags the Bitrate slider to `x`.
    fn drag_bitrate_slider(&mut self, x: i32, screen_w: u32, screen_h: u32) {
        let (_, track) = self.bitrate_row_and_track(screen_w, screen_h);
        self.set_bitrate_from_x(x, track);
    }

    /// The dropdown option index under the pointer, if a dropdown is open and the
    /// pointer is over one of its options. Shares `dropdown_geom` +
    /// `ui::widgets::dropdown_option_rect` with the renderer so hover previews exactly what a
    /// click confirms.
    pub(crate) fn dropdown_option_at(
        &self,
        x: i32,
        y: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<usize> {
        let dd = self.dropdown.as_ref()?;
        let (content, scroll_px) = self.dropdown_geom(screen_w, screen_h, fonts)?;
        let overlay = view::settings::dropdown_overlay_rect_at_px(content, dd.row, scroll_px);
        let options_len = self.dropdown_options_len(dd.row);
        (0..options_len).find(|&i| ui::widgets::dropdown_option_rect(overlay, i).contains_point((x, y)))
    }

    /// A click while a dropdown is open: an option under the pointer confirms it,
    /// anything else dismisses (tap-outside-to-close). The hovered option is already
    /// the cursor courtesy of `handle_mouse_motion`.
    fn dropdown_click_event(&self, x: i32, y: i32, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> MenuEvent {
        if self.dropdown_option_at(x, y, screen_w, screen_h, fonts).is_some() {
            MenuEvent::Confirm
        } else {
            MenuEvent::Back
        }
    }

    /// `(row index, on its ⋯ button)` under the pointer on the host menu. Hover and click
    /// both go through this, so hovering previews exactly what clicking will do — a click
    /// on a row's ⋯ opens that instead of the row's own action, the same split as a sidebar
    /// host row's button.
    fn host_menu_row_at(
        &self,
        x: i32,
        y: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<(usize, bool)> {
        let (_, content) = self.modal_list_geometry(screen_w, screen_h, fonts)?;
        let row = Self::list_row_at(content, x, y)?;
        let dots = self.host_menu_row_has_dots()
            && ui::widgets::sidebar_menu_button_rect(ui::widgets::focus_row_rect(content, row)).contains_point((x, y));
        Some((row, dots))
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
        Self::list_row_at(content, x, y)
    }

    /// Which row of a list modal's `content` viewport `(x, y)` is on, if any. The row count
    /// comes from the viewport's own height rather than a per-screen table: the content rect
    /// *is* `row_count` strides tall, and a second table is a second thing to keep in step.
    fn list_row_at(content: Rect, x: i32, y: i32) -> Option<usize> {
        let rows = (content.height() / ui::widgets::focus_row_stride()) as usize;
        (0..rows).find(|&r| ui::widgets::focus_row_rect(content, r).contains_point((x, y)))
    }

    fn hover_close_at(&mut self, x: i32, y: i32, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> bool {
        let Some(card) = self.modal_card_rect(screen_w, screen_h, fonts) else {
            // Home draws no close button, but `hover_close` is only ever set true by a
            // modal branch — without clearing it on the way back to Home it stayed stuck
            // `true` forever (nothing on Home reset it), and `handle_mouse_click`'s
            // `if self.hover_close { return self.back() }` then swallowed every Home
            // click. Not reported as a visible change: Home draws no close button.
            self.hover_close = false;
            return false;
        };
        self.set_hover_close(ui::widgets::modal_close_rect(card).contains_point((x, y)))
    }

    /// Updates `hover_close` and reports whether it actually changed — every modal
    /// screen's close-button hover check in `handle_mouse_motion` follows this same
    /// shape.
    pub(crate) fn set_hover_close(&mut self, hover_close: bool) -> bool {
        let changed = hover_close != self.hover_close;
        self.hover_close = hover_close;
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
        if self.hover_close {
            // Same "what Back means here" as everywhere else — see `back`'s docs.
            return self.back(screen_w, screen_h, fonts);
        }
        // An open dropdown owns the click wherever it landed: an option, or outside it,
        // which closes it. Same as hover.
        if self.dropdown.is_some() {
            let ev = self.dropdown_click_event(x, y, screen_w, screen_h, fonts);
            self.handle_menu_event(ev, screen_w, screen_h, fonts);
            return None;
        }
        // Unlike hover, a click DOES move `home_focus`/`settings_focused` — fresh at
        // the click's own position, so it confirms what was actually clicked rather
        // than whatever the keyboard/remote last focused elsewhere. Each arm only
        // *places* focus (or bails on a click that landed on nothing); the shared
        // `press` below is what confirms it, so a click and an OK press act alike.
        match self.screen {
            Screen::Home if self.card_menu.is_some() => {
                // The held card's submenu is over the grid: a click either picks one of its
                // rows or dismisses it. Nothing underneath is reachable while it is up.
                let Some(row) = self.card_menu_row_at(x, y, screen_w, fonts) else {
                    self.close_card_menu();
                    return None;
                };
                if let Some(menu) = self.card_menu.as_mut() {
                    menu.focus(row);
                }
            }
            Screen::Home => {
                // The ⋯ button sits inside its row, so it has to be tested first or the
                // click just reads as a click on the host.
                if let Some(idx) =
                    view::sidebar::hit_test_menu_button(x, y, self.entries.len(), self.sidebar_len(), screen_h)
                {
                    self.home_focus = HomeFocus::SidebarMenu(idx);
                    self.open_host_menu(idx);
                    return None;
                }
                if let Some(idx) = view::sidebar::hit_test_row(x, y, self.sidebar_len(), screen_h) {
                    self.home_focus = HomeFocus::Sidebar(idx);
                } else {
                    let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
                    let columns = view::home::grid_columns(available_w);
                    // Clicked empty space — either between cards (`?`'s early
                    // `None`) or the padding after a partial pinned row.
                    let idx = view::home::hit_test_grid_card(
                        x,
                        y,
                        columns,
                        self.grid_len(columns),
                        ui::widgets::SIDEBAR_W as i32,
                        available_w,
                        self.grid_scroll,
                    )?;
                    if !self.is_grid_card(idx, columns) {
                        return None;
                    }
                    self.home_focus = HomeFocus::Grid(idx);
                }
            }
            Screen::Settings(_) => {
                // `?` bails if the click hit the gap between rows or outside the
                // viewport — nothing to focus or confirm.
                self.settings_focused = self.settings_row_at(x, y, screen_w, screen_h)?;
                // A press on the Bitrate track sets the value under the cursor directly and
                // arms the drag (see `handle_mouse_motion`) instead of nudging one notch the
                // way `Confirm` below would — a slider is for landing on a value, not stepping
                // to it one click at a time.
                if menu::settings_logical_row(self.row_set(), self.settings_focused) == menu::ROW_BITRATE
                    && menu::row_lock(menu::ROW_BITRATE, self.settings_target(), self.detected_gamepad_type).is_none()
                {
                    let (row_rect, track) = self.bitrate_row_and_track(screen_w, screen_h);
                    // Full row height, not just the thin track — vertical precision on a
                    // slider isn't worth demanding of a Magic Remote pointer.
                    let in_track = x >= track.x() && x < track.right() && y >= row_rect.y() && y < row_rect.bottom();
                    if in_track {
                        self.slider_drag = true;
                        self.set_bitrate_from_x(x, track);
                        return None;
                    }
                }
            }
            Screen::Pairing => {
                // The Magic Remote pointer is the most reliable input on this TV, so the
                // "Request access" button is clickable directly: focus it and confirm.
                let card = view::pairing::card_rect(screen_w, screen_h, fonts);
                if !view::pairing::request_button_rect(card, fonts).contains_point((x, y)) {
                    return None;
                }
                self.pairing_focus = PairingFocus::RequestAccess;
            }
            Screen::HostMenu => {
                let (i, dots) = self.host_menu_row_at(x, y, screen_w, screen_h, fonts)?;
                self.menu_focused = i;
                self.host_menu_dots = dots;
            }
            // One row, already focused — the click only has to have landed on it.
            Screen::WakeSettings => {
                let (_, content) = self.modal_list_geometry(screen_w, screen_h, fonts)?;
                if !ui::widgets::focus_row_rect(content, 0).contains_point((x, y)) {
                    return None;
                }
            }
            // Identical row-list geometry; only which focus field they carry differs.
            Screen::Diagnostics | Screen::Experimental | Screen::CursorSettings => {
                let row = self.modal_list_row_at(x, y, screen_w, screen_h, fonts)?;
                *self.list_modal_focused_mut()? = row;
            }
            // Nothing positional to hit: the confirm dialogs confirm whichever button
            // already has focus, and PinLimit has a single OK.
            Screen::Wake | Screen::ForgetHost | Screen::SpeedTest | Screen::SendLogs | Screen::PinLimit => {}
            // Nothing clickable but the close button (handled above).
            Screen::AddHost | Screen::EditHost | Screen::About => return None,
        }
        self.press(screen_w, screen_h, fonts)
    }
}
