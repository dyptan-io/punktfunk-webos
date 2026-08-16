//! Rasterization: which tiles are stale this frame, and what to draw into each.
//!
//! The CPU half of the render path — the only place `tiny_skia` runs. Split out of
//! `app/mod.rs`, which held the state machine and both render halves in one 3.4k-line file.
//! Each `prepare_*` covers one family (sidebar, grid, hero, modal, dropdown, scroll) and
//! reports the tiles it rebuilt so `runtime` can re-upload their textures.
use std::time::Instant;

use anyhow::Result;

use crate::app::render::key::{ModalFocusKey, ModalShellKey, ScrollContentKey};
use crate::app::render::tile;
// A glob, deliberately: these are `impl App` blocks lifted out of `app/mod.rs`, and
// they read the same private tuning constants the rest of that module does.
use crate::app::*;
use crate::ui;
use crate::ui::cache::{self, TileStore};
use crate::ui::render::{Rect, TileId};
use crate::ui::Painter;

impl App {
    /// Sidebar family: the focus-free strip (rebuilt on content change) plus the
    /// single focused-row overlay tile. Pushes any rebuilt tiles onto `updated`.
    /// Extracted from `prepare_tiles` as a self-contained family (A2 staging).
    fn prepare_sidebar(
        &mut self,
        tiles: &mut TileStore,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        screen_h: u32,
        updated: &mut Vec<TileId>,
    ) -> Result<()> {
        // Kept on the `sidebar_dirty` flag rather than a content version: the strip is
        // built from every entry plus its reachability, and hashing all of that once a
        // frame would cost more than the flag the event side already maintains.
        if self.sidebar_dirty || !tiles.contains(tile::SIDEBAR) {
            let selected = self.sidebar_index_of_selected_host();
            let entries = &self.entries;
            let reach = self.reachability_list();
            // Reuses the existing full-height strip as its own scratch surface — several MB
            // that would otherwise be reallocated on every host list change.
            // The outer condition has already decided this is stale, so the version just
            // has to differ from the last one — `ensure_in_place` would otherwise see an
            // unchanged `STATIC` and skip the rebuild it was called to do.
            self.sidebar_gen = self.sidebar_gen.wrapping_add(1);
            tiles.ensure_in_place(
                tile::SIDEBAR,
                self.sidebar_gen,
                || Painter::new(ui::widgets::SIDEBAR_W, screen_h),
                |layer| {
                    view::sidebar::draw(
                        &mut ui::Canvas {
                            painter: layer,
                            text_cache,
                            fonts,
                            // The sidebar's own strip is the full panel height and a fixed width.
                            screen_w: ui::widgets::SIDEBAR_W,
                            screen_h,
                        },
                        entries,
                        None,
                        selected,
                        &reach,
                    )
                },
            )?;
            self.sidebar_dirty = false;
            tiles.remove(tile::FOCUS_ROW); // row content may have changed under it
            updated.push(tile::SIDEBAR);
        }
        // One tile serves both sidebar focus states (see `render_focused_row_tile`).
        let sidebar_focus = match self.home_focus {
            HomeFocus::Sidebar(i) => Some((i, false)),
            HomeFocus::SidebarMenu(i) => Some((i, true)),
            HomeFocus::Grid(_) => None,
        };
        if let Some(key) = sidebar_focus {
            let online = self.entries.get(key.0).and_then(|e| self.entry_online(e));
            if tiles.ensure(tile::FOCUS_ROW, cache::version(&key), || {
                view::sidebar::render_focused_row_tile(text_cache, fonts, &self.entries, key.0, key.1, online)
            })? {
                updated.push(tile::FOCUS_ROW);
            }
        }
        Ok(())
    }

    /// Grid family: windowed/budgeted card-tile building, eviction, the reveal
    /// spinner, and the shared ring/outline/pin-badge tiles — or the "no host"
    /// hint when nothing is selected. Extracted from `prepare_tiles` (A2 staging).
    #[allow(clippy::too_many_arguments)]
    fn prepare_grid(
        &mut self,
        tiles: &mut TileStore,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        columns: usize,
        card_w: u32,
        card_h: u32,
        screen_h: u32,
        updated: &mut Vec<TileId>,
    ) -> Result<()> {
        // Reset before the branch: it is only ever set inside it, and a stale `true` left
        // behind by a host that has since been deselected would spin the render loop at
        // full rate forever.
        self.tiles_pending = false;
        if self.selected_host.is_some() {
            let count = self.grid_len(columns);
            // Captured before it's cleared below: a fresh library load is the only
            // rebuild that also re-arms the spinner.
            let full_reset = self.grid_dirty;
            if full_reset {
                // Every existing texture is stale (different games, different host) —
                // drop them rather than leaving them to be overwritten one by one,
                // which would strand the tail of a longer previous library.
                for id in self.card_ids.release_all() {
                    tiles.remove(id);
                    self.evicted_tiles.push(id);
                }
                self.card_pop.clear();
                self.grid_dirty = false;
                self.grid_cards_dirty.clear();
                // Scrolling or re-pinning a card must not hide the already-visible
                // grid behind the spinner again.
                self.grid_reveal_ready = false;
                self.spinner_since = None;
                self.spinner_frame = None;
            } else {
                for id in std::mem::take(&mut self.grid_cards_dirty) {
                    if let Some(t) = self.card_ids.release(&id) {
                        tiles.remove(t);
                    }
                    self.card_pop.remove(&id);
                }
            }

            // Windowed, budgeted tile building — see `CARD_BUILD_BUDGET`.
            let row_h = card_h as i32 + view::home::GRID_GAP;
            let visible_rows = (screen_h as i32 - view::home::GRID_TOP_Y).max(row_h) / row_h + 1;
            let first_visible_row = (self.grid_scroll / row_h).max(0);
            let row_of = |idx: usize| (idx / columns.max(1)) as i32;
            let build_lo = first_visible_row - CARD_PREFETCH_ROWS;
            let build_hi = first_visible_row + visible_rows + CARD_PREFETCH_ROWS;
            let keep_lo = first_visible_row - CARD_KEEP_ROWS;
            let keep_hi = first_visible_row + visible_rows + CARD_KEEP_ROWS;

            // Held by value, not re-derived per index — and, unlike the `App`
            // helpers, it maps indices without borrowing all of `self`, so the art
            // lookups below can sit next to `&mut self.art_loader`.
            let layout = self.grid_layout(columns);

            // Evict first, so a long scroll frees textures in the same frame it needs new
            // ones rather than a frame later.
            for idx in 0..count {
                let row = row_of(idx);
                if row < keep_lo || row > keep_hi {
                    let Some(id) = layout.pin_id_at(&self.games, idx) else {
                        continue;
                    };
                    if let Some(t) = self.card_ids.release(id) {
                        tiles.remove(t);
                        self.evicted_tiles.push(t);
                    }
                    self.card_pop.remove(id);
                    if layout.game_at(&self.games, idx).is_some() {
                        // Drop the decoded cover too — it is several times the size of the
                        // tile it feeds. Re-requested from the disk cache on scroll back.
                        self.art.remove(id);
                        if let Some(loader) = &mut self.art_loader {
                            loader.forget(id);
                        }
                    }
                }
            }

            // Ready once nothing more can arrive: cover already in `self.art`, or the game
            // never had one to fetch (no `self.art` entry either way). "Desktop" and the
            // padding after a partial pinned row have no `games` entry and are always ready.
            let art_ready = |idx: usize| {
                layout.game_at(&self.games, idx).is_none_or(|game| {
                    self.art.contains_key(&game.id) || (game.art.portrait.is_none() && game.art.header.is_none())
                })
            };

            // Art-ready cards build first — building one before its cover arrives just
            // burns a second budget slot re-dirtying it once the cover shows up.
            let mut to_build = Vec::new();
            for idx in 0..count {
                let row = row_of(idx);
                if row < build_lo || row > build_hi {
                    continue;
                }
                // Nothing to build or fetch art for in the padding after a partial
                // pinned row.
                let Some(id) = layout.pin_id_at(&self.games, idx) else {
                    continue;
                };
                // Ask for this card's cover as it enters the window, not for the whole
                // library at once (see `art::ArtLoader`).
                if let (Some(loader), Some(game)) = (&mut self.art_loader, layout.game_at(&self.games, idx)) {
                    loader.request(game);
                }
                if self.card_ids.get(id).is_some_and(|t| tiles.contains(t)) {
                    continue;
                }
                to_build.push((idx, id.to_string(), art_ready(idx)));
            }
            to_build.sort_by_key(|(_, _, ready)| !ready);

            let mut pending = false;
            for (built, (idx, id, _)) in to_build.into_iter().enumerate() {
                if built >= CARD_BUILD_BUDGET {
                    pending = true;
                    break;
                }
                let tile = {
                    let (title, art) = self.grid_card_content(idx, columns);
                    ui::tiles::render_card_tile(text_cache, fonts, card_w, card_h, title, art)
                };
                let tile_id = self.card_ids.id(&id);
                tiles.put(tile_id, cache::STATIC, tile);
                if self.grid_reveal_ready {
                    self.card_pop.insert(id.clone(), Instant::now());
                }
                updated.push(tile_id);
            }
            self.tiles_pending = pending;

            // Prefetch the focused card's hero, so the connecting screen has one ready the
            // moment OK is pressed. Deduped in the loader, and the fetched bytes are
            // disk-cached, so hovering back over a card costs no round trip.
            //
            // Only once the visible window has settled: the loader serves hero requests
            // ahead of card art, so queueing one mid-scroll would put the cards the user is
            // actually looking at behind a full-screen fetch and decode.
            if self.grid_reveal_ready && !pending {
                if let HomeFocus::Grid(focus_idx) = self.home_focus {
                    if let Some(game) = layout.game_at(&self.games, focus_idx) {
                        if let Some(loader) = &mut self.art_loader {
                            loader.request_hero(game);
                        }
                        self.hero.want(&game.id);
                    }
                }
            }

            // The focused card's title strip: its own tile, so the wipe in `draw_list` is
            // a moving source/destination rect — one small blur per focus move instead of
            // re-rasterizing the card every animation frame.
            if let HomeFocus::Grid(idx) = self.home_focus {
                if let Some(pin_id) = layout.pin_id_at(&self.games, idx) {
                    let (title, art) = self.grid_card_content(idx, columns);
                    // Keyed by card identity like the card tiles themselves (`CardIds`),
                    // not by title — two games can share one.
                    let version = cache::version(&(pin_id, card_w, card_h, art.is_some()));
                    if tiles.ensure(tile::CARD_TITLE, version, || {
                        ui::tiles::render_card_title_tile(text_cache, fonts, card_w, card_h, title, art)
                    })? {
                        updated.push(tile::CARD_TITLE);
                    }
                }
            }

            // The pinned badge tile — built once, composited over the focused
            // card in `draw_list` rather than baked into individual card tiles.
            if tiles.ensure_static(tile::PIN_BADGE, || ui::tiles::render_pin_badge_tile(text_cache, fonts))? {
                updated.push(tile::PIN_BADGE);
            }

            // Rechecks the whole window rather than trusting `!pending`, since a card
            // built earlier can still be waiting behind a re-dirtied sibling; requires
            // `art_ready` too so a placeholder built this tick can't count as revealed.
            if !self.grid_reveal_ready {
                let window_ready = (0..count)
                    .filter(|&idx| {
                        let row = row_of(idx);
                        row >= build_lo && row <= build_hi
                    })
                    .all(|idx| {
                        layout
                            .pin_id_at(&self.games, idx)
                            .is_none_or(|id| self.card_ids.get(id).is_some_and(|t| tiles.contains(t)) && art_ready(idx))
                    });
                let since = *self.spinner_since.get_or_insert_with(Instant::now);
                self.grid_reveal_ready = window_ready || since.elapsed() >= SPINNER_MAX_WAIT;
                if self.grid_reveal_ready {
                    self.spinner_since = None;
                    self.spinner_frame = None;
                    // Everything built behind the spinner becomes visible in this
                    // one frame, so it all zooms in off a single clock.
                    let now = Instant::now();
                    for id in self.card_ids.pin_ids() {
                        self.card_pop.entry(id.to_string()).or_insert(now);
                    }
                } else {
                    let (frame_idx, _) = crate::assets::spinner_frame_at(since.elapsed().as_secs_f32());
                    if self.spinner_frame != Some(frame_idx) {
                        self.spinner_frame = Some(frame_idx);
                        updated.push(tile::spinner(frame_idx));
                    }
                }
            }

            // One shared tile at the current card size, so the card size *is* the
            // version — a resolution change rebuilds it, nothing else does.
            let size = cache::version(&(card_w, card_h));
            if tiles.ensure(tile::RING, size, || {
                Ok(ui::tiles::render_focus_ring_tile(card_w, card_h))
            })? {
                updated.push(tile::RING);
            }
            if tiles.ensure(tile::CARD_SHADOW, size, || {
                Ok(ui::tiles::render_card_shadow_tile(card_w, card_h))
            })? {
                updated.push(tile::CARD_SHADOW);
            }
            if tiles.ensure(tile::CARD_OUTLINE, size, || {
                Ok(ui::tiles::render_card_outline_tile(card_w, card_h))
            })? {
                updated.push(tile::CARD_OUTLINE);
            }
        } else {
            self.grid_reveal_ready = true;
            self.spinner_since = None;
            if tiles.ensure_static(tile::NO_HOST, || {
                ui::tiles::render_text_tile(
                    text_cache,
                    fonts,
                    fonts.label,
                    "No host selected — pick one from the list, or add one.",
                    ui::style::theme().muted,
                )
            })? {
                updated.push(tile::NO_HOST);
            }
        }
        Ok(())
    }

    /// Uploads the launching game's hero art as `tile::HERO`, once, and starts its
    /// fade-in clock. Gated on the launch having actually started: at ~1600px wide this
    /// is a multi-MB texture, and putting one on the GPU for every card the user merely
    /// scrolls past would undo the whole point of the windowed card cache.
    fn prepare_hero(&mut self, updated: &mut Vec<TileId>) {
        if self.launch_anim.is_none() {
            return;
        }
        let Some(id) = self.hero.pending_upload() else { return };
        // One hero slot, so replacing one means dropping the old texture first —
        // `Compositor::upload_raw` treats an existing texture as already correct.
        if self.hero.mark_uploaded(id).is_some() {
            self.evicted_tiles.push(tile::HERO);
        }
        updated.push(tile::HERO);
    }

    /// Copies the leaving modal's pixels aside so it can fade out while the entering one
    /// rebuilds `tile::MODAL` — on the frame `advance_frame` started a close fade, only.
    /// Cloned rather than re-rendered: the left screen's state may already be torn down
    /// (a cleared `host_menu_index`), and these pixels are what was on screen last frame.
    fn snapshot_closing_modal(
        &mut self,
        tiles: &mut TileStore,
        fonts: &ui::text::Fonts,
        screen_w: u32,
        screen_h: u32,
        screen_changed: bool,
        updated: &mut Vec<TileId>,
    ) {
        let closing = self.modal_fade.closing_frame(MODAL_FADE_OUT);
        if closing.is_none() {
            // Fade over (or cancelled by reopening the same screen) — drop the copies
            // rather than keep two card-sized textures alive for nothing.
            if self.modal_prev.take().is_some() {
                tiles.remove(tile::MODAL_PREV);
                tiles.remove(tile::MODAL_PREV_CONTENT);
                self.evicted_tiles.push(tile::MODAL_PREV);
                self.evicted_tiles.push(tile::MODAL_PREV_CONTENT);
            }
            return;
        }
        let Some((_, left)) = closing.filter(|_| screen_changed) else {
            return;
        };
        let Some(shell) = tiles.get(tile::MODAL).cloned() else {
            return;
        };
        // Still the left card's — `modal_painter` moves it on later in this same call.
        let region = self.modal_tile_region;
        // The crop `compose_modal` was drawing for this screen last frame, frozen.
        let content = self
            .scroll_src_rect(left, screen_w, screen_h, fonts)
            .zip(tiles.get(tile::SCROLL_CONTENT).cloned())
            .map(|((src, dst), body)| (body, src, dst));
        tiles.put(tile::MODAL_PREV, cache::STATIC, shell);
        updated.push(tile::MODAL_PREV);
        let content = content.map(|(body, src, dst)| {
            tiles.put(tile::MODAL_PREV_CONTENT, cache::STATIC, body);
            updated.push(tile::MODAL_PREV_CONTENT);
            (src, dst)
        });
        self.modal_prev = Some(crate::app::render::ModalSnapshot { region, content });
    }

    /// Modal family: the open modal's full-screen shell tile (rebuilt only on content
    /// change, keyed by `ModalShellKey`) and its single focused-widget tile (keyed by
    /// `ModalFocusKey`). Extracted from `prepare_tiles` (A2 staging).
    #[allow(clippy::too_many_arguments)]
    fn prepare_modal(
        &mut self,
        tiles: &mut TileStore,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        screen_w: u32,
        screen_h: u32,
        content_dirty: bool,
        screen_changed: bool,
        updated: &mut Vec<TileId>,
    ) -> Result<()> {
        self.snapshot_closing_modal(tiles, fonts, screen_w, screen_h, screen_changed, updated);
        let modal_open = !matches!(self.screen, Screen::Home);
        // Every modal's shell only reacts to *content* changes — not to
        // `content_dirty`, which is also `true` on plain focus movement (see
        // `ModalShellKey`'s docs). `AddHost` has no `ModalShellKey` variant
        // (no split focus tile to protect) and just redraws on any
        // `content_dirty` tick, same as every modal did before this split.
        let modal_shell_key = match self.screen {
            Screen::Settings => Some(ModalShellKey::Settings {
                hover_close: self.hover_close,
            }),
            Screen::Wake => self.wake.as_ref().map(|w| ModalShellKey::Wake {
                name: w.name.clone(),
                mac_empty: w.mac.is_empty(),
                sent: w.sent,
                hover_close: self.hover_close,
            }),
            Screen::Pairing => Some(ModalShellKey::Pairing {
                digits: self.pin_digits,
                status: self.pairing_status.clone(),
                busy: self.pairing_busy,
                hover_close: self.hover_close,
            }),
            Screen::ForgetHost => Some(ModalShellKey::ForgetHost {
                name: self
                    .host_menu_index
                    .and_then(|i| self.entries.get(i))
                    .map(|e| e.name().to_string()),
                hover_close: self.hover_close,
            }),
            Screen::HostMenu => Some(ModalShellKey::HostMenu {
                name: self.host_menu_title(),
                subtitle: self.host_menu_subtitle(),
                rows: self.host_menu_actions().len(),
                hover_close: self.hover_close,
            }),
            Screen::WakeSettings => Some(ModalShellKey::WakeSettings {
                title: view::wakesettings::title(&self.host_menu_title()),
                auto: self.wake_settings_host().is_some_and(|h| h.wol_auto),
                hover_close: self.hover_close,
            }),
            Screen::About => Some(ModalShellKey::About {
                hover_close: self.hover_close,
            }),
            // The whole shell is derived from the status sentence, which already encodes
            // the phase and the latest measurement.
            Screen::SpeedTest => Some(ModalShellKey::SpeedTest {
                status: view::speedtest::status(self.speed_test.as_ref(), &self.speed_test_name),
                hover_close: self.hover_close,
            }),
            Screen::Diagnostics => Some(ModalShellKey::Diagnostics {
                log_level: self.settings.log_level_override,
                stats_overlay: self.settings.stats_overlay,
                show_logs: self.settings.show_logs,
                hover_close: self.hover_close,
            }),
            Screen::Experimental => Some(ModalShellKey::Experimental {
                video_pacing: self.settings.video_pacing,
                game_mode: self.settings.game_mode,
                hover_close: self.hover_close,
            }),
            Screen::CursorSettings => Some(ModalShellKey::CursorSettings {
                cursor_capture: self.settings.cursor_capture,
                cursor_gestures: self.settings.cursor_gestures,
                hover_close: self.hover_close,
            }),
            Screen::SendLogs => Some(ModalShellKey::SendLogs {
                hover_close: self.hover_close,
            }),
            // `EditHost` joins `AddHost` in having no shell key: its typed-digit
            // display has no separate focus tile to protect, so it just redraws on
            // any `content_dirty` tick — same for `PinLimit`, which is a fixed
            // message plus one always-focused button.
            Screen::Home | Screen::AddHost | Screen::EditHost | Screen::PinLimit => None,
        };
        let modal_stale = if modal_shell_key.is_some() {
            !tiles.contains(tile::MODAL) || self.modal_shell_key != modal_shell_key
        } else {
            content_dirty || !tiles.contains(tile::MODAL)
        };
        self.modal_shell_key = modal_shell_key;
        if modal_open && (screen_changed || modal_stale) {
            // Sized to the card's bounding box, not the whole screen: the render fns
            // below draw at absolute, screen-centered coordinates, and the painter's
            // origin shift (see `Painter::set_origin`) maps that geometry into the
            // smaller buffer.
            let mut p = self.modal_painter(screen_w, screen_h, fonts);
            let c = &mut ui::Canvas::new(&mut p, text_cache, fonts, screen_w, screen_h);
            let hover_close = self.hover_close;
            self.with_modal_screen(|s| s.render(c, hover_close)).transpose()?;
            // Staleness is decided above (the shell key is compared against the previous
            // frame's, not hashed), so the store just takes the result.
            tiles.put(tile::MODAL, cache::STATIC, p);
            updated.push(tile::MODAL);
        }
        // Whichever modal is open has at most one focused, zoom-animated widget
        // (`ModalFocusKey`'s docs) — `None` for screens with no such widget
        // (Home, AddHost) or when Wake has nothing to focus (no MAC on record,
        // see `handle_wake_event`'s matching guard).
        let focus_key = match self.screen {
            Screen::Settings => Some(ModalFocusKey::SettingsRow(
                self.settings_focused,
                self.settings,
                self.detected_gamepad_type,
            )),
            Screen::Wake => self
                .wake
                .as_ref()
                .filter(|w| !w.mac.is_empty())
                .map(|w| ModalFocusKey::WakeButton(w.focused)),
            Screen::Pairing => Some(match self.pairing_focus {
                PairingFocus::Pin => {
                    ModalFocusKey::PairingDigit(self.pin_digit_index, self.pin_digits[self.pin_digit_index])
                }
                PairingFocus::RequestAccess => ModalFocusKey::PairingButton,
            }),
            Screen::ForgetHost => Some(ModalFocusKey::ForgetButton(self.host_menu_focused)),
            Screen::HostMenu => self
                .host_menu_actions()
                .get(self.menu_focused)
                .map(|(_, row)| ModalFocusKey::MenuRow(self.menu_focused, row.label.clone(), self.host_menu_dots)),
            Screen::WakeSettings => Some(ModalFocusKey::WakeToggle(
                self.wake_settings_host().is_some_and(|h| h.wol_auto),
            )),
            // Only once there are buttons to focus — while measuring there is nothing
            // on the card but text.
            Screen::SpeedTest => view::speedtest::finished(self.speed_test.as_ref()).then(|| {
                let label = view::speedtest::apply_label(view::speedtest::recommendation(self.speed_test.as_ref()));
                ModalFocusKey::SpeedTestButton(self.speed_test_focused, label)
            }),
            Screen::Diagnostics => Some(ModalFocusKey::DiagnosticsRow(
                self.diagnostics_focused,
                self.settings.log_level_override,
                self.settings.stats_overlay,
                self.settings.show_logs,
            )),
            Screen::Experimental => Some(ModalFocusKey::ExperimentalRow(
                self.experimental_focused,
                self.settings.video_pacing,
                self.settings.game_mode,
            )),
            Screen::CursorSettings => Some(ModalFocusKey::CursorSettingsRow(
                self.cursor_settings_focused,
                self.settings.cursor_capture,
                self.settings.cursor_gestures,
            )),
            Screen::SendLogs => Some(ModalFocusKey::SendLogsButton(self.send_logs_focused)),
            // Neither has a single focused widget: the address form is one always-active
            // field, About is a scrolling document, and `PinLimit`'s one button is
            // always drawn focused directly in `render_pin_limit`.
            Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => None,
        };
        if let Some(key) = focus_key {
            // Also stale on every tick of an in-flight `switch_anim`: the knob's
            // position depends on elapsed time, not on `key`, which doesn't
            // change mid-flip.
            let stale = self.switch_anim.is_some() || !tiles.is_fresh(tile::MODAL_FOCUS, cache::version(&key));
            if stale {
                let tile = match self.screen {
                    Screen::Settings => {
                        let (_, content) = view::settings::layout(screen_w, screen_h);
                        let rows = self.settings_rows();
                        let dropdown_open = self.dropdown.as_ref().is_some_and(|dd| dd.row == self.settings_focused);
                        let target_on = rows.get(self.settings_focused).is_some_and(|r| r.value == "On");
                        ui::widgets::render_focus_row_tile(
                            text_cache,
                            fonts,
                            &rows,
                            content.width(),
                            self.settings_focused,
                            dropdown_open,
                            self.toggle_frac(target_on, self.settings_focused),
                        )?
                    }
                    // Every two-button confirm dialog shares the button geometry (one subtitle
                    // sizes the card, so one button row falls out of it); the button *labels*
                    // stay with the screen that owns them. `focus_key` is only `Some` here for
                    // a variant that has its buttons up, so both lookups resolve.
                    Screen::Wake | Screen::ForgetHost | Screen::SendLogs | Screen::SpeedTest => {
                        let subtitle = self
                            .confirm_subtitle()
                            .expect("focus_key is Some only for a confirm dialog showing buttons");
                        let i = self
                            .confirm_focused()
                            .expect("focus_key is Some only for a confirm dialog showing buttons");
                        let rect = Self::confirm_focus_button_rect(screen_w, screen_h, fonts, &subtitle, i);
                        // SpeedTest is the only one whose primary button has a dynamic label
                        // (the bitrate it would apply); bound out here so the borrow below
                        // outlives the array.
                        let speed_test_label = match self.screen {
                            Screen::SpeedTest => {
                                view::speedtest::apply_label(view::speedtest::recommendation(self.speed_test.as_ref()))
                            }
                            _ => String::new(),
                        };
                        let buttons = match self.screen {
                            Screen::Wake => view::wake::buttons(),
                            Screen::ForgetHost => view::forget::buttons(),
                            Screen::SendLogs => view::sendlogs::buttons(),
                            _ => view::speedtest::buttons(&speed_test_label),
                        };
                        ui::widgets::render_confirm_button_tile(
                            text_cache,
                            fonts,
                            &buttons[i],
                            rect.width(),
                            rect.height(),
                        )?
                    }
                    Screen::Pairing => match self.pairing_focus {
                        PairingFocus::Pin => {
                            view::pairing::render_digit_tile(text_cache, fonts, self.pin_digits[self.pin_digit_index])?
                        }
                        PairingFocus::RequestAccess => {
                            let card = view::pairing::card_rect(screen_w, screen_h, fonts);
                            let btn = view::pairing::request_button_rect(card, fonts);
                            view::pairing::render_button_tile(text_cache, fonts, btn.width(), btn.height())?
                        }
                    },
                    Screen::HostMenu => {
                        let mut rows = self.host_menu_rows();
                        // The only place a row's ⋯ is drawn lit — see `host_menu_actions`.
                        if let Some(row) = rows.get_mut(self.menu_focused) {
                            row.menu = row.menu.map(|_| self.host_menu_dots);
                        }
                        let content = self.modal_list_content(screen_w, screen_h, fonts);
                        ui::widgets::render_focus_row_tile(
                            text_cache,
                            fonts,
                            &rows,
                            content.width(),
                            self.menu_focused,
                            false,
                            0.0,
                        )?
                    }
                    // The plain list modals: same tile, same geometry, built from whichever
                    // rows this screen shows. Only Diagnostics can have a dropdown open.
                    Screen::WakeSettings | Screen::Diagnostics | Screen::Experimental | Screen::CursorSettings => {
                        let rows = match self.screen {
                            Screen::WakeSettings => {
                                view::wakesettings::rows(self.wake_settings_host().is_some_and(|h| h.wol_auto))
                            }
                            Screen::Diagnostics => view::diagnostics::rows(&self.settings),
                            Screen::Experimental => view::experimental::rows(&self.settings, Self::rooted()),
                            _ => view::cursorsettings::rows(&self.settings),
                        };
                        let focused = self
                            .list_modal_focused()
                            .expect("focus_key is Some only for a screen with a focused row");
                        let content = self.modal_list_content(screen_w, screen_h, fonts);
                        let dropdown_open = self.dropdown.as_ref().is_some_and(|dd| dd.row == focused);
                        let target_on = rows.get(focused).is_some_and(|r| r.value == "On");
                        ui::widgets::render_focus_row_tile(
                            text_cache,
                            fonts,
                            &rows,
                            content.width(),
                            focused,
                            dropdown_open,
                            self.toggle_frac(target_on, focused),
                        )?
                    }
                    Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => {
                        unreachable!("focus_key checked above")
                    }
                };
                tiles.put(tile::MODAL_FOCUS, cache::version(&key), tile);
                updated.push(tile::MODAL_FOCUS);
            }
        } else {
            tiles.remove(tile::MODAL_FOCUS);
        }
        Ok(())
    }

    /// Dropdown family: the overlay panel + focused-option tile for an open Settings/Diagnostics dropdown; cleared when closed (unless a close-fade still needs them). Extracted from `prepare_tiles`.
    fn prepare_dropdown(
        &mut self,
        tiles: &mut TileStore,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        screen_w: u32,
        screen_h: u32,
        updated: &mut Vec<TileId>,
    ) -> Result<()> {
        if let Some(dd) = &self.dropdown {
            let (options, content_w) = match self.screen {
                Screen::Diagnostics => {
                    let content = self.modal_list_content(screen_w, screen_h, fonts);
                    (menu::log_level_dropdown_options(), content.width())
                }
                _ => {
                    let (_, content) = view::settings::layout(screen_w, screen_h);
                    let logical = menu::settings_logical_row(dd.row);
                    (
                        menu::dropdown_options(logical, self.detected_gamepad_type),
                        content.width(),
                    )
                }
            };

            // Keyed by screen as well as row: row 0 means a different setting on Settings
            // than it does on Diagnostics.
            let overlay = cache::version(&(self.screen, dd.row));
            if tiles.ensure(tile::DROPDOWN_OVERLAY, overlay, || {
                let overlay_h = options.len() as u32 * ui::widgets::DROPDOWN_OPTION_H;
                let mut p = Painter::new(content_w, overlay_h.max(1));
                let rect = Rect::new(0, 0, content_w, overlay_h);
                ui::Canvas::tile(&mut p, text_cache, fonts)
                    .render(ui::widgets::DropdownOverlay::new(&options), rect)?;
                Ok(p)
            })? {
                updated.push(tile::DROPDOWN_OVERLAY);
            }

            let focused = cache::version(&(self.screen, dd.row, dd.focused));
            if tiles.ensure(tile::DROPDOWN_FOCUS, focused, || {
                let option = options.get(dd.focused).map_or("", String::as_str);
                ui::widgets::render_dropdown_option_tile(text_cache, fonts, option, content_w)
            })? {
                updated.push(tile::DROPDOWN_FOCUS);
            }
        } else if self.dropdown_fade.closing_frame(DROPDOWN_FADE).is_none() {
            // Keep the tiles cached while a close-fade is in flight — `draw_list`
            // still composites them at falling alpha.
            tiles.remove(tile::DROPDOWN_OVERLAY);
            tiles.remove(tile::DROPDOWN_FOCUS);
        }
        Ok(())
    }

    /// Scroll family: the indicator, edge-fade ramps, and windowed content tile for whichever modal overflows (Settings rows / About document). Extracted from `prepare_tiles`.
    fn prepare_scroll(
        &mut self,
        tiles: &mut TileStore,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        screen_w: u32,
        screen_h: u32,
        updated: &mut Vec<TileId>,
    ) -> Result<()> {
        // Whichever modal's content overflows its viewport (Settings' rows, About's
        // document) gets its scroll indicator and content tile refreshed here — see
        // `scroll_geometry`'s docs for why this one block covers every such modal
        // instead of being hand-copied per screen.
        if matches!(self.screen, Screen::About) {
            // Mutates `about_wrapped` only — must happen before `scroll_geometry`
            // (a `&self` read) can report a non-zero total for this frame.
            let card = view::about::card_rect(screen_w, screen_h);
            let body = view::about::body_rect(card, fonts);
            self.ensure_about_wrapped(fonts, body.width());
        }
        if let Some((total, visible, _, content)) = self.scroll_geometry(screen_w, screen_h, fonts) {
            let scroll = self.scroll.clamped(total, visible);
            let ind = cache::version(&(self.screen, total, visible, scroll, content.height()));
            if tiles.ensure(tile::SCROLL_INDICATOR, ind, || {
                Ok(ui::widgets::render_list_scrollbar_tile(
                    SCROLL_INDICATOR_TILE_W,
                    content.height(),
                    total,
                    visible,
                    scroll,
                ))
            })? {
                updated.push(tile::SCROLL_INDICATOR);
            }
            // Static ramps, so these are once-per-run bakes rather than keyed rebuilds —
            // scrolling and resizing both leave them valid (the GPU restretches them).
            if tiles.ensure_static(tile::SCROLL_FADE, || {
                Ok(ui::widgets::render_scroll_fade_tile(ui::widgets::FadeEdge::Bottom))
            })? {
                updated.push(tile::SCROLL_FADE);
            }
            if tiles.ensure_static(tile::SCROLL_FADE_TOP, || {
                Ok(ui::widgets::render_scroll_fade_tile(ui::widgets::FadeEdge::Top))
            })? {
                updated.push(tile::SCROLL_FADE_TOP);
            }
            let stride = self.scroll_stride(fonts);
            self.sync_modal_scroll(self.screen, total, visible, content.height(), stride);

            match self.screen {
                Screen::Settings => {
                    let dropdown_row = self.dropdown.as_ref().map(|dd| dd.row);
                    let key = cache::version(&(
                        Screen::Settings,
                        ScrollContentKey::Settings(self.settings, dropdown_row, self.detected_gamepad_type),
                    ));
                    if !tiles.is_fresh(tile::SCROLL_CONTENT, key) {
                        let rows = self.settings_rows();
                        let tile = ui::widgets::render_focus_rows_tile(
                            text_cache,
                            fonts,
                            &rows,
                            content.width(),
                            dropdown_row,
                        )?;
                        tiles.put(tile::SCROLL_CONTENT, key, tile);
                        // Settings' whole row list always fits one tile — no windowing.
                        self.content_window = ui::scroll::ContentWindow {
                            start: 0,
                            len: menu::settings_row_count(),
                        };
                        updated.push(tile::SCROLL_CONTENT);
                    }
                }
                Screen::About => {
                    if let Some(new_start) = self.content_window.recenter_if_needed(
                        scroll,
                        visible,
                        total,
                        ABOUT_WINDOW_BUDGET,
                        ABOUT_WINDOW_MARGIN,
                    ) {
                        let len = ABOUT_WINDOW_BUDGET.min(total.saturating_sub(new_start));
                        if let Some((_, wrapped)) = &self.about_wrapped {
                            let stride = self.scroll_stride(fonts) as u32;
                            let mut p = Painter::new(content.width().max(1), (len as u32 * stride).max(1));
                            let mut c = ui::Canvas::tile(&mut p, text_cache, fonts);
                            view::about::draw_window(&mut c, fonts.value, wrapped, new_start, len)?;
                            self.content_window = ui::scroll::ContentWindow { start: new_start, len };
                            let key = cache::version(&(Screen::About, ScrollContentKey::About(new_start)));
                            tiles.put(tile::SCROLL_CONTENT, key, p);
                            updated.push(tile::SCROLL_CONTENT);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Warms the caches a modal's first open would otherwise fill cold, off the
    /// open path — call once on an idle Home frame. The first real open pays a
    /// burst of `SDL2_ttf` glyph renders plus the card's shadow blur; on the very
    /// first open of a session nothing is cached, which is the hitch the user
    /// sees. Rendering the Settings shell + rows into throwaway painters here has
    /// three surviving side effects: `text_cache` (per-line pixmaps), the
    /// thread-local shadow/glow caches, and `SDL2_ttf`'s own freetype glyph cache.
    /// The last is font-wide, so it speeds up every modal's cold renders — not
    /// just Settings — even where the exact strings differ (e.g. the host menu).
    pub fn prewarm_modal_caches(
        &self,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<()> {
        let mut scratch = Painter::new(screen_w, screen_h);
        view::settings::render(
            &mut ui::Canvas::new(&mut scratch, text_cache, fonts, screen_w, screen_h),
            self.hover_close,
        )?;
        let (_, content) = view::settings::layout(screen_w, screen_h);
        let rows = self.settings_rows();
        let _ = ui::widgets::render_focus_rows_tile(text_cache, fonts, &rows, content.width(), None)?;
        let _ = ui::widgets::render_focus_row_tile(text_cache, fonts, &rows, content.width(), 0, false, 0.0)?;
        Ok(())
    }

    /// Rasterizes every stale tile (tiny-skia, CPU — the only place rasterization
    /// happens) and returns which tiles need their GPU texture re-uploaded.
    /// `content_dirty` is the main loop's "an event/drain changed something this
    /// tick" flag — it forces the open modal's tile to re-rasterize, since modal
    /// content has no finer dirty tracking of its own. Pure animation frames pass
    /// `false` and rasterize nothing at all. Call `advance_frame` first.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_tiles(
        &mut self,
        tiles: &mut TileStore,
        text_cache: &mut crate::ui::text::TextCache,
        fonts: &ui::text::Fonts,
        screen_w: u32,
        screen_h: u32,
        content_dirty: bool,
        screen_changed: bool,
    ) -> Result<Vec<TileId>> {
        let mut updated = Vec::new();
        // The transition frame's cost is what decides whether the open fade is visible.
        let started = screen_changed.then(Instant::now);
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        // `self.card_size` is set by `advance_frame` (same formula) before this runs; the
        // local copy is what the tile-build loop below reads.
        let (card_w, card_h) = view::home::grid_card_size(available_w, columns);

        self.prepare_sidebar(tiles, text_cache, fonts, screen_h, &mut updated)?;

        self.prepare_grid(
            tiles,
            text_cache,
            fonts,
            columns,
            card_w,
            card_h,
            screen_h,
            &mut updated,
        )?;

        self.prepare_hero(&mut updated);

        // Status line block — built whenever `home_status` is set, independent of
        // whether a host is selected (the "Send logs" result shows here too).
        match &self.home_status {
            Some(s) => {
                if tiles.ensure(tile::STATUS, cache::version(s), || {
                    let avail = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
                    let max_w = avail.saturating_sub(2 * view::home::GRID_PAD as u32);
                    ui::tiles::render_wrapped_text_tile(
                        text_cache,
                        fonts,
                        fonts.label,
                        s,
                        max_w,
                        ui::style::theme().muted,
                        6,
                    )
                })? {
                    updated.push(tile::STATUS);
                }
            }
            None => {
                tiles.remove(tile::STATUS);
            }
        }

        self.prepare_modal(
            tiles,
            text_cache,
            fonts,
            screen_w,
            screen_h,
            content_dirty,
            screen_changed,
            &mut updated,
        )?;

        self.prepare_dropdown(tiles, text_cache, fonts, screen_w, screen_h, &mut updated)?;

        self.prepare_scroll(tiles, text_cache, fonts, screen_w, screen_h, &mut updated)?;

        // Entering a modal rasterizes it — shell, rows, and (Settings/About) the full
        // content strip: tens of ms on this SoC, more than the fade itself. `advance_frame`
        // started the clocks before all that, so re-stamp them here and the fade is
        // measured from the first frame that can show it.
        if let Some(started) = started {
            tracing::debug!(
                "entered {:?}: {} tiles rasterized in {:?}",
                self.screen,
                updated.len(),
                started.elapsed()
            );
            self.modal_fade.restart();
        }
        Ok(updated)
    }
}
