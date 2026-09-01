//! Pre-stream UI: Home screen (sidebar + game grid) with modals (Pairing/Settings/Add-host).
//! `ui.rs` owns drawing/input-mapping, `store.rs` owns persistence, `discovery.rs` owns mDNS.
//!
//! Per-screen `impl App` blocks are split by concern: `state` (event handling, transitions)
//! and `view` (geometry + draw-list building). Keeping them under `app` lets `ui`/`core`
//! stay dependency leaves — neither reaches back into `App`.
pub(crate) mod assets;
pub(crate) mod grid;
pub(crate) mod hero;
pub(crate) mod hosts;
pub(crate) mod jobs;
pub(crate) mod library;
pub(crate) mod menu;
pub(crate) mod modal;
pub(crate) mod nav;
pub(crate) mod pointer;
pub(crate) mod press;
pub(crate) mod render;
pub(crate) mod render_input;
pub(crate) mod screens;
pub(crate) mod settingsui;
pub(crate) mod spinner;
pub(crate) mod state;
pub(crate) mod view;

use std::time::{Duration, Instant};

use crate::ui::render::Rect;
use anyhow::Result;
use tiny_skia::Pixmap;

use crate::app::hosts::HostEntry;
use crate::app::nav::ScreenKey;
use crate::core::event::MenuEvent;
pub use crate::core::model::ConnectTarget;
pub use crate::core::screen::{HomeFocus, PairingFocus, Screen};
use crate::services::discovery::Discovery;
use crate::services::store::{self, KnownHost, Settings};
use crate::ui;

/// How much a focused grid card grows. Bigger than the modal widgets' pop (they sit
/// in a fixed column where any spill reads as a layout shift); a card has the grid gap
/// around it to grow into.
pub(crate) const CARD_GROWTH: f32 = 0.045;
pub(crate) const LAUNCH_GROWTH: f32 = 3.5;
pub(crate) const CARD_POP: Duration = Duration::from_millis(300);
pub(crate) const CARD_POP_SHRINK: f32 = 0.14;
/// The grid's first appearance after the spinner: one diagonal wave from the top-left corner,
/// scale-free, so the whole screen reads as one surface arriving rather than as a field of
/// individually popping cards. The launch backdrop leaves on the same motion (`app::hero`).
pub(crate) const GRID_REVEAL_WAVE: ui::animation::Wave = ui::animation::Wave {
    span: Duration::from_millis(380),
    fade: Duration::from_millis(420),
};
pub(crate) const SCROLL_INDICATOR_HOLD: Duration = Duration::from_millis(700);
pub(crate) const SCROLL_INDICATOR_FADE: Duration = Duration::from_millis(350);
/// How long a Home status line stays up at full opacity before it fades out. The fade
/// itself is [`OVERLAY_FADE`], the same curve the toast notification leaves on. Every line here is
/// ambient (a load result, a wake report, a launch error) and none of them stay true
/// forever, so the grid gets its bottom edge back instead of keeping stale text.
pub(crate) const HOME_STATUS_LIFETIME: Duration = Duration::from_secs(15);

/// How long a library fetch may run before its progress line is worth putting up — avoids
/// flashing "Loading library…" for one frame on a fast fetch.
pub(crate) const LIBRARY_STATUS_DELAY: Duration = Duration::from_secs(1);
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
    pub(crate) nav: nav::Nav,
    pub(crate) jobs: jobs::Jobs,
    pub(crate) library: library::Library,
    pub(crate) hosts: hosts::HostsState,
    pub(crate) settings_ui: settingsui::SettingsUi,
    pub(crate) screens: screens::slots::ScreenSlots,
    pub(crate) render: render::state::RenderState,
    pub(crate) home_focus: HomeFocus,
    pub(crate) home_status: Option<String>,
    /// Must survive library reload (cleared on success, else error disappears after 1s).
    pub(crate) home_status_sticky: bool,
    /// One-shot toast, outbox since App has no overlay handle.
    pub(crate) toast: Option<String>,
    home_status_shown_at: Option<Instant>,
    /// Status line waiting out `LIBRARY_STATUS_DELAY`.
    library_status_due: Option<(Instant, String)>,
    pub(crate) launch_ready: Option<ConnectTarget>,
    pub(crate) launch_anim: Option<Instant>,
    pub(crate) launch_anim_idx: Option<usize>,
    /// Submenu over held card's title strip.
    pub(crate) card_menu: Option<state::cardmenu::CardMenu>,
    /// Intro hint owed on first launch after version bump.
    pub(crate) intro_hint_owed: bool,
    /// Per-host launch history (orders Library section). Cached at startup.
    pub(crate) recents: crate::services::recents::Recents,
    /// Off-thread settings persist.
    pub(crate) state_writer: store::StateWriter,
    /// Detected pad type (meaningful only if `gamepad_type` is Auto).
    pub(crate) detected_gamepad_type: Option<store::GamepadType>,
    /// webOS on-screen keyboard up (moves address form from under panel).
    pub(crate) keyboard_shown: bool,
    pub(crate) identity: (String, String),
    /// Last tick time (for real-time scroll easing, not frame-count based).
    last_tick: Option<Instant>,
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
    // ------------------------------------------------------ what `runtime` may write --
    // The menu owns its own state; these are the four things only the outer loop can know.

    /// The attached pad's type per `gamepad::detect_type`, refreshed on hotplug.
    pub fn set_gamepad_type(&mut self, kind: Option<store::GamepadType>) {
        self.detected_gamepad_type = kind;
    }

    /// Whether webOS's on-screen keyboard is up, polled from `SDL_IsScreenKeyboardShown`.
    pub fn set_keyboard_shown(&mut self, shown: bool) {
        self.keyboard_shown = shown;
    }

    /// Show status line only if fetch still running after `LIBRARY_STATUS_DELAY`.
    pub(crate) fn set_home_status_delayed(&mut self, line: String) {
        self.set_home_status(None, false);
        self.library_status_due = Some((Instant::now(), line));
    }

    /// Set Home status (sticky survives reload, cleared on success). Drops delayed line.
    pub(crate) fn set_home_status(&mut self, status: Option<String>, sticky: bool) {
        self.library_status_due = None;
        self.home_status_shown_at = status.is_some().then(Instant::now);
        self.home_status = status;
        self.home_status_sticky = sticky;
    }

    /// Open modal's scroll indicator (hold-then-fade like all self-expiring overlays).
    pub(crate) fn scroll_indicator_alpha(&self) -> Option<f32> {
        ui::fade::hold_alpha(
            self.render.scroll.shown_at?,
            SCROLL_INDICATOR_HOLD,
            SCROLL_INDICATOR_FADE,
        )
    }

    /// Status line opacity (same clock as toast, so lines leave screen identically).
    pub(crate) fn home_status_alpha(&self) -> Option<f32> {
        ui::fade::hold_alpha(
            self.home_status_shown_at?,
            HOME_STATUS_LIFETIME,
            crate::ui::fade::OVERLAY_FADE,
        )
    }

    /// Queue transient toast (replaces waiting ones; second action before tick = first stale).
    pub(crate) fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some(message.into());
    }

    /// Take queued toast for loop's overlay.
    pub fn take_toast(&mut self) -> Option<String> {
        self.toast.take()
    }

    /// Ends a bitrate-slider drag; the button can only come up on the loop that owns events.
    pub fn end_slider_drag(&mut self) {
        self.settings_ui.slider_drag = false;
    }

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
        let mut app = Self {
            nav: nav::Nav::default(),
            library: library::Library::default(),
            jobs: jobs::Jobs {
                discovery: crate::services::discovery::Discovery::start(),
                ..Default::default()
            },
            settings_ui: settingsui::SettingsUi::new(settings),
            screens: screens::slots::ScreenSlots::default(),
            render: render::state::RenderState::default(),
            hosts: hosts::HostsState {
                known: known_hosts,
                entries,
                reachable: Self::new_reachability(),
                ..Default::default()
            },
            home_focus: HomeFocus::Sidebar(0),
            home_status: None,
            home_status_sticky: false,
            toast: None,
            home_status_shown_at: None,
            library_status_due: None,
            launch_ready: None,
            launch_anim: None,
            launch_anim_idx: None,
            card_menu: None,
            intro_hint_owed: new_build,
            recents: crate::services::recents::Recents::load(),
            state_writer,
            detected_gamepad_type: None,
            keyboard_shown: false,
            identity,
            last_tick: None,
        };
        // Restore the last-active sidebar host (if it's still known and paired)
        // so relaunching the app lands back on its game grid.
        if let Some((host, port)) = selected_host {
            if let Some(h) = app
                .hosts
                .known
                .iter()
                .find(|h| h.host == host && h.port == port && h.is_paired())
            {
                let (host, port, mgmt_port) = (h.host.clone(), h.port, h.mgmt_port);
                app.select_host(host, port, mgmt_port);
            }
        }
        // Applies the persisted "Show logs" preference to the otherwise-ephemeral overlay.
        if app.settings_ui.settings.show_logs {
            crate::runtime::set_log_overlay_enabled(true);
        }
        // Same call the Experimental toggle makes, so the persisted value and a live flip take
        // exactly the same path into `ui::theme`.
        app.restyle();
        // Rasterizes the spinner's frames off the render thread (OnceLock warm-up). After
        // `restyle`, because the cache snapshots the palette it rasterizes in.
        std::thread::spawn(crate::app::assets::spinner_frames);
        app
    }

    /// Name of the host whichever host-scoped modal (Forget, Host power settings) is acting on.
    pub(crate) fn host_menu_host_name(&self) -> Option<&str> {
        self.screens
            .host_menu_index
            .and_then(|i| self.hosts.entries.get(i))
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
            menu::SettingsScope::Game => self.settings_ui.game_settings.as_ref(),
            menu::SettingsScope::Global => None,
        }
    }

    pub(crate) fn editing_game_mut(&mut self) -> Option<&mut state::gamesettings::GameSettingsState> {
        match self.settings_scope() {
            menu::SettingsScope::Game => self.settings_ui.game_settings.as_mut(),
            menu::SettingsScope::Global => None,
        }
    }

    /// The `Settings` the open settings screen is editing: the global document, or the
    /// per-game scratch copy. One accessor so every mutator, lock check and dropdown lookup
    /// in `menu` sees the same value the rows were built from.
    pub(crate) fn settings_target(&self) -> &Settings {
        match self.editing_game() {
            Some(gs) => &gs.merged,
            None => &self.settings_ui.settings,
        }
    }

    /// Spells `editing_game_mut`'s gate out rather than calling it: the fallback arm needs
    /// `self` back, which borrowck won't grant while a returned `Option<&mut _>` is in scope.
    pub(crate) fn settings_target_mut(&mut self) -> &mut Settings {
        let scope = self.settings_scope();
        match &mut self.settings_ui.game_settings {
            Some(gs) if scope == menu::SettingsScope::Game => &mut gs.merged,
            _ => &mut self.settings_ui.settings,
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

    /// `(row, focused, alpha)` for the open dropdown or its close-fade; `None` if neither.
    pub(crate) fn dropdown_draw_state(&self) -> Option<(usize, usize, f32)> {
        if let Some(dd) = &self.settings_ui.dropdown {
            Some((dd.row, dd.focused, self.settings_ui.dropdown_fade.open_alpha()))
        } else {
            self.settings_ui
                .dropdown_fade
                .closing_frame()
                .map(|(alpha, (row, focused))| (row, focused, alpha))
        }
    }

    /// Grid geometry bridges — `view::home` is pure geometry, so these supply the two
    /// pieces of live state (the section shape and the scroll offset) it takes.
    pub(crate) fn unscrolled_card_rect(&self, idx: usize, columns: usize, grid_x: i32, available_w: u32) -> Rect {
        view::home::unscrolled_card_rect(idx, grid_x, available_w, self.library.layout(columns))
    }

    pub(crate) fn scrolled_card_rect(&self, idx: usize, columns: usize, grid_x: i32, available_w: u32) -> Rect {
        view::home::scrolled_card_rect(
            idx,
            grid_x,
            available_w,
            self.library.layout(columns),
            self.render.grid.scroll,
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
            grid_x,
            available_w,
            self.library.layout(columns),
            (x, y + self.render.grid.scroll),
        )
    }

    /// Rebuilds the sidebar from `known_hosts`, dropping any discovered-but-unsaved rows. Every
    /// caller that mutates `known_hosts` goes through this rather than collecting the list
    /// itself, so no site has to remember to re-anchor focus.
    pub(crate) fn rebuild_entries(&mut self) {
        self.set_entries(known_entries(&self.hosts.known));
    }

    /// The one place the sidebar row list is replaced: keeps focus on the row the user is on and
    /// marks the layer dirty, neither of which any caller should have to remember.
    fn set_entries(&mut self, entries: Vec<HostEntry>) {
        let before = self.hosts.entries.len();
        self.hosts.entries = entries;
        self.reanchor_sidebar_focus(before);
        // The sidebar layer is a cached tile keyed by nothing but this flag (see `prepare_tiles`),
        // so a rebuilt row list that doesn't set it leaves the previous host list on screen.
        self.render.sidebar_dirty = true;
    }

    /// Keeps sidebar focus on the row the user is actually on after the host list changed
    /// length (`before` is what it was). Focus is a flat index over hosts + "Add host" +
    /// "Settings", and the two utility rows are identified purely by their index — see
    /// `compose_sidebar_focus`, which only draws the bottom-pinned highlight for
    /// `entries.len() + 1` — so leaving a stale index there puts "Settings" mid-list.
    fn reanchor_sidebar_focus(&mut self, before: usize) {
        let now = self.hosts.entries.len();
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
        // Content reanchoring preserves focus identity; it is not an interactive move.
        self.home_focus = HomeFocus::Sidebar(i);
    }

    /// Whether `addr:port` already has a sidebar row, saved or merely discovered.
    pub(crate) fn host_listed(&self, addr: &str, port: u16) -> bool {
        self.hosts.known.iter().any(|h| h.host == addr && h.port == port)
            || self
                .hosts
                .entries
                .iter()
                .any(|e| matches!(e, HostEntry::Discovered(d) if d.addr == addr && d.port == port))
    }

    /// Merges freshly-discovered hosts into the entry list (known hosts keep their
    /// paired status; a discovered host not yet known gets appended), learns each
    /// known host's Wake-on-LAN MAC(s) from its live advert while it's awake to
    /// advertise them, and — if a wake is in flight (`self.screens.wake`) — notices when the
    /// waking host reappears on mDNS and reconnects. Returns whether the sidebar
    /// actually changed — `main.rs`'s render loop uses this to skip a redraw when a
    /// discovery tick found nothing new (see its dirty-flag docs).
    pub fn drain_discovery(&mut self) -> bool {
        let before = self.hosts.entries.len();
        let mut changed = false;
        let mut mac_learned = false;
        let mut woke = None;
        // `found.addr` throughout this loop is deliberate, not a typo for a nonexistent
        // `found.host` — `DiscoveredHost` (discovery.rs) only has `addr`, `WakeState`/
        // `KnownHost` only have `host`; both hold the same kind of value (network address).
        let polled = self.jobs.discovery.as_mut().map(Discovery::poll).unwrap_or_default();
        for found in polled {
            // An announce is the host saying it is up, on its own initiative — the cheapest
            // liveness evidence there is, and previously the one the dot ignored.
            changed |= self.note_reachable(&found.addr, found.port, true);
            #[allow(clippy::suspicious_operation_groupings)]
            if let Some(w) = &self.screens.wake {
                if found.addr == w.host && found.port == w.port {
                    woke = Some((found.addr.clone(), found.port, found.mgmt_port));
                }
            }
            #[allow(clippy::suspicious_operation_groupings)]
            let known = self
                .hosts
                .known
                .iter_mut()
                .find(|h| h.host == found.addr && h.port == found.port);
            if let Some(known) = known {
                if !found.mac.is_empty() && known.mac != found.mac {
                    known.mac.clone_from(&found.mac);
                    mac_learned = true;
                }
            }
            if !self.host_listed(&found.addr, found.port) {
                self.hosts.entries.push(HostEntry::Discovered(found));
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
            self.render.sidebar_dirty = true;
        }
        changed
    }

    /// Ends an in-flight wake because the host is actually back — whether that was
    /// noticed passively (`drain_discovery` seeing a fresh mDNS resolve) or actively
    /// (`tick_wake`'s reachability probe succeeding). `source` is just for the log line.
    pub(crate) fn wake_succeeded(&mut self, host: String, port: u16, mgmt_port: Option<u16>, source: &str) {
        tracing::info!("wake succeeded: {host}:{port} back ({source})");
        let name = self.screens.wake.take().map(|w| w.name);
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
            self.set_home_status_delayed(format!("{name} is back online — loading its library…"));
        }
    }

    /// Drains any cover art that's finished decoding since the last tick — called
    /// alongside `drain_discovery`. Returns whether any new art actually arrived
    /// (see `drain_discovery`'s docs on why).
    pub fn drain_art(&mut self) -> bool {
        let Some(loader) = &self.jobs.art else { return false };
        let loaded = loader.drain();
        if loaded.is_empty() {
            return false;
        }
        for item in loaded {
            match item {
                crate::services::art::ArtLoaded::Card { game_id, pixmap } => {
                    // Layout is unchanged by art arriving — queue a repaint of just that
                    // card's tile (see `grid_cards_dirty`) rather than a full layer rebuild.
                    self.render.grid.cards_dirty.push(game_id.clone());
                    self.library.art.insert(game_id, pixmap);
                }
                crate::services::art::ArtLoaded::Hero { game_id, image } => {
                    // One that's no longer of use (focus moved on) is let go of in the
                    // loader too, so coming back to that card asks again — served from the
                    // disk cache by then, no round trip.
                    if !self.render.hero.accept(game_id.clone(), image) {
                        if let Some(loader) = &mut self.jobs.art {
                            loader.forget_hero(&game_id);
                        }
                    }
                }
            }
        }
        true
    }
    /// Erases one character from whichever screen is currently editing text, reporting
    /// whether it consumed the key. The counterpart to [`Self::back`]: one definition of
    /// "what an erase means here", so the loop that sees the Backspace doesn't have to
    /// know which screens edit what. `false` leaves the key to its normal `Back` meaning,
    /// which is what makes an erase on an already-empty field still close the modal.
    pub fn erase_text_entry(&mut self) -> bool {
        match self.nav.screen {
            Screen::AddHost | Screen::EditHost => {
                !self.screens.add_host.text().is_empty() && {
                    self.screens.add_host.backspace();
                    true
                }
            }
            Screen::RenameCollection => {
                !self.screens.collections.name.text().is_empty() && {
                    self.screens.collections.name.backspace();
                    true
                }
            }
            Screen::Pairing => self.erase_pin_digit(),
            _ => false,
        }
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
            if self.card_menu.is_some() {
                self.close_card_menu();
                return None;
            }
            match self.home_focus {
                HomeFocus::Grid(_) => {
                    self.set_home_focus(HomeFocus::Sidebar(self.sidebar_index_for_selected()));
                }
                HomeFocus::SidebarMenu(i) => {
                    self.set_home_focus(HomeFocus::Sidebar(i));
                }
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
        let mut animating =
            ui::animation::ease_scroll(&mut self.render.grid.scroll, self.render.grid.scroll_target, dt);
        // The scrolling modal's viewport, on the same ease-out as the grid. `scroll.offset`
        // has already jumped to its new row; this is only the rendered crop catching up.
        animating |=
            ui::animation::ease_scroll(&mut self.render.modal.scroll_px, self.render.modal.scroll_target_px, dt);
        if let Some(t) = self.render.focus_anim {
            let duration = match self.home_focus {
                HomeFocus::Grid(_) => ui::animation::CARD_FOCUS_POP,
                HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_) => ui::animation::FOCUS_POP,
            };
            if t.elapsed() >= duration {
                self.render.focus_anim = None;
            }
            animating = true;
        }
        if self.render.modal.fade.tick() {
            animating = true;
        }
        if self.settings_ui.dropdown_fade.tick() {
            animating = true;
        }
        // The hero loading screen keeps panning for as long as the launch is on screen,
        // which (unlike the fade) is however long the handshake takes.
        if self
            .launch_anim
            .is_some_and(|t| t.elapsed() < hero::LAUNCH_FADE || self.render.hero.showing())
        {
            animating = true;
        }
        if let Some(t) = self.render.modal.focus_anim {
            if t.elapsed() >= ui::animation::FOCUS_POP {
                self.render.modal.focus_anim = None;
            }
            animating = true;
        }
        // Disarmed by `poll_press` (the render loop retires the dip), not here.
        if self.render.press.armed() {
            animating = true;
        }
        if let Some((t, _, _)) = self.render.modal.switch_anim {
            if t.elapsed() >= ui::animation::FOCUS_POP {
                self.render.modal.switch_anim = None;
            }
            animating = true;
        }
        // The lifetime outranks `home_status_sticky`: sticky defends a line against the
        // library reload's clear, not against the clock.
        // Only the expiring frame reports `animating` — the idle branch's `wait_for_event`
        // still times out at `TICK_BUDGET`, so this runs on schedule without holding the
        // SoC at 60Hz for the whole 15s.
        if let Some((_, line)) = self
            .library_status_due
            .take_if(|(t, _)| t.elapsed() >= LIBRARY_STATUS_DELAY)
        {
            // A fetch that already landed needs no line — and must not overwrite whatever
            // `drain_games` put up instead. Only a line that actually goes up is a redraw.
            if self.library_fetch_in_flight() {
                self.set_home_status(Some(line), false);
                animating = true;
            }
        }
        // Every frame of the fade out is a redraw; the frame after it is the clear.
        if self
            .home_status_shown_at
            .is_some_and(|t| t.elapsed() >= HOME_STATUS_LIFETIME)
        {
            if self.home_status_alpha().is_none() {
                self.set_home_status(None, false);
            }
            animating = true;
        }
        if self.render.scroll.shown_at.is_some() {
            if self.scroll_indicator_alpha().is_none() {
                self.render.scroll.shown_at = None;
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
        if self.render.grid.card_pops_running() || self.render.grid.reveal.dissolving() {
            animating = true;
        }
        animating
    }

    /// Queues the whole document for the background writer. Every mutation of settings, hosts or
    /// selection comes through here rather than writing its own slice.
    pub(crate) fn persist(&self) {
        self.state_writer.save(store::Persisted {
            settings: self.settings_ui.settings,
            known_hosts: self.hosts.known.clone(),
            selected_host: self.library.selected_host.clone(),
            // Always this build's version: whatever wrote the document last is what a future
            // migration needs to know, and that is now us.
            version: Some(store::VERSION.to_string()),
        });
    }

    /// The known-host record for an address — the one place `(host, port)` is matched.
    pub(crate) fn known_host(&self, host: &str, port: u16) -> Option<&KnownHost> {
        self.hosts.known.iter().find(|h| h.host == host && h.port == port)
    }

    /// The `KnownHost` record backing `selected_host`, if any — shared by every lookup
    /// that needs the selected host's collections or per-game settings.
    pub(crate) fn selected_known_host(&self) -> Option<&KnownHost> {
        let (host, port) = self.library.selected_host.as_ref()?;
        self.known_host(host, *port)
    }

    /// What to do to the selected host on the way out, or `None` when it is set to "None",
    /// has never been paired (the management lane needs this device's cert on its list), or
    /// no host is selected at all.
    ///
    /// Built while `App` is alive so the exit paths can fire it after it is gone — see
    /// [`services::power::ExitPlan`](crate::services::power::ExitPlan).
    pub(crate) fn exit_plan(&self) -> Option<crate::services::power::ExitPlan> {
        // The SELECTED host and only it — the one the sidebar highlights as active, via the
        // same `library.selected_host` that `sidebar_index_of_selected_host` reads. Every
        // other known host is left alone whatever its own `exit_action` says: the setting is
        // per host, but quitting only ever ends the session you are in.
        let known = self.selected_known_host()?;
        // Only a host that answered its last check. Asking one that is already down or gone
        // means waiting out `budget::EXIT_ACTION` on a connection that cannot complete, and
        // the whole point of that budget being 200 ms is that it is never spent guessing.
        // Unknown counts as down, same as everywhere else reachability is read.
        if self.known_host_online(known) != Some(true) {
            tracing::debug!("exit action skipped: {} was not reachable", known.host);
            return None;
        }
        self.power_plan(known, known.exit_action)
    }

    /// The management-lane target for one power action on `known`, or `None` when there is
    /// nothing to send: no action ([`ExitAction::None`]), or no pairing to send it under.
    ///
    /// The one place the mgmt-port default and the pin-is-required rule are stated — the exit
    /// path, the host menu's power row and the permission probe all build their target here.
    pub(crate) fn power_plan(
        &self,
        known: &KnownHost,
        action: crate::services::store::ExitAction,
    ) -> Option<crate::services::power::ExitPlan> {
        action.action_id()?;
        Some(crate::services::power::ExitPlan {
            addr: known.host.clone(),
            mgmt_port: known.mgmt_port.unwrap_or(crate::services::library::DEFAULT_MGMT_PORT),
            identity: self.identity.clone(),
            // Required, not merely pinned-if-known: an unpaired host would refuse the invoke
            // anyway, and a power action is the last request to send to an unverified peer.
            pin: Some(known.fingerprint?),
            action,
        })
    }

    /// Which of the selected host's collections holds `pin_id`, or `None` for Library.
    pub(crate) fn collection_of_card(&self, pin_id: &str) -> Option<usize> {
        self.selected_known_host()?.collection_of(pin_id)
    }

    /// Whether a collection holds `pin_id` — what the card menu's Remove row, its Add/Move
    /// wording and the collections modal's heading all turn on. Library *is* "in no
    /// collection", so a card there is not held.
    pub(crate) fn card_is_held(&self, pin_id: &str) -> bool {
        self.collection_of_card(pin_id).is_some()
    }

    pub(crate) fn known_host_mut(&mut self, host: &str, port: u16) -> Option<&mut KnownHost> {
        self.hosts.known.iter_mut().find(|h| h.host == host && h.port == port)
    }

    pub(crate) fn selected_known_host_mut(&mut self) -> Option<&mut KnownHost> {
        let (host, port) = self.library.selected_host.clone()?;
        self.known_host_mut(&host, port)
    }

    /// The title of grid card `idx` (see `grid_card_at`) and its cover art, if
    /// fetched. Callers must only pass an `idx` that `is_grid_card` (tile
    /// building already filters padding gaps out).
    pub(crate) fn grid_card_content(&self, idx: usize, columns: usize) -> (&str, Option<&Pixmap>) {
        match self.grid_card_at(idx, columns) {
            Some(game) => (game.title.as_str(), self.library.art.get(&game.id)),
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
        match self.render.modal.switch_anim {
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
        self.render.grid.card_size = view::home::grid_card_size(available_w, columns);

        // Every screen transition triggers close-fade for the left screen and
        // open-fade for the entered screen, centralized here rather than at each
        // dispatch site. Every modal exit fades, modal-to-modal included: the leaving
        // card's pixels go to `tile::MODAL_PREV` (see `snapshot_closing_modal`), so the
        // entering screen taking over `tile::MODAL` no longer forces the close to be a cut.
        let screen_changed = self.nav.screen != self.nav.last_screen;
        if screen_changed {
            let left = self.nav.last_screen;
            self.nav.last_screen = self.nav.screen;
            // Modal-to-modal cross-fades: `ui::fade` makes the leaving card the entering
            // one's inverse. Anything involving Home is a plain open or close.
            if !matches!(left, Screen::Home) {
                if matches!(self.nav.screen, Screen::Home) {
                    self.render.modal.fade.close(left);
                } else {
                    self.render.modal.fade.close_cross(left);
                }
            }
            if !matches!(self.nav.screen, Screen::Home) {
                self.render.modal.fade.open();
                // Reopening the same screen before its close-fade finished — the new
                // open wins. A close-fade for a *different* screen is left alone.
                self.render.modal.fade.cancel_closing(self.nav.screen);
            }
        }
        screen_changed
    }
}
