//! Home screen logic: sidebar/grid navigation, host selection, game library fetch,
//! launching. Grid pixel geometry (rect helpers) lives in `app::view::home`.
use crate::app::grid::{GridCard, GridLayout};
use crate::app::hosts::HostEntry;
use crate::app::state::addhost::AddHostState;
use crate::app::view;
use crate::app::App;
use crate::app::ConnectTarget;
use crate::core::event::MenuEvent;
use crate::core::screen::{HomeFocus, Screen, SettingsScope};
use crate::services::store::{self};
use crate::ui;
use std::time::Instant;

/// Home's two focus containers. Rows and their ⋯ buttons share one: they are laterally
/// paired, so Left off a ⋯ must reach *its own* row rather than the group's remembered
/// entry point. Column-stickiness when walking the ⋯ column upward falls out of the
/// rects instead — the button above is cross-axis aligned, the row body above isn't.
const GROUP_SIDEBAR: u8 = 0;
const GROUP_GRID: u8 = 1;

impl App {
    /// Total sidebar nav positions: host rows + "+ Add host" + "Settings".
    pub(crate) fn sidebar_len(&self) -> usize {
        self.entries.len() + 2
    }

    /// Grid shape at `columns` columns — plain field reads (see [`App::desktop_pin`]), so a
    /// caller in a loop may still prefer to hoist it.
    pub(crate) fn grid_layout(&self, columns: usize) -> GridLayout {
        GridLayout::new(self.pinned_count, self.desktop_pin, self.games_loaded, columns)
    }

    /// Total grid nav positions — `0` (no cards at all) only when no host is
    /// selected yet, or one's selected but hasn't answered a library fetch yet.
    pub(crate) fn grid_len(&self, columns: usize) -> usize {
        if self.selected_host.is_none() {
            return 0;
        }
        self.grid_layout(columns).len(self.games.len())
    }

    /// The grid's vertical section shape at `columns` columns — what every card rect and the
    /// scroll extent are offset by (see `GridLayout::sections`). Callers already holding a
    /// `GridLayout` should ask it directly rather than rebuild one through here.
    pub(crate) fn grid_sections(&self, columns: usize) -> view::home::GridSections {
        self.grid_layout(columns).sections(self.games.len())
    }

    /// The card at grid index `idx`, or `None` for the padding after a partial
    /// pinned row, or out of range.
    pub(crate) fn grid_card_at(&self, idx: usize, columns: usize) -> Option<GridCard<'_>> {
        self.grid_layout(columns).card_at(&self.games, idx)
    }

    /// The pin id for whatever's at grid index `idx` — see `GridLayout::pin_id_at`.
    pub(crate) fn pin_id_at_grid_idx(&self, idx: usize, columns: usize) -> Option<&str> {
        self.grid_layout(columns).pin_id_at(&self.games, idx)
    }

    /// Inverse of `pin_id_at_grid_idx`: grid index for a pin ID, keeping focus after reorder.
    pub(crate) fn grid_idx_for_pin_id(&self, id: &str, columns: usize) -> Option<usize> {
        self.grid_layout(columns).idx_for_pin_id(&self.games, id)
    }

    /// Whether grid index `idx` is an actual card rather than empty padding
    /// after a partial pinned row.
    pub(crate) fn is_grid_card(&self, idx: usize, columns: usize) -> bool {
        self.grid_card_at(idx, columns).is_some()
    }

    pub(crate) fn sidebar_index_for_selected(&self) -> usize {
        self.sidebar_index_of_selected_host().unwrap_or(0)
    }

    /// Like `sidebar_index_for_selected`, but `None` both when nothing is selected
    /// and when the selected host has since dropped out of `entries` — a caller
    /// highlighting the active row must not fall back to row 0 in that case.
    pub(crate) fn sidebar_index_of_selected_host(&self) -> Option<usize> {
        let (h, p) = self.selected_host.as_ref()?;
        self.entries.iter().position(|e| e.host() == h && e.port() == *p)
    }
    /// Everything focusable on Home, as rects in screen space at the *target* scroll
    /// (not the eased `grid_scroll` — navigation should chase where the grid is going,
    /// not where it currently is). Rebuilt per d-pad press from the same geometry the
    /// draw path uses, so a layout hole simply has no item and no direction can land
    /// on it.
    fn home_focus_map(&self, columns: usize, screen_w: u32, screen_h: u32) -> ui::focus::FocusMap<HomeFocus> {
        let host_count = self.entries.len();
        let sidebar_len = self.sidebar_len();
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);

        let mut map = ui::focus::FocusMap::default();
        map.group(
            GROUP_SIDEBAR,
            ui::focus::Wrap::Vertical,
            HomeFocus::Sidebar(self.sidebar_index_for_selected()),
            HomeFocus::Sidebar(0),
        );
        map.group(
            GROUP_GRID,
            ui::focus::Wrap::None,
            HomeFocus::Grid(self.grid.focus_last),
            HomeFocus::Grid(0),
        );

        // One split for the whole sidebar — the same `Vec<Rect>` the painter and both hit
        // tests read, so focus can't disagree with what is on screen.
        for (index, row) in view::sidebar::nav_rows(sidebar_len, screen_h).into_iter().enumerate() {
            let has_menu = index < host_count;
            if has_menu {
                map.item(
                    HomeFocus::SidebarMenu(index),
                    ui::widgets::sidebar_menu_button_rect(row),
                    GROUP_SIDEBAR,
                );
            }
            map.item(
                HomeFocus::Sidebar(index),
                view::sidebar::row_body_rect(row, has_menu),
                GROUP_SIDEBAR,
            );
        }

        // One layout for the whole sweep: `is_grid_card`/`unscrolled_card_rect` would
        // each rebuild it — and rescan the host's pin list — for every card.
        let layout = self.grid_layout(columns);
        let sections = layout.sections(self.games.len());
        let grid_len = if self.selected_host.is_some() {
            layout.len(self.games.len())
        } else {
            0
        };
        // Only the cards a single move can actually reach (see `view::home::focus_window`) —
        // one item per card in the library made every keypress allocate and rescan the whole
        // list to answer a question about its immediate neighbours.
        let focus_window = view::home::focus_window(
            grid_len,
            columns,
            available_w,
            sections,
            self.grid.scroll_target,
            screen_h as i32,
            match self.home_focus {
                HomeFocus::Grid(i) => Some(i),
                HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_) => None,
            },
        );
        for idx in focus_window {
            if layout.card_at(&self.games, idx).is_none() {
                continue;
            }
            let r =
                view::home::unscrolled_card_rect(idx, columns, ui::widgets::SIDEBAR_W as i32, available_w, sections);
            // Screen space, at the scroll the grid is easing *toward* — so that a move
            // crossing into the sidebar compares against its rows on equal terms.
            map.item(HomeFocus::Grid(idx), r.offset(0, -self.grid.scroll_target), GROUP_GRID);
        }
        map
    }

    /// Moves Home's focus one step in `dir`: one spatial lookup, with the sidebar and
    /// grid containers deciding where a move that crosses between them lands (see
    /// `home_focus_map`). Layout holes, the pinned-row split and the row/⋯ split all
    /// fall out of the rects themselves.
    fn navigate_home(&mut self, dir: ui::focus::Dir, screen_w: u32, screen_h: u32) {
        let columns = view::home::grid_columns(screen_w.saturating_sub(ui::widgets::SIDEBAR_W));
        let Some(next) = self
            .home_focus_map(columns, screen_w, screen_h)
            .navigate(self.home_focus, dir)
        else {
            return;
        };
        self.home_focus = next;
        if let HomeFocus::Grid(idx) = next {
            self.grid.focus_last = idx;
            self.ensure_grid_visible(idx, columns, screen_w, screen_h);
        }
    }

    /// Handles one menu event on the Home screen (sidebar + grid). Returns a
    /// `ConnectTarget` when a grid card is confirmed.
    pub fn handle_home_event(&mut self, ev: MenuEvent, screen_w: u32, screen_h: u32) -> Option<ConnectTarget> {
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);

        // The held card's submenu owns every key while it is up — it sits over one card,
        // so moving Home's focus underneath it would leave it pointing at another game.
        if self.handle_card_menu_event(ev, screen_w, screen_h) {
            return None;
        }
        if let Some(dir) = crate::app::menu::nav_dir(ev) {
            self.navigate_home(dir, screen_w, screen_h);
            return None;
        }
        match ev {
            MenuEvent::Confirm => match self.home_focus {
                HomeFocus::Sidebar(i) if i < self.entries.len() => {
                    self.confirm_sidebar_host(i);
                }
                HomeFocus::Sidebar(i) if i == self.entries.len() => {
                    self.add_host = AddHostState::default();
                    self.screen = Screen::AddHost;
                }
                HomeFocus::Sidebar(_) => {
                    self.screen = Screen::Settings(SettingsScope::Global);
                    self.dropdown = None;
                    self.settings_focused = 0;
                    self.scroll = ui::scroll::ScrollWindow::new();
                    self.content_window = ui::scroll::ContentWindow::new();
                }
                HomeFocus::SidebarMenu(i) => self.open_host_menu(i),
                HomeFocus::Grid(i) => self.confirm_grid_card(i, columns),
            },
            // Forgets the focused host (removes its persisted entry/fingerprint —
            // it'll reappear as "not paired" if still discoverable on the LAN).
            MenuEvent::Secondary => {
                if let HomeFocus::Sidebar(i) = self.home_focus {
                    if i < self.entries.len() {
                        self.forget_host(i);
                    }
                }
            }
            // Back on Home is owned by `App::back` (grid/⋯ → sidebar), reached via
            // `dispatch_menu_event` before this handler — never routed here.
            // The four directions returned above, through `menu::nav_dir`.
            MenuEvent::Back | MenuEvent::Up | MenuEvent::Down | MenuEvent::Left | MenuEvent::Right => {}
        }
        None
    }

    /// Pin ID of focused grid card, or `None` for sidebar/padding.
    pub(crate) fn focused_pin_id(&self, columns: usize) -> Option<&str> {
        match self.home_focus {
            HomeFocus::Grid(idx) => self.pin_id_at_grid_idx(idx, columns),
            HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_) => None,
        }
    }

    /// Toggles focused card pin state and reorders the grid; opens pin-limit
    /// alert on overflow.
    pub(crate) fn toggle_focused_pin(&mut self, screen_w: u32, screen_h: u32) {
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        let HomeFocus::Grid(old_idx) = self.home_focus else {
            return;
        };
        let Some(id) = self.pin_id_at_grid_idx(old_idx, columns).map(str::to_string) else {
            return;
        };
        let Some(known) = self.selected_known_host() else {
            return;
        };
        if !known.can_toggle_pin(&id) {
            // At MAX_PINNED_GAMES already — explain instead of a silent no-op.
            self.open_pin_limit();
            return;
        }
        let was_pinned = known.is_pinned(&id);

        let Some(known) = self.selected_known_host_mut() else {
            return;
        };
        known.toggle_pin(&id);
        self.persist();

        self.reorder_games_by_pin();
        if let Some(new_idx) = self.grid_idx_for_pin_id(&id, columns) {
            self.home_focus = HomeFocus::Grid(new_idx);
            self.grid.focus_last = new_idx;
            self.ensure_grid_visible(new_idx, columns, screen_w, screen_h);
        }
        self.replay_reorder_pop(&id, was_pinned, columns);
    }

    /// Reorder's appear animation — the same "every card pops in together" look
    /// as a fresh library reveal (see `app::spinner::GridReveal`), scoped to what
    /// actually needs it: the newly pinned card alone (top row — an already-
    /// pinned card that just changed order doesn't replay), plus every card in
    /// the unpinned "rest" section, which reshuffles regardless of direction.
    /// Card tiles themselves need no rebuilding either way — they're keyed by
    /// pin id (see `card_tiles`), which reordering never changes.
    fn replay_reorder_pop(&mut self, id: &str, was_pinned: bool, columns: usize) {
        let now = Instant::now();
        let layout = self.grid_layout(columns);
        // Driven off what is rasterized, not off the library: a card outside the scroll window
        // has no pop on screen to replay, and `prepare_grid` arms its clock when it is built.
        // Off the whole library this put one entry per game into `card_pop` — a map
        // `tick_animations` scans every frame, and which eviction never reaches for a card it
        // holds no tile for. The pinned block is the grid's first `front_count` indices, so
        // "in the rest section" is decidable from a set bounded by `MAX_PINNED_GAMES`.
        let pinned: std::collections::HashSet<&str> = (0..layout.front_count)
            .filter_map(|idx| layout.pin_id_at(&self.games, idx))
            .collect();
        let rest_ids: Vec<String> = self
            .grid
            .card_ids
            .pin_ids()
            .filter(|id| !pinned.contains(id))
            .map(str::to_string)
            .collect();
        // Re-arm the pop clock unconditionally (not gated on a built tile like the old
        // per-`CardTile` clock): a not-yet-built card has no visible pop to replay, and
        // its clock is overwritten with a fresh one when `prepare_grid` builds it.
        if !was_pinned {
            self.grid.card_pop.insert(id.to_string(), now);
        }
        for pin_id in rest_ids {
            self.grid.card_pop.insert(pin_id, now);
        }
    }

    /// Re-sorts games: pinned first (in pin order), rest untouched. A pin for a game
    /// not currently listed just doesn't sort — it is *not* dropped here, because this
    /// runs on every pin toggle and a host that failed to answer has an empty
    /// `self.games`. Dropping is [`App::prune_stale_game_prefs`]'s job.
    pub(crate) fn reorder_games_by_pin(&mut self) {
        let Some(known_idx) = self.selected_known_host_idx() else {
            self.clear_grid_pins();
            return;
        };
        self.desktop_pin = self.known_hosts[known_idx].is_pinned(store::DESKTOP_PIN_ID);
        let pinned_ids: Vec<String> = self.known_hosts[known_idx]
            .pinned_ids()
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut pinned = Vec::new();
        for id in &pinned_ids {
            // Desktop isn't in `self.games`, so it never sorts here.
            if let Some(pos) = self.games.iter().position(|g| &g.id == id) {
                pinned.push(self.games.remove(pos));
            }
        }
        self.pinned_count = pinned.len();
        pinned.append(&mut self.games);
        self.games = pinned;
    }

    /// Forgets the pin state the grid is drawn from — for the paths that drop the library
    /// itself, where there is no host left to recompute it from.
    fn clear_grid_pins(&mut self) {
        self.pinned_count = 0;
        self.desktop_pin = false;
    }

    /// Drops per-game state (pins *and* settings overrides) for games the host no longer
    /// lists — otherwise a removed game keeps counting toward `MAX_PINNED_GAMES` and its
    /// overrides linger forever.
    ///
    /// Call only from the success arm of [`App::drain_games`]: `self.games` is empty
    /// whenever a fetch failed or a host is unreachable, and pruning against that would
    /// wipe everything the user configured the moment their host went to sleep.
    pub(crate) fn prune_stale_game_prefs(&mut self) {
        let Some(known_idx) = self.selected_known_host_idx() else {
            return;
        };
        let live: std::collections::HashSet<&str> = self.games.iter().map(|g| g.id.as_str()).collect();
        if self.known_hosts[known_idx].prune_games(|id| live.contains(id)) {
            self.persist();
        }
    }

    fn selected_known_host_idx(&self) -> Option<usize> {
        self.selected_host
            .as_ref()
            .and_then(|(h, p)| self.known_hosts.iter().position(|k| k.host == *h && k.port == *p))
    }

    /// Eased 0..=1 progress of pin id `id`'s zoom-in (see `card_pop`)
    /// — 1.0, full size, for anything not animating.
    pub(crate) fn card_pop_frac(&self, id: &str) -> f32 {
        ui::animation::anim_frac(self.grid.card_pop.get(id).copied(), crate::app::CARD_POP)
    }

    /// The largest useful `grid_scroll` for the current library/layout — 0 when
    /// everything already fits on screen.
    pub(crate) fn max_grid_scroll(&self, columns: usize, available_w: u32, screen_h: u32) -> i32 {
        let viewport_h = screen_h as i32 - view::home::GRID_PAD - view::home::GRID_TOP_Y;
        let extra = self.grid_sections(columns).total_extra();
        (view::home::grid_layer_height(self.grid_len(columns), columns, available_w) as i32 + extra
            - 2 * view::home::GRID_LAYER_PAD
            - viewport_h)
            .max(0)
    }

    /// Scrolls the grid (via `grid_scroll_target` — the rendered offset eases
    /// toward it, see `tick_animations`) just far enough that focused card `idx`,
    /// including its focus-ring halo, will be fully on screen; also starts the
    /// focus pop, since this is called on exactly the moves that change grid
    /// focus. Clamped to the grid's real extent.
    pub(crate) fn ensure_grid_visible(&mut self, idx: usize, columns: usize, screen_w: u32, screen_h: u32) {
        /// Focus ring + `inflate` overhang around a focused card, plus a little
        /// breathing room.
        const FOCUS_MARGIN: i32 = 16;
        self.focus_anim = Some(Instant::now());
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let card = self.unscrolled_card_rect(idx, columns, ui::widgets::SIDEBAR_W as i32, available_w);
        let viewport = (view::home::GRID_TOP_Y, screen_h as i32 - view::home::GRID_PAD);
        let max_scroll = self.max_grid_scroll(columns, available_w, screen_h);
        self.grid.scroll_target =
            ui::scroll::scroll_to_reveal(card, viewport, self.grid.scroll_target, FOCUS_MARGIN).clamp(0, max_scroll);
    }

    /// Scrolls the grid by `dy_px` (positive = content moves up), clamped — the
    /// Magic Remote's scroll wheel on the Home screen. Returns whether the target
    /// actually moved (drives redraw; the eased offset follows in
    /// `tick_animations`).
    pub fn scroll_grid_by(&mut self, dy_px: i32, screen_w: u32, screen_h: u32) -> bool {
        if self.selected_host.is_none() {
            return false;
        }
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        let max_scroll = self.max_grid_scroll(columns, available_w, screen_h);
        let next = (self.grid.scroll_target + dy_px).clamp(0, max_scroll);
        let changed = next != self.grid.scroll_target;
        self.grid.scroll_target = next;
        changed
    }
    pub(crate) fn confirm_sidebar_host(&mut self, idx: usize) {
        let entry = self.entries[idx].clone();
        match entry {
            HostEntry::Known(h) if h.is_paired() => {
                let (host, port, mgmt_port) = (h.host, h.port, h.mgmt_port);
                // Re-confirming the already-active host refreshes its library too — a
                // user clicking it is asking to see the current game list, e.g. after
                // installing something new on the host.
                self.select_host(host, port, mgmt_port);
            }
            _ => self.open_pairing(idx),
        }
    }

    /// Drops the selected host and everything drawn from its library — the grid, its art and any
    /// in-flight fetch. Whatever removed the host from the sidebar (Forget) must call this, or
    /// its grid stays on screen with no row to go back to.
    pub(crate) fn clear_selected_host(&mut self) {
        self.selected_host = None;
        self.games = Vec::new();
        self.games_loaded = false;
        self.clear_grid_pins();
        self.art.clear();
        self.art_loader = None;
        self.games_rx = None;
        self.home_status = None;
        self.home_status_sticky = false;
        self.home_focus = HomeFocus::Sidebar(0);
        self.grid.focus_last = 0;
        self.grid.dirty = true;
    }

    /// Selects host and kicks off async library fetch; avoids blocking the UI thread (used to freeze input).
    pub(crate) fn select_host(&mut self, host: String, port: u16, mgmt_port: Option<u16>) {
        self.selected_host = Some((host.clone(), port));
        self.persist();
        let name = self
            .known_hosts
            .iter()
            .find(|h| h.host == host && h.port == port)
            .map_or_else(|| host.clone(), |h| h.name.clone());
        self.home_status = Some(format!("Loading library from {name}…"));
        // Picking a host is the user's own action, so its progress replaces whatever the last
        // launch left on screen. The reload `App::new` starts is not this — it runs before
        // `home_status_sticky` is ever set.
        self.home_status_sticky = false;
        self.games = Vec::new();
        self.clear_grid_pins();
        self.games_loaded = false;
        self.art.clear();
        // Dropping the loader stops its worker (its request channel closes), so a host
        // switch abandons in-flight fetches for the previous library.
        self.art_loader = None;
        // Focus stays on the sidebar until `drain_games` has cards to land on: `navigate`
        // can't move off a key with no rect, so an empty grid would kill the d-pad.
        self.grid.focus_last = 0;
        self.sidebar_dirty = true;
        self.grid.dirty = true;
        self.grid.scroll = 0;
        self.grid.scroll_target = 0;

        let identity = (self.identity.0.clone(), self.identity.1.clone());
        let known = self.known_hosts.iter().find(|h| h.host == host && h.port == port);
        let fingerprint = known.and_then(|k| k.fingerprint);
        let mgmt_port = mgmt_port.unwrap_or(crate::services::library::DEFAULT_MGMT_PORT);
        tracing::debug!("library: fetching from {host}:{mgmt_port}…");
        self.games_rx = Some(crate::services::library::load_games_async(
            host,
            port,
            mgmt_port,
            identity,
            fingerprint,
        ));
    }

    /// Whether `select_host`'s library fetch is still out. Deliberately not `!games_loaded`:
    /// that stays `false` down the `Unreachable` path too, which would leave the grid's
    /// loading spinner running forever behind the Wake dialog.
    pub(crate) fn library_fetch_in_flight(&self) -> bool {
        self.games_rx.is_some()
    }

    /// Drains `select_host`'s library fetch; switching hosts aborts old fetches safely.
    pub fn drain_games(&mut self) -> bool {
        let Some(rx) = &self.games_rx else { return false };
        let Ok(loaded) = rx.try_recv() else { return false };
        self.games_rx = None;
        let crate::services::library::GamesLoaded {
            host,
            port,
            mgmt_port,
            result,
        } = loaded;
        match result {
            Ok(mut games) => {
                // The host returns its own scan order, which is neither stable nor
                // meaningful to a reader. On a TV the grid is navigated a card at a time
                // with a d-pad, so alphabetical is the difference between "find the game"
                // and "sweep the whole library". Case-insensitive so casing doesn't
                // scatter otherwise-adjacent titles.
                games.sort_by_key(|g| g.title.to_lowercase());
                tracing::info!("library: {} games from {host}:{mgmt_port}", games.len());
                let identity = (self.identity.0.clone(), self.identity.1.clone());
                let known = self.known_hosts.iter().find(|h| h.host == host && h.port == port);
                let fingerprint = known.and_then(|k| k.fingerprint);
                // Covers are requested per card as the grid window reaches them (see
                // `App::prepare_tiles`), not fetched for the whole library up front.
                self.art_loader = Some(crate::services::art::ArtLoader::spawn(
                    host,
                    port,
                    mgmt_port,
                    identity,
                    fingerprint,
                    self.grid.card_size,
                ));
                self.games = games;
                self.games_loaded = true;
                // Hand the grid the focus `select_host` held back — only if the user hasn't
                // navigated off that row, so a late fetch can't yank them.
                if matches!(self.home_focus, HomeFocus::Sidebar(i) if Some(i) == self.sidebar_index_of_selected_host())
                {
                    self.home_focus = HomeFocus::Grid(0);
                }
                if !self.home_status_sticky {
                    self.home_status = None;
                }
                // Held until now rather than shown at startup: the hint points at the cards,
                // so it waits for a library to exist. Spent on sight, whatever happens next.
                if std::mem::take(&mut self.intro_hint_owed) {
                    self.home_status = Some(crate::app::state::cardmenu::INTRO_HINT.to_string());
                }
                self.prune_stale_game_prefs();
                self.reorder_games_by_pin();
            }
            Err(e) => {
                tracing::warn!("library fetch failed ({host}:{mgmt_port}): {e}");
                self.handle_library_error(host, port, &e);
            }
        }
        self.grid.dirty = true;
        true
    }

    /// Shared handling for a failed library fetch/reachability check, used by both
    /// `drain_games` and `drain_launch_check`. `Unreachable` opens the Wake dialog
    /// (even with no MAC on record — `start_wake`/`render_wake` just hide the send
    /// controls then); `NotPaired`/`PinMismatch`/`Http` mean the host answered, so
    /// Wake-on-LAN wouldn't help — those stay a plain status line.
    pub(crate) fn handle_library_error(&mut self, host: String, port: u16, e: &crate::services::library::LibraryError) {
        let reason = e.to_string();
        // A live problem with the host beats the previous launch's reason for bouncing.
        self.home_status_sticky = false;
        if matches!(e, crate::services::library::LibraryError::Unreachable(_)) {
            let mac = self
                .known_hosts
                .iter()
                .find(|h| h.host == host && h.port == port)
                .map(|h| h.mac.clone())
                .unwrap_or_default();
            self.start_wake(host, port, mac, reason);
        } else {
            // The host answered — just not with a usable library — so Desktop is a
            // legitimate fallback here, unlike the `Unreachable` branch above.
            self.games_loaded = true;
            self.home_status = Some(reason);
        }
    }
    /// Confirms grid card, arming the launch straight away so the connect thread starts on the
    /// click and overlaps the zoom. No pre-flight reachability probe: it cost a whole mTLS
    /// round trip before the handshake, and connect reports an unreachable host itself (the
    /// host list probes on selection, which is where the Wake dialog comes from).
    pub(crate) fn confirm_grid_card(&mut self, idx: usize, columns: usize) {
        if self.launch_ready.is_some() || self.launch_anim.is_some() {
            return;
        }
        let Some((host, port)) = self.selected_host.clone() else {
            return;
        };
        let Some(known) = self.known_hosts.iter().find(|h| h.host == host && h.port == port) else {
            return;
        };
        // The pin is also the pair state: no pin means the host was never paired, so there is
        // nothing to connect with.
        let Some(fingerprint) = known.fingerprint else {
            return;
        };
        let launch = match self.grid_card_at(idx, columns) {
            Some(GridCard::Desktop) => None,
            Some(GridCard::Game(game)) => Some(game.id.clone()),
            None => return,
        };
        // The loading screen's backdrop. Desktop has no art, so it keeps the plain fade to
        // black — as does a game with none, which hands straight over to the stream. Armed
        // before the art is handed over, so `Hero::accept` recognises a cache hit as belonging
        // to this launch even if the focus prefetch never ran.
        let game = launch.as_ref().and_then(|id| self.games.iter().find(|g| &g.id == id));
        self.hero.arm(launch.clone());
        if let (Some(game), Some(loader)) = (game, &mut self.art_loader) {
            // The disk is only touched when the focus prefetch hasn't already decoded this
            // hero — re-reading it would be several MB of nothing, and that is the common case.
            let in_hand = self.hero.image_for(&game.id).is_some()
                || loader
                    .cached_hero(&game.id)
                    .is_some_and(|image| self.hero.accept(game.id.clone(), image));
            if !in_hand && (game.art.hero.is_some() || game.art.header.is_some()) {
                // Asked for here as well as on focus, since a card can be confirmed before the
                // prefetch got round to it. The fetch overlaps the connect, and only *this*
                // case — art that isn't on this TV yet — is worth holding the hand-off for.
                loader.request_hero(game);
                self.hero.await_art();
            }
        }
        tracing::debug!("launch: connecting to {host}:{port} now, zoom runs in parallel");
        // The zoom and the fade to black say the launch started; a status line under them
        // only competes with that. Cleared so the last launch's failure doesn't sit under
        // this one either.
        self.home_status = None;
        self.home_status_sticky = false;
        self.launch_anim_idx = Some(idx);
        self.launch_ready = Some(ConnectTarget {
            host,
            port,
            fingerprint,
            launch,
        });
        // Not `grid_dirty`: contents are unchanged, and dirtying rebuilds every card tile and
        // re-arms the loading spinner right as the zoom starts.
        self.sidebar_dirty = true;
    }

    /// Takes the `ConnectTarget` `confirm_grid_card` armed, if any — the runtime's tick loop
    /// calls this and breaks its event loop with it to actually start the stream.
    pub fn take_ready_launch(&mut self) -> Option<ConnectTarget> {
        self.launch_ready.take()
    }
    pub(crate) fn forget_host(&mut self, idx: usize) {
        let HostEntry::Known(h) = &self.entries[idx] else {
            return;
        };
        let (host, port) = (h.host.clone(), h.port);
        self.known_hosts.retain(|k| !(k.host == host && k.port == port));
        crate::services::art::reconcile_host_caches(&self.known_hosts);
        self.rebuild_entries();
        if self.selected_host.as_ref() == Some(&(host, port)) {
            self.clear_selected_host();
        }
        // After the selection clear: persisting first would leave `selected_host` in the document
        // pointing at the host just forgotten.
        self.persist();
        let sidebar_len = self.sidebar_len();
        if let HomeFocus::Sidebar(i) = &mut self.home_focus {
            if *i >= sidebar_len {
                *i = sidebar_len - 1;
            }
        }
        self.sidebar_dirty = true;
        self.grid.dirty = true;
    }
}
