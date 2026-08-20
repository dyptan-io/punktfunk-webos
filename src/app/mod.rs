//! Pre-stream UI: Home screen (sidebar + game grid) with modals (Pairing/Settings/Add-host).
//! `ui.rs` owns drawing/input-mapping, `store.rs` owns persistence, `discovery.rs` owns mDNS.
//!
//! Per-screen `impl App` blocks are split by concern: `state` (event handling, transitions)
//! and `view` (geometry + draw-list building). Keeping them under `app` lets `ui`/`core`
//! stay dependency leaves — neither reaches back into `App`.
pub(crate) mod grid;
pub(crate) mod hero;
pub(crate) mod hosts;
pub(crate) mod menu;
pub(crate) mod modal;
pub(crate) mod nav;
pub(crate) mod pointer;
pub(crate) mod press;
pub(crate) mod render;
pub(crate) mod render_input;
pub(crate) mod spinner;
pub(crate) mod state;
pub(crate) mod view;

use std::time::{Duration, Instant};

use crate::ui::render::Rect;
use anyhow::Result;
use tiny_skia::Pixmap;

use crate::app::grid::GridCard;
use crate::app::hosts::HostEntry;
use crate::app::nav::ScreenKey;
use crate::app::state::addhost::AddHostState;
use crate::core::event::MenuEvent;
pub use crate::core::model::ConnectTarget;
use crate::core::model::GameEntry;
pub use crate::core::screen::{HomeFocus, PairingFocus, Screen};
use crate::services::discovery::Discovery;
use crate::services::store::{self, KnownHost, Settings};
use crate::ui;
use crate::ui::render::TileId;

/// How much a focused grid card grows. Bigger than the modal widgets' pop (they sit
/// in a fixed column where any spill reads as a layout shift); a card has the grid gap
/// around it to grow into.
pub(crate) const CARD_GROWTH: f32 = 0.045;
pub(crate) const LAUNCH_GROWTH: f32 = 3.5;
const PIN_BADGE_MARGIN: i32 = 10;
pub(crate) const CARD_POP: Duration = Duration::from_millis(300);
pub(crate) const CARD_POP_SHRINK: f32 = 0.14;
/// Modal open. Short: the card is the response to a keypress, and delay there reads as
/// a slow TV, not as an animation.
pub(crate) const MODAL_FADE: Duration = Duration::from_millis(75);
/// Modal close. Slower than the open — nothing is waiting on it, and outlasting the
/// incoming card is what makes a modal-to-modal step read as one replacing the other.
pub(crate) const MODAL_FADE_OUT: Duration = Duration::from_millis(150);
pub(crate) const DROPDOWN_FADE: Duration = MODAL_FADE;
/// Transparent margin the modal tile leaves around the card so its drop shadow
/// (`Painter::card_shadow`: blur `SHADOW_BLUR`=14, offset dy 5) fits inside the tile.
/// The tile is sized to the card's bounding box plus this pad rather than the whole
/// screen, so every open rasterizes and uploads a fraction of the pixels it used to.
pub(crate) const MODAL_TILE_PAD: i32 = 24;
pub(crate) const SCROLL_INDICATOR_HOLD: Duration = Duration::from_millis(700);
pub(crate) const SCROLL_INDICATOR_FADE: Duration = Duration::from_millis(350);
pub(crate) const SCROLL_INDICATOR_LIFETIME: Duration =
    Duration::from_millis(SCROLL_INDICATOR_HOLD.as_millis() as u64 + SCROLL_INDICATOR_FADE.as_millis() as u64);
/// Wider than track for rounded caps not to clip.
const SCROLL_INDICATOR_TILE_W: u32 = 10;

/// About document window size (lines). Balances GPU texture height limit vs rebuild hitch.
const ABOUT_WINDOW_BUDGET: usize = 80;
/// Margin (lines) before recentering the baked window.
const ABOUT_WINDOW_MARGIN: usize = 16;

/// Home status bar's vertical padding; box height is fixed at two text rows.
const STATUS_BG_PAD: i32 = 12;

/// WOL packet resend interval; silent-mode timeout before showing prompt.
pub(crate) const WAKE_RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// Reachability recheck interval (independent of WOL timers).
pub(crate) const WAKE_PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// Wake-on-LAN flow state: both interactive prompt and silent background wait.
pub struct WakeState {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) name: String,
    pub(crate) mac: Vec<String>,
    /// Original library error, restored on back-out.
    pub(crate) reason: String,
    pub(crate) focused: usize,
    pub(crate) sent: bool,
    /// Packet count; shown so silent wait visibly progresses.
    pub(crate) attempts: u32,
    pub(crate) since: Option<Instant>,
    pub(crate) last_attempt: Option<Instant>,
    /// `true` while running silently (auto-send before prompt shown).
    pub(crate) silent: bool,
    pub(crate) last_probe: Option<Instant>,
    pub(crate) probe_rx: Option<std::sync::mpsc::Receiver<crate::services::library::GamesLoaded>>,
}

/// Open dropdown on settings modal.
pub struct DropdownState {
    pub row: usize,
    pub focused: usize,
}

pub struct App {
    /// Which screen is up, which was before it, and where the cursor sits on each — see
    /// [`nav::Nav`].
    pub nav: nav::Nav,
    pub known_hosts: Vec<KnownHost>,
    /// `None` if the mDNS daemon didn't start. Owned here so it stops with the menu.
    pub(crate) discovery: Option<crate::services::discovery::Discovery>,
    pub entries: Vec<HostEntry>,
    pub home_focus: HomeFocus,
    /// Whether this TV is webosbrew-rooted — `None` until [`App::start_root_probe`] answers.
    pub(crate) rooted: Option<bool>,
    pub(crate) rooted_rx: Option<std::sync::mpsc::Receiver<bool>>,
    pub selected_host: Option<(String, u16)>,
    pub games: Vec<GameEntry>,
    /// Leading pinned-game entries; kept in pin order.
    pub(crate) pinned_count: usize,
    /// Whether the selected host has its Desktop card pinned. Maintained next to
    /// `pinned_count` by [`App::reorder_games_by_pin`] rather than read back out of the host's
    /// pin map on demand: `grid_layout` is asked for a card rect on every frame and every
    /// pointer motion, and deriving this meant scanning `known_hosts` and a map lookup each
    /// time. Every path that changes a pin, the selected host, or the library goes through that
    /// one function (or clears the grid via [`App::clear_grid_pins`]).
    pub(crate) desktop_pin: bool,
    /// Host answered library fetch (gates Desktop card).
    pub(crate) games_loaded: bool,
    pub(crate) games_rx: Option<std::sync::mpsc::Receiver<crate::services::library::GamesLoaded>>,
    pub home_status: Option<String>,
    /// Whether `home_status` is the reason the last launch bounced back to the menu, and so must
    /// survive the library reload a fresh menu entry starts — that reload clears the status on
    /// success, which wiped the error a second after the user landed back on the grid. Anything
    /// the user's own actions produce replaces it as usual.
    pub(crate) home_status_sticky: bool,
    /// Cover art pixmaps by game id.
    pub art: std::collections::HashMap<String, Pixmap>,
    pub(crate) art_loader: Option<crate::services::art::ArtLoader>,
    /// The connecting screen's backdrop, and every clock it runs on.
    pub(crate) hero: hero::Hero,
    pub(crate) launch_ready: Option<ConnectTarget>,
    pub(crate) launch_anim: Option<Instant>,
    pub(crate) launch_anim_idx: Option<usize>,
    pub settings: Settings,
    /// What `Screen::GameSettings` is editing, `None` when it isn't up — see
    /// `app::state::gamesettings`.
    pub(crate) game_settings: Option<state::gamesettings::GameSettingsState>,
    /// The submenu raised over a held grid card's title strip (Pin/Unpin + Settings), and
    /// the only way to `Screen::GameSettings`. `None` when no card is held open.
    pub card_menu: Option<state::cardmenu::CardMenu>,
    /// Whether the card submenu's introduction (`state::cardmenu::INTRO_HINT`) is still owed —
    /// set on the first launch of a build that stamped a new version into the document, spent
    /// on the status line as soon as a library has actually landed to hold the cards it talks
    /// about.
    pub(crate) intro_hint_owed: bool,
    /// Persists settings off UI thread to avoid blocking.
    pub(crate) state_writer: store::StateWriter,
    /// The attached pad's type per `gamepad::detect_type`, refreshed on hotplug in
    /// `runtime::ui_flow`. `None` with no pad attached or an unrecognized one. Only meaningful
    /// when `settings.gamepad_type` is `Auto` — an explicit pick doesn't need this to know what
    /// it's driving, but the Controller row's `DualSense` caption does (see `settings_rows`).
    pub(crate) detected_gamepad_type: Option<store::GamepadType>,
    /// Whether the mouse button is down on the Settings screen's slider row (Bitrate) with
    /// the press having landed on the track itself — while `true`, `MouseMotion` drags the
    /// thumb to the pointer's x instead of just moving hover focus. Cleared on
    /// `MouseButtonUp`; never survives a screen change since the button can't be released
    /// on another screen from webOS's own D-pad OK -> click translation.
    pub(crate) slider_drag: bool,
    /// Scroll state for overflowing modal content.
    pub(crate) scroll: ui::scroll::ScrollWindow,
    /// Settings' scroll position, stashed while About borrows `scroll` for its
    /// own document — restored on return so the focus highlight doesn't end up
    /// outside the visible rows.
    pub(crate) settings_scroll: ui::scroll::ScrollWindow,
    /// Window slice of baked About document.
    pub(crate) content_window: ui::scroll::ContentWindow,
    pub dropdown: Option<DropdownState>,
    /// Dropdown overlay's own open/close fade, payload `(row, focused)` so the
    /// close-fade can still draw it after `dropdown` goes `None`.
    pub(crate) dropdown_fade: ui::fade::ModalFade<(usize, usize)>,
    /// The sidebar row `Screen::ForgetHost` is confirming forgetting — set
    /// alongside `screen = Screen::ForgetHost` (see `App::open_forget_host`),
    /// `None` otherwise.
    pub host_menu_index: Option<usize>,
    /// Whether focus is on the ⋯ button of the host menu's focused row rather than on
    /// the row body — the list-modal counterpart of `HomeFocus::SidebarMenu`. Only the
    /// "Wake host" row has one (see `host_menu_actions`).
    pub host_menu_dots: bool,
    /// Delivers the background log upload's result; `None` when no upload is in
    /// flight. Drained each tick by `drain_send_logs`.
    pub(crate) send_logs_rx: Option<std::sync::mpsc::Receiver<crate::app::state::sendlogs::SendLogsMsg>>,
    /// The sidebar row `Screen::EditHost` is editing, `None` otherwise.
    pub edit_host_index: Option<usize>,
    /// The in-flight/finished speed test, `None` when that screen isn't open.
    pub(crate) speed_test: Option<crate::app::state::speedtest::SpeedTestState>,
    /// Delivers the background probe's progress/result — dropping it cancels.
    pub(crate) speed_test_rx: Option<std::sync::mpsc::Receiver<crate::app::state::speedtest::SpeedTestMsg>>,
    /// The host being measured, for the status line.
    pub speed_test_name: String,
    /// Last known reachability per `(host, port)` — see `app::reach`.
    pub(crate) reachable: std::collections::HashMap<(String, u16), bool>,
    pub(crate) reach_rx: Option<std::sync::mpsc::Receiver<crate::app::state::reach::Reachability>>,
    pub(crate) reach_last: Option<Instant>,
    /// Whether webOS's on-screen keyboard is currently up, polled from
    /// `SDL_IsScreenKeyboardShown` each tick by `main.rs` — it moves the address form out
    /// from under the panel (see `App::keyboard_modal_card`).
    pub keyboard_shown: bool,
    /// The About document's source lines, built once on first open. ~10,000
    /// static string slices; cheap to hold, wasteful to rebuild per frame.
    pub about_lines: Vec<&'static str>,
    /// `about_lines` wrapped to a body width, flattened into one list of visual
    /// lines (see `view::about::wrap_document`) — the unit `scroll`/`content_window`
    /// actually scroll over, since a source line's wrapped length varies and
    /// only the flattened list has a uniform per-unit stride. Keyed by the
    /// body width it was wrapped for, rebuilt if that width changes.
    pub(crate) about_wrapped: Option<(u32, Vec<String>)>,
    pub add_host: AddHostState,
    /// The active "host unreachable — wake it?" prompt/wait, if any — see `WakeState`.
    pub wake: Option<WakeState>,
    /// PIN entry: 4 digits, each 0-9, edited one at a time.
    pub pin_digits: [u8; 4],
    pub pin_digit_index: usize,
    /// Whether the pairing modal's input is on the PIN row or the Request-access button.
    pub pairing_focus: PairingFocus,
    pub pairing_status: Option<String>,
    pub pairing_busy: bool,
    /// Index into `entries` currently being paired — captured when entering
    /// `Screen::Pairing`.
    pub(crate) pairing_entry: usize,
    /// Whether the Magic Remote's pointer is currently hovering a modal's
    /// close (X) button.
    pub hover_close: bool,
    pub(crate) identity: (String, String),
    // -------------------------------------------------------- render clocks --
    /// Bumped every time the sidebar strip is rebuilt, so its cache entry is versioned by
    /// the `sidebar_dirty` flag the event side maintains rather than by hashing every row.
    pub(crate) sidebar_gen: u64,
    // The `Painter` tile cache (`ui::cache::TileStore`) is owned by the render loop
    // (`runtime::ui_flow`), not `App` — App keeps only screen state plus the render-facing
    // derived values the event side also needs (see [`grid::GridState`]).
    /// Sidebar row content changed — the sidebar strip must re-rasterize
    /// (never set on focus movement).
    pub(crate) sidebar_dirty: bool,
    /// Tiles whose GPU texture should be released this frame — drained by `runtime`, which
    /// owns the `Compositor`, before it uploads whatever the same pass rebuilt.
    pub(crate) evicted_tiles: Vec<TileId>,
    /// Everything that only exists while a modal is up — see [`modal::ModalState`].
    pub(crate) modal: modal::ModalState,
    /// The game grid: its tiles, its clocks, its eased scroll and its dirty flags — see
    /// [`grid::GridState`].
    pub(crate) grid: grid::GridState,
    // ------------------------------------------------------------ animations --
    /// When the current grid-focus pop started (card scales in over
    /// `ui::animation::FOCUS_POP` — set on every d-pad focus move).
    pub(crate) focus_anim: Option<Instant>,
    /// When `tick_animations` last ran, so the eased scroll can advance by real elapsed time
    /// instead of by a frame count (see `ui::animation::ease_scroll`). Every other animation
    /// here is already clocked off its own `Instant`.
    last_tick: Option<Instant>,
    /// The pressed button's dip, if one is in flight — purely visual, and only for a
    /// press that stayed on its screen (see `App::press`).
    pub(crate) press: ui::animation::Press,
    /// In-flight PIN-pairing / request-access ceremony, delivering its outcome
    /// from a background thread — the ceremony blocks for up to minutes
    /// (request-access parks until a human approves it on the host), which used
    /// to freeze the whole UI when run inline on this thread. Drained by
    /// `drain_pairing` each tick; dropping the receiver (Back while busy)
    /// cancels: the worker's send fails and it exits.
    pub(crate) pairing_rx: Option<std::sync::mpsc::Receiver<PairingOutcome>>,
}

/// What a finished background pairing/request-access ceremony reports back —
/// everything needed to persist the host on success (captured going in, so the
/// worker doesn't need `App` access).
pub(crate) struct PairingOutcome {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) name: String,
    pub(crate) mgmt_port: Option<u16>,
    pub(crate) mac: Vec<String>,
    /// The host's pinned fingerprint, or a user-displayable error.
    pub(crate) result: Result<[u8; 32], String>,
}

/// The sidebar's saved-host rows.
fn known_entries(known_hosts: &[store::KnownHost]) -> Vec<HostEntry> {
    known_hosts.iter().cloned().map(HostEntry::Known).collect()
}

impl App {
    pub fn new(identity: (String, String)) -> Self {
        let store::Loaded {
            state: loaded,
            new_build,
        } = store::load();
        // The writer's baseline is the document as loaded, so an unchanged launch never writes.
        let state_writer = store::StateWriter::spawn(loaded.clone());
        let store::Persisted {
            settings,
            known_hosts,
            selected_host,
            version: _,
        } = loaded;
        let entries = known_entries(&known_hosts);

        // Catches hosts that left the list while the app was closed (migration, torn document);
        // in-session removals reconcile at their own sites.
        crate::services::art::reconcile_host_caches(&known_hosts);
        let discovery = crate::services::discovery::Discovery::start();
        let mut app = Self {
            nav: nav::Nav::default(),
            known_hosts,
            discovery,
            entries,
            home_focus: HomeFocus::Sidebar(0),
            rooted: None,
            rooted_rx: None,
            selected_host: None,
            games: Vec::new(),
            pinned_count: 0,
            desktop_pin: false,
            games_loaded: false,
            games_rx: None,
            home_status: None,
            home_status_sticky: false,
            art: std::collections::HashMap::new(),
            art_loader: None,
            hero: hero::Hero::default(),
            launch_ready: None,
            launch_anim: None,
            launch_anim_idx: None,
            settings,
            game_settings: None,
            card_menu: None,
            intro_hint_owed: new_build,
            state_writer,
            detected_gamepad_type: None,
            slider_drag: false,
            scroll: ui::scroll::ScrollWindow::new(),
            settings_scroll: ui::scroll::ScrollWindow::new(),
            content_window: ui::scroll::ContentWindow::new(),
            dropdown: None,
            dropdown_fade: ui::fade::ModalFade::new(),
            host_menu_index: None,
            host_menu_dots: false,
            send_logs_rx: None,
            edit_host_index: None,
            speed_test: None,
            speed_test_rx: None,
            speed_test_name: String::new(),
            reachable: Self::new_reachability(),
            reach_rx: None,
            reach_last: None,
            keyboard_shown: false,
            about_lines: Vec::new(),
            about_wrapped: None,
            add_host: AddHostState::default(),
            wake: None,
            pin_digits: [0; 4],
            pin_digit_index: 0,
            pairing_focus: PairingFocus::Pin,
            pairing_status: None,
            pairing_busy: false,
            pairing_entry: 0,
            hover_close: false,
            identity,
            sidebar_dirty: true,
            evicted_tiles: Vec::new(),
            sidebar_gen: 0,
            grid: grid::GridState::default(),
            modal: modal::ModalState::default(),
            focus_anim: None,
            last_tick: None,
            press: ui::animation::Press::default(),
            pairing_rx: None,
        };
        // Restore the last-active sidebar host (if it's still known and paired)
        // so relaunching the app lands back on its game grid.
        if let Some((host, port)) = selected_host {
            if let Some(h) = app
                .known_hosts
                .iter()
                .find(|h| h.host == host && h.port == port && h.is_paired())
            {
                let (host, port, mgmt_port) = (h.host.clone(), h.port, h.mgmt_port);
                app.select_host(host, port, mgmt_port);
            }
        }
        // Rasterizes the spinner's frames off the render thread (OnceLock warm-up).
        // Applies the persisted "Show logs" preference to the otherwise-ephemeral overlay.
        if app.settings.show_logs {
            crate::runtime::set_log_overlay_enabled(true);
        }
        std::thread::spawn(crate::assets::spinner_frames);
        app
    }

    /// Name of the host whichever host-scoped modal (Forget, Wake settings) is acting on.
    pub(crate) fn host_menu_host_name(&self) -> Option<&str> {
        self.host_menu_index
            .and_then(|i| self.entries.get(i))
            .map(HostEntry::name)
    }

    /// Which document the settings-shaped screen that is up is editing. Read off the screen
    /// itself — it and the Cursor sub-screen both carry their scope — so a scratch copy that
    /// outlives its flow can't redirect the global screen's edits into it.
    pub(crate) fn settings_scope(&self) -> menu::SettingsScope {
        match self.nav.screen {
            Screen::Settings(scope) | Screen::CursorSettings(scope) => scope,
            _ => menu::SettingsScope::Global,
        }
    }

    /// The per-game scratch state, but only while a per-game screen is actually up — the one
    /// gate every accessor below shares, in the two forms borrowck needs.
    pub(crate) fn editing_game(&self) -> Option<&state::gamesettings::GameSettingsState> {
        match self.settings_scope() {
            menu::SettingsScope::Game => self.game_settings.as_ref(),
            menu::SettingsScope::Global => None,
        }
    }

    pub(crate) fn editing_game_mut(&mut self) -> Option<&mut state::gamesettings::GameSettingsState> {
        match self.settings_scope() {
            menu::SettingsScope::Game => self.game_settings.as_mut(),
            menu::SettingsScope::Global => None,
        }
    }

    /// The `Settings` the open settings screen is editing: the global document, or the
    /// per-game scratch copy. One accessor so every mutator, lock check and dropdown lookup
    /// in `menu` sees the same value the rows were built from.
    pub(crate) fn settings_target(&self) -> &Settings {
        match self.editing_game() {
            Some(gs) => &gs.merged,
            None => &self.settings,
        }
    }

    /// Spells `editing_game_mut`'s gate out rather than calling it: the fallback arm needs
    /// `self` back, which borrowck won't grant while a returned `Option<&mut _>` is in scope.
    pub(crate) fn settings_target_mut(&mut self) -> &mut Settings {
        let scope = self.settings_scope();
        match &mut self.game_settings {
            Some(gs) if scope == menu::SettingsScope::Game => &mut gs.merged,
            _ => &mut self.settings,
        }
    }

    /// This game's overrides while the per-game screen is up — what decides which rows wear
    /// a "use global" button. Empty everywhere else, so the global screen shows none.
    pub(crate) fn editing_override(&self) -> store::SettingsOverride {
        self.editing_game()
            .map_or_else(store::SettingsOverride::default, |gs| gs.over)
    }

    /// The settings rows, with the platform/hardware facts the view can't reach folded in,
    /// plus the override dot on every row this game differs from the global on.
    pub(crate) fn settings_rows(&self) -> Vec<ui::widgets::FocusRow> {
        let set = self.settings_scope();
        let settings = self.settings_target();
        let effective = if settings.gamepad_type == store::GamepadType::Auto {
            self.detected_gamepad_type.unwrap_or_default()
        } else {
            settings.gamepad_type
        };
        let dualsense_limited = effective.is_dualsense() && !crate::platform::webos::dualsense::hid_playstation_bound();
        let webos_major = crate::platform::webos::device::sdk_version().map(|(major, _)| major);
        let mut rows = view::settings::rows(
            set,
            settings,
            self.detected_gamepad_type,
            dualsense_limited,
            webos_major,
        );
        let over = self.editing_override();
        let focused = self.nav.cursor(ScreenKey::Settings);
        for (display, (row, logical)) in rows
            .iter_mut()
            .zip(menu::settings_visible_logical_rows(set))
            .enumerate()
        {
            menu::decorate_override(row, &over, logical, display == focused);
        }
        rows
    }

    /// Scrolls `settings_focused` into view.
    pub(crate) fn scroll_settings_into_view(&mut self, screen_h: u32) {
        let set = self.settings_scope();
        let visible = view::settings::visible_rows(set, screen_h);
        self.scroll.scroll_into_view(
            self.nav.cursor(ScreenKey::Settings),
            menu::settings_row_count(set),
            visible,
        );
    }

    /// `(row, focused, alpha)` for the open dropdown or its close-fade; `None` if neither.
    pub(crate) fn dropdown_draw_state(&self) -> Option<(usize, usize, f32)> {
        if let Some(dd) = &self.dropdown {
            Some((dd.row, dd.focused, self.dropdown_fade.open_alpha(DROPDOWN_FADE)))
        } else {
            self.dropdown_fade
                .closing_frame(DROPDOWN_FADE)
                .map(|(alpha, (row, focused))| (row, focused, alpha))
        }
    }

    /// Grid geometry bridges — `view::home` is pure geometry, so these supply the two
    /// pieces of live state (the section shape and the scroll offset) it takes.
    pub(crate) fn unscrolled_card_rect(&self, idx: usize, columns: usize, grid_x: i32, available_w: u32) -> Rect {
        view::home::unscrolled_card_rect(idx, columns, grid_x, available_w, self.grid_sections(columns))
    }

    pub(crate) fn scrolled_card_rect(&self, idx: usize, columns: usize, grid_x: i32, available_w: u32) -> Rect {
        view::home::scrolled_card_rect(
            idx,
            columns,
            grid_x,
            available_w,
            self.grid_sections(columns),
            self.grid.scroll,
        )
    }

    /// The grid card under the pointer, if any — see `view::home::card_at_point`, which
    /// honours the section headings and the gap that push the library block down on screen (a
    /// test against the bare grid rect picked the row above).
    pub(crate) fn hit_test_grid_card(&self, x: i32, y: i32, columns: usize, available_w: u32) -> Option<usize> {
        let grid_x = ui::widgets::SIDEBAR_W as i32;
        if x < grid_x {
            return None;
        }
        view::home::card_at_point(
            self.grid_len(columns),
            columns,
            grid_x,
            available_w,
            self.grid_sections(columns),
            (x, y + self.grid.scroll),
        )
    }

    /// Rebuilds the sidebar from `known_hosts`, dropping any discovered-but-unsaved rows. Every
    /// caller that mutates `known_hosts` goes through this rather than collecting the list
    /// itself, so no site has to remember to re-anchor focus.
    pub(crate) fn rebuild_entries(&mut self) {
        self.set_entries(known_entries(&self.known_hosts));
    }

    /// The one place the sidebar row list is replaced: keeps focus on the row the user is on and
    /// marks the layer dirty, neither of which any caller should have to remember.
    fn set_entries(&mut self, entries: Vec<HostEntry>) {
        let before = self.entries.len();
        self.entries = entries;
        self.reanchor_sidebar_focus(before);
        // The sidebar layer is a cached tile keyed by nothing but this flag (see `prepare_tiles`),
        // so a rebuilt row list that doesn't set it leaves the previous host list on screen.
        self.sidebar_dirty = true;
    }

    /// Keeps sidebar focus on the row the user is actually on after the host list changed
    /// length (`before` is what it was). Focus is a flat index over hosts + "Add host" +
    /// "Settings", and the two utility rows are identified purely by their index — see
    /// `compose_sidebar_focus`, which only draws the bottom-pinned highlight for
    /// `entries.len() + 1` — so leaving a stale index there puts "Settings" mid-list.
    fn reanchor_sidebar_focus(&mut self, before: usize) {
        let now = self.entries.len();
        let (HomeFocus::Sidebar(i) | HomeFocus::SidebarMenu(i)) = self.home_focus else {
            return; // grid focus doesn't index the sidebar
        };
        if now == before {
            return;
        }
        // A ⋯ belongs to a host row, so it survives only while that row does.
        if matches!(self.home_focus, HomeFocus::SidebarMenu(_)) && i < now {
            return;
        }
        // Past the hosts are the two utility rows, which move with the list's length; a host
        // index only needs clamping into what's left.
        let i = if i >= before { i - before + now } else { i.min(now) };
        self.home_focus = HomeFocus::Sidebar(i);
    }

    /// Whether `addr:port` already has a sidebar row, saved or merely discovered.
    pub(crate) fn host_listed(&self, addr: &str, port: u16) -> bool {
        self.known_hosts.iter().any(|h| h.host == addr && h.port == port)
            || self
                .entries
                .iter()
                .any(|e| matches!(e, HostEntry::Discovered(d) if d.addr == addr && d.port == port))
    }

    /// Merges freshly-discovered hosts into the entry list (known hosts keep their
    /// paired status; a discovered host not yet known gets appended), learns each
    /// known host's Wake-on-LAN MAC(s) from its live advert while it's awake to
    /// advertise them, and — if a wake is in flight (`self.wake`) — notices when the
    /// waking host reappears on mDNS and reconnects. Returns whether the sidebar
    /// actually changed — `main.rs`'s render loop uses this to skip a redraw when a
    /// discovery tick found nothing new (see its dirty-flag docs).
    pub fn drain_discovery(&mut self) -> bool {
        let before = self.entries.len();
        let mut changed = false;
        let mut mac_learned = false;
        let mut woke = None;
        // `found.addr` throughout this loop is deliberate, not a typo for a nonexistent
        // `found.host` — `DiscoveredHost` (discovery.rs) only has `addr`, `WakeState`/
        // `KnownHost` only have `host`; both hold the same kind of value (network address).
        let polled = self.discovery.as_mut().map(Discovery::poll).unwrap_or_default();
        for found in polled {
            #[allow(clippy::suspicious_operation_groupings)]
            if let Some(w) = &self.wake {
                if found.addr == w.host && found.port == w.port {
                    woke = Some((found.addr.clone(), found.port, found.mgmt_port));
                }
            }
            #[allow(clippy::suspicious_operation_groupings)]
            let known = self
                .known_hosts
                .iter_mut()
                .find(|h| h.host == found.addr && h.port == found.port);
            if let Some(known) = known {
                if !found.mac.is_empty() && known.mac != found.mac {
                    known.mac.clone_from(&found.mac);
                    mac_learned = true;
                }
            }
            if !self.host_listed(&found.addr, found.port) {
                self.entries.push(HostEntry::Discovered(found));
                changed = true;
            }
        }
        if mac_learned {
            self.persist();
        }
        if let Some((host, port, mgmt_port)) = woke {
            self.wake_succeeded(host, port, mgmt_port, "mDNS");
            changed = true;
        }
        if changed {
            // Rows were appended, so the utility rows have moved.
            self.reanchor_sidebar_focus(before);
            self.sidebar_dirty = true;
        }
        changed
    }

    /// Ends an in-flight wake because the host is actually back — whether that was
    /// noticed passively (`drain_discovery` seeing a fresh mDNS resolve) or actively
    /// (`tick_wake`'s reachability probe succeeding). `source` is just for the log line.
    pub(crate) fn wake_succeeded(&mut self, host: String, port: u16, mgmt_port: Option<u16>, source: &str) {
        tracing::info!("wake succeeded: {host}:{port} back ({source})");
        let name = self.wake.take().map(|w| w.name);
        // A wake ends on its own timing, not a keypress, so it dismisses its own modal and
        // nothing else. The selection still moves — that is what the wait was for. Safe under
        // an open modal: the one that writes per-host state off the selection pins its target
        // at open time (`GameSettingsState::host`), and the rest key off `host_menu_index`,
        // which nothing in the background reorders (`drain_discovery` only appends).
        if matches!(self.nav.screen, Screen::Wake) {
            self.nav.screen = Screen::Home;
        }
        self.select_host(host, port, mgmt_port);
        // Overrides `select_host`'s plain "Loading library…": after a wait that may
        // have run for minutes with no modal up, the bar's job is to report that the
        // host came back, not just that a fetch started.
        if let Some(name) = name {
            self.home_status = Some(format!("{name} is back online — loading its library…"));
        }
    }

    /// Drains any cover art that's finished decoding since the last tick — called
    /// alongside `drain_discovery`. Returns whether any new art actually arrived
    /// (see `drain_discovery`'s docs on why).
    pub fn drain_art(&mut self) -> bool {
        let Some(loader) = &self.art_loader else { return false };
        let loaded = loader.drain();
        if loaded.is_empty() {
            return false;
        }
        for item in loaded {
            match item {
                crate::services::art::ArtLoaded::Card { game_id, pixmap } => {
                    // Layout is unchanged by art arriving — queue a repaint of just that
                    // card's tile (see `grid_cards_dirty`) rather than a full layer rebuild.
                    self.grid.cards_dirty.push(game_id.clone());
                    self.art.insert(game_id, pixmap);
                }
                crate::services::art::ArtLoaded::Hero { game_id, image } => {
                    // One that's no longer of use (focus moved on) is let go of in the
                    // loader too, so coming back to that card asks again — served from the
                    // disk cache by then, no round trip.
                    if !self.hero.accept(game_id.clone(), image) {
                        if let Some(loader) = &mut self.art_loader {
                            loader.forget_hero(&game_id);
                        }
                    }
                }
            }
        }
        true
    }
    /// Applies a `Back` to whichever screen is current — the single shared
    /// definition of "what Back means here" for every caller that needs it
    /// pre-emptively rather than through the normal per-screen `MenuEvent`
    /// dispatch: `main.rs`'s Back handling on Home (a no-op there, but routed
    /// through here so the policy lives in one place) and a modal's close (X)
    /// button click (`handle_mouse_click`'s `hover_close` branch below).
    pub fn back(&mut self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<ConnectTarget> {
        // Back steps focus out of the game grid (and the ⋯ column) back onto the
        // host sidebar first. Only a Back from the sidebar itself is a no-op here
        // — the menu loop turns that into the quit dialog.
        if matches!(self.nav.screen, Screen::Home) {
            // A held card's submenu is up: Back dismisses it rather than stepping focus
            // out from under it.
            if self.card_menu.take().is_some() {
                return None;
            }
            match self.home_focus {
                HomeFocus::Grid(_) => {
                    self.home_focus = HomeFocus::Sidebar(self.sidebar_index_for_selected());
                }
                HomeFocus::SidebarMenu(i) => self.home_focus = HomeFocus::Sidebar(i),
                HomeFocus::Sidebar(_) => {}
            }
            return None;
        }
        // Every modal decides for itself where Back goes.
        self.handle_menu_event(MenuEvent::Back, screen_w, screen_h, fonts)
    }

    /// Advances every live animation one tick — the eased scroll, the focus pop,
    /// the modal fade — and reports whether anything is still moving (the main
    /// loop keeps rendering while true). Expired animations report one final
    /// `true` so their end state gets drawn.
    pub fn tick_animations(&mut self) -> bool {
        let now = Instant::now();
        let dt = self.last_tick.map_or(ui::animation::SCROLL_STEP_TICK, |t| now - t);
        self.last_tick = Some(now);
        let mut animating = ui::animation::ease_scroll(&mut self.grid.scroll, self.grid.scroll_target, dt);
        // The scrolling modal's viewport, on the same ease-out as the grid. `scroll.offset`
        // has already jumped to its new row; this is only the rendered crop catching up.
        animating |= ui::animation::ease_scroll(&mut self.modal.scroll_px, self.modal.scroll_target_px, dt);
        if let Some(t) = self.focus_anim {
            if t.elapsed() >= ui::animation::CARD_FOCUS_POP {
                self.focus_anim = None;
            }
            animating = true;
        }
        if self.modal.fade.tick_split(MODAL_FADE, MODAL_FADE_OUT) {
            animating = true;
        }
        if self.dropdown_fade.tick(DROPDOWN_FADE) {
            animating = true;
        }
        // The hero loading screen keeps panning for as long as the launch is on screen,
        // which (unlike the fade) is however long the handshake takes.
        if self
            .launch_anim
            .is_some_and(|t| t.elapsed() < hero::LAUNCH_FADE || self.hero.showing())
        {
            animating = true;
        }
        if let Some(t) = self.modal.focus_anim {
            if t.elapsed() >= ui::animation::FOCUS_POP {
                self.modal.focus_anim = None;
            }
            animating = true;
        }
        // Disarmed by `poll_press` (the render loop retires the dip), not here.
        if self.press.armed() {
            animating = true;
        }
        if let Some((t, _, _)) = self.modal.switch_anim {
            if t.elapsed() >= ui::animation::FOCUS_POP {
                self.modal.switch_anim = None;
            }
            animating = true;
        }
        if let Some(t) = self.scroll.shown_at {
            if t.elapsed() >= SCROLL_INDICATOR_LIFETIME {
                self.scroll.shown_at = None;
            }
            animating = true;
        }
        // The held card's submenu: its rise, and the selection band's slide between rows.
        // Both run off clocks on `CardMenu`, not off `focus_anim` — without reporting them
        // here the loop parks in `wait_for_event` mid-rise (the auto-repeat KeyDowns the
        // hold swallows set no `dirty`), and the panel finishes only when OK is released.
        if self.card_menu.as_mut().is_some_and(state::cardmenu::CardMenu::tick) {
            animating = true;
        }
        if self.grid.card_pops_running() {
            animating = true;
        }
        animating
    }

    /// Queues the whole document for the background writer. Every mutation of settings, hosts or
    /// selection comes through here rather than writing its own slice.
    pub(crate) fn persist(&self) {
        self.state_writer.save(store::Persisted {
            settings: self.settings,
            known_hosts: self.known_hosts.clone(),
            selected_host: self.selected_host.clone(),
            // Always this build's version: whatever wrote the document last is what a future
            // migration needs to know, and that is now us.
            version: Some(store::VERSION.to_string()),
        });
    }

    /// The known-host record for an address — the one place `(host, port)` is matched.
    pub(crate) fn known_host(&self, host: &str, port: u16) -> Option<&KnownHost> {
        self.known_hosts.iter().find(|h| h.host == host && h.port == port)
    }

    /// The `KnownHost` record backing `selected_host`, if any — shared by every
    /// pin-related lookup (the focused card's badge, `toggle_focused_pin`).
    pub(crate) fn selected_known_host(&self) -> Option<&KnownHost> {
        let (host, port) = self.selected_host.as_ref()?;
        self.known_host(host, *port)
    }

    pub(crate) fn known_host_mut(&mut self, host: &str, port: u16) -> Option<&mut KnownHost> {
        self.known_hosts.iter_mut().find(|h| h.host == host && h.port == port)
    }

    pub(crate) fn selected_known_host_mut(&mut self) -> Option<&mut KnownHost> {
        let (host, port) = self.selected_host.clone()?;
        self.known_host_mut(&host, port)
    }

    /// The title of grid card `idx` (see `grid_card_at`) and its cover art, if
    /// fetched. Callers must only pass an `idx` that `is_grid_card` (tile
    /// building already filters padding gaps out).
    pub(crate) fn grid_card_content(&self, idx: usize, columns: usize) -> (&str, Option<&Pixmap>) {
        match self.grid_card_at(idx, columns) {
            Some(GridCard::Desktop) => ("Desktop", None),
            Some(GridCard::Game(game)) => (game.title.as_str(), self.art.get(&game.id)),
            None => unreachable!("idx filtered to a real card before building"),
        }
    }

    /// The current position (0.0..=1.0, see `Painter::switch`) of a `Toggle`
    /// row's switch given its settled state `target_on` — mid-slide while
    /// `switch_anim` is in flight *for that same row and transition*, otherwise
    /// settled at the endpoint. `row` is the focused row being rendered; the
    /// slide only plays for the row that actually flipped, not a same-valued
    /// neighbor focused mid-animation.
    pub(crate) fn toggle_frac(&self, target_on: bool, row: usize) -> f32 {
        match self.modal.switch_anim {
            Some((t, from_on, anim_row)) if anim_row == row && from_on != target_on => {
                let f = ui::animation::anim_frac(Some(t), ui::animation::FOCUS_POP);
                if target_on {
                    f
                } else {
                    1.0 - f
                }
            }
            _ => f32::from(target_on),
        }
    }

    /// Per-tick app-state advance that must run exactly once, *before* `prepare_tiles`
    /// composes the frame — kept out of `prepare_tiles` so that method only touches tiles.
    /// Derives `card_size` from the current width and advances the modal open/close fades on
    /// a screen transition. Ordering matters: fades must advance once per tick before compose,
    /// so the `ui_flow`/`stream` loops call this immediately ahead of `prepare_tiles`.
    /// Returns whether the screen changed this tick — `prepare_tiles` needs it to force a
    /// modal-tile rebuild on entry, but this method has already consumed the transition by
    /// advancing `last_screen`, so it hands the flag back rather than leaving it to recompute.
    pub fn advance_frame(&mut self, screen_w: u32) -> bool {
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        self.grid.card_size = view::home::grid_card_size(available_w, columns);

        // Every screen transition triggers close-fade for the left screen and
        // open-fade for the entered screen, centralized here rather than at each
        // dispatch site. Every modal exit fades, modal-to-modal included: the leaving
        // card's pixels go to `tile::MODAL_PREV` (see `snapshot_closing_modal`), so the
        // entering screen taking over `tile::MODAL` no longer forces the close to be a cut.
        let screen_changed = self.nav.screen != self.nav.last_screen;
        if screen_changed {
            let left = self.nav.last_screen;
            self.nav.last_screen = self.nav.screen;
            if !matches!(left, Screen::Home) {
                self.modal.fade.close(left);
            }
            if !matches!(self.nav.screen, Screen::Home) {
                self.modal.fade.open();
                // Reopening the same screen before its close-fade finished — the new
                // open wins. A close-fade for a *different* screen is left alone.
                self.modal.fade.cancel_closing(self.nav.screen);
            }
        }
        screen_changed
    }
}
