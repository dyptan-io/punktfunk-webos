//! Rasterization: which tiles are stale this frame, and what to draw into each.
//!
//! The CPU half of the render path — the only place `tiny_skia` runs. Split out of
//! `app/mod.rs`, which held the state machine and both render halves in one 3.4k-line file.
//! Each `prepare_*` covers one family (sidebar, grid, hero, modal, dropdown, scroll) and
//! reports the tiles it rebuilt so `runtime` can re-upload their textures.
use std::time::Instant;

use anyhow::Result;

use crate::app::grid::{CARD_BUILD_BUDGET, CARD_BUILD_BURST, CARD_KEEP_ROWS, CARD_PREFETCH_ROWS};
use crate::app::hosts::HostEntry;
use crate::app::nav::ScreenKey;
use crate::app::render::ctx::RenderCtx;
use crate::app::render::key::{ModalFocusKey, ModalShellKey, ScrollContentKey};
use crate::app::render::tile;
use crate::app::{
    menu, view, App, HomeFocus, PairingFocus, Screen, ABOUT_WINDOW_BUDGET, ABOUT_WINDOW_MARGIN, DROPDOWN_FADE,
    MODAL_FADE_OUT, SCROLL_INDICATOR_TILE_W,
};
use crate::ui;
use crate::ui::cache;
use crate::ui::render::{Rect, TileId};
use crate::ui::Painter;

impl App {
    /// Sidebar family: the focus-free strip (rebuilt on content change) plus the
    /// single focused-row overlay tile. Pushes any rebuilt tiles onto `updated`.
    /// Extracted from `prepare_tiles` as a self-contained family (A2 staging).
    fn prepare_sidebar(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            screen,
            updated,
            ..
        } = ctx;
        let screen_h = screen.h;
        // Kept on the `sidebar_dirty` flag rather than a content version: the strip is
        // built from every entry plus its reachability, and hashing all of that once a
        // frame would cost more than the flag the event side already maintains.
        if self.render.sidebar_dirty || !tiles.contains(tile::SIDEBAR) {
            let selected = self.sidebar_index_of_selected_host();
            let entries = &self.hosts.entries;
            let reach = self.reachability_list();
            // Reuses the existing full-height strip as its own scratch surface — several MB
            // that would otherwise be reallocated on every host list change.
            // The outer condition has already decided this is stale, so the version just
            // has to differ from the last one — `ensure_in_place` would otherwise see an
            // unchanged `STATIC` and skip the rebuild it was called to do.
            self.render.sidebar_gen = self.render.sidebar_gen.wrapping_add(1);
            tiles.ensure_in_place(
                tile::SIDEBAR,
                self.render.sidebar_gen,
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
            self.render.sidebar_dirty = false;
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
            let online = self.hosts.entries.get(key.0).and_then(|e| self.entry_online(e));
            if tiles.ensure(tile::FOCUS_ROW, cache::version(&key), || {
                ui::rasterize(
                    view::sidebar::FocusedRowTile {
                        entries: &self.hosts.entries,
                        index: key.0,
                        menu_focused: key.1,
                        online,
                    },
                    text_cache,
                    fonts,
                )
            })? {
                updated.push(tile::FOCUS_ROW);
            }
        }
        Ok(())
    }

    /// Whether this frame's grid pass can be skipped outright: a modal owns the screen, the
    /// grid has already revealed, and nothing has invalidated a card behind it.
    fn grid_window_frozen(&self) -> bool {
        !matches!(self.nav.screen, Screen::Home)
            && self.render.grid.reveal.is_revealed()
            && !self.render.grid.dirty
            && self.render.grid.cards_dirty.is_empty()
    }

    /// Grid family: windowed/budgeted card-tile building, eviction, the reveal
    /// spinner, and the shared ring/outline/pin-badge tiles — or the "no host"
    /// hint when nothing is selected. Extracted from `prepare_tiles` (A2 staging).
    fn prepare_grid(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            screen: size,
            updated,
            ..
        } = ctx;
        let (screen_w, screen_h) = (size.w, size.h);
        // The same three numbers `advance_frame` sized `self.render.grid.card_size` from — the grid's
        // whole geometry follows from the width it has to fill.
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        let (card_w, card_h) = view::home::grid_card_size(available_w, columns);
        // Reset before the branch: it is only ever set inside it, and a stale `true` left
        // behind by a host that has since been deselected would spin the render loop at
        // full rate forever.
        self.render.grid.tiles_pending = false;
        if self.library.selected_host.is_some() {
            // Nothing behind an open modal can come into view: the grid neither scrolls nor
            // moves focus while a modal owns input, so the whole windowed pass — the one cost
            // here that scales with the window — is skipped unless a card was actually
            // invalidated. The grid still composites under the modal's scrim from the tiles it
            // already holds.
            if self.grid_window_frozen() {
                return Ok(());
            }
            let count = self.grid_len(columns);
            // Captured before it's cleared below: a fresh library load is the only
            // rebuild that also re-arms the spinner.
            let full_reset = self.render.grid.dirty;
            if full_reset {
                // Every existing texture is stale (different games, different host) —
                // drop them rather than leaving them to be overwritten one by one,
                // which would strand the tail of a longer previous library.
                for id in self.render.grid.card_ids.release_all() {
                    tiles.remove(id);
                    self.render.evicted_tiles.push(id);
                }
                self.render.grid.card_pop_until = None;
                self.render.grid.dirty = false;
                self.render.grid.cards_dirty.clear();
                // Scrolling or re-pinning a card must not hide the already-visible
                // grid behind the spinner again.
                self.render.grid.reveal.restart();
            } else {
                for id in std::mem::take(&mut self.render.grid.cards_dirty) {
                    if let Some(t) = self.render.grid.card_ids.release(&id) {
                        tiles.remove(t);
                    }
                }
            }

            // Windowed, budgeted tile building — see `CARD_BUILD_BUDGET`. Both windows are
            // index ranges, so every pass below iterates the window rather than the library.
            let row_h = card_h as i32 + view::home::GRID_GAP;
            let visible_rows = (screen_h as i32 - view::home::GRID_TOP_Y).max(row_h) / row_h + 1;
            let first_visible_row = (self.render.grid.scroll / row_h).max(0);
            let rows = count.div_ceil(columns.max(1)) as i32;
            // Row band -> index range, clamped to the library. Deliberately ignores the
            // section headings' offsets: a row's worth of slack either way is what
            // `CARD_PREFETCH_ROWS`/`CARD_KEEP_ROWS` already exist to absorb.
            let window = |lo: i32, hi: i32| {
                let lo = lo.clamp(0, rows) as usize * columns.max(1);
                let hi = (hi + 1).clamp(0, rows) as usize * columns.max(1);
                lo.min(count)..hi.min(count)
            };
            // `mut` because the reveal check below consumes it as an iterator.
            let mut build_window = window(
                first_visible_row - CARD_PREFETCH_ROWS,
                first_visible_row + visible_rows + CARD_PREFETCH_ROWS,
            );
            let keep_window = window(
                first_visible_row - CARD_KEEP_ROWS,
                first_visible_row + visible_rows + CARD_KEEP_ROWS,
            );

            // Held by value, not re-derived per index — and, unlike the `App`
            // helpers, it maps indices without borrowing all of `self`, so the art
            // lookups below can sit next to `&mut self.jobs.art`.
            let layout = self.grid_layout(columns);

            // Evict first, so a long scroll frees textures in the same frame it needs new
            // ones rather than a frame later.
            //
            // Driven off what is resident (`CardIds`) against the keep window, not off a scan
            // of the whole library: the resident set is bounded by the window, so this costs
            // the window twice over however large the library gets.
            //
            // By tile id rather than by pin id: the ids are `Copy` integers, so the keep set
            // is a sorted window-sized vector and the test is a binary search — where a
            // `HashSet<&str>` re-hashed every resident card's id string every frame. Both
            // lists are `GridState` scratch, cleared and refilled rather than allocated.
            let mut keep = std::mem::take(&mut self.render.grid.scratch.keep);
            keep.clear();
            keep.extend(
                keep_window
                    .filter_map(|idx| layout.pin_id_at(&self.library.games, idx))
                    .filter_map(|id| self.render.grid.card_ids.get(id)),
            );
            keep.sort_unstable();
            let mut dropped = std::mem::take(&mut self.render.grid.scratch.dropped);
            dropped.clear();
            dropped.extend(
                self.render.grid
                    .card_ids
                    .entries()
                    .filter(|(_, t)| keep.binary_search(t).is_err())
                    .map(|(id, _)| id.to_string()),
            );
            self.render.grid.scratch.keep = keep;
            for id in dropped.drain(..) {
                if let Some(t) = self.render.grid.card_ids.release(&id) {
                    // Kept for the cards built below: a scroll frees and needs the same
                    // card-sized pixmap in the same frame (see `GridState::free_cards`).
                    if let Some(painter) = tiles.take(t) {
                        self.render.grid.free_cards.push(painter);
                    }
                    self.render.evicted_tiles.push(t);
                }
                // Drop the decoded cover too — it is several times the size of the tile it
                // feeds. Re-requested from the disk cache on scroll back. (Nothing to drop for
                // the pinned "Desktop" entry, which has no art at all.)
                self.library.art.remove(&id);
                if let Some(loader) = &mut self.jobs.art {
                    loader.forget(&id);
                }
            }

            // Ready once nothing more can arrive: cover already in `self.library.art`, or the game
            // never had one to fetch (no `self.library.art` entry either way). "Desktop" and the
            // padding after a partial pinned row have no `games` entry and are always ready.
            let art_ready = |idx: usize| {
                layout.game_at(&self.library.games, idx).is_none_or(|game| {
                    self.library.art.contains_key(&game.id) || (game.art.portrait.is_none() && game.art.header.is_none())
                })
            };

            // Art-ready cards build first — building one before its cover arrives just
            // burns a second budget slot re-dirtying it once the cover shows up. Two lists
            // rather than one sorted one, and indices rather than ids: a candidate past the
            // budget is never built, so nothing should be copied on its behalf.
            let mut ready = std::mem::take(&mut self.render.grid.scratch.ready);
            let mut waiting = std::mem::take(&mut self.render.grid.scratch.waiting);
            ready.clear();
            waiting.clear();
            for idx in build_window.clone() {
                // Nothing to build or fetch art for in the padding after a partial
                // pinned row.
                let Some(id) = layout.pin_id_at(&self.library.games, idx) else {
                    continue;
                };
                // Ask for this card's cover as it enters the window, not for the whole
                // library at once (see `art::ArtLoader`).
                if let (Some(loader), Some(game)) = (&mut self.jobs.art, layout.game_at(&self.library.games, idx)) {
                    loader.request(game);
                }
                if self.render.grid.card_ids.get(id).is_some_and(|t| tiles.contains(t)) {
                    continue;
                }
                if art_ready(idx) {
                    ready.push(idx);
                } else {
                    waiting.push(idx);
                }
            }

            let mut pending = false;
            let budget_from = Instant::now();
            let mut built = 0usize;
            for idx in ready.iter().copied().chain(waiting.iter().copied()) {
                // Counted on cards actually rasterized, not on candidates seen: the budget is
                // a time budget, and skipping a padding slot costs none of it.
                if built >= CARD_BUILD_BURST || (built > 0 && budget_from.elapsed() >= CARD_BUILD_BUDGET) {
                    pending = true;
                    break;
                }
                let Some(id) = layout.pin_id_at(&self.library.games, idx).map(str::to_string) else {
                    continue;
                };
                built += 1;
                let recycled = self.render.grid.free_cards.pop();
                let tile = {
                    let (title, art) = self.grid_card_content(idx, columns);
                    ui::rasterize_into(
                        ui::tiles::CardTile {
                            w: card_w,
                            h: card_h,
                            title,
                            art,
                        },
                        recycled,
                        text_cache,
                        fonts,
                    )?
                };
                let tile_id = self.render.grid.card_ids.id(&id);
                tiles.put(tile_id, cache::STATIC, tile);
                if self.render.grid.reveal.is_revealed() {
                    self.render.grid.arm_card_pop(&id, Instant::now());
                }
                updated.push(tile_id);
            }
            // Anything still here evicted without a card being built in its place, so it is
            // surplus rather than churn — held past the frame it would just be a cache of
            // pixmaps nothing asked for.
            self.render.grid.free_cards.clear();
            self.render.grid.scratch.dropped = dropped;
            self.render.grid.scratch.ready = ready;
            self.render.grid.scratch.waiting = waiting;
            self.render.grid.tiles_pending = pending;

            // Prefetch the focused card's hero, so the connecting screen has one ready the
            // moment OK is pressed. Deduped in the loader, and the fetched bytes are
            // disk-cached, so hovering back over a card costs no round trip.
            //
            // Only once the visible window has settled: the loader serves hero requests
            // ahead of card art, so queueing one mid-scroll would put the cards the user is
            // actually looking at behind a full-screen fetch and decode.
            if self.render.grid.reveal.is_revealed() && !pending {
                if let HomeFocus::Grid(focus_idx) = self.home_focus {
                    if let Some(game) = layout.game_at(&self.library.games, focus_idx) {
                        if let Some(loader) = &mut self.jobs.art {
                            loader.request_hero(game);
                        }
                        self.render.hero.want(&game.id);
                    }
                }
            }

            // The focused card's title strip: its own tile, so the wipe in `draw_list` is
            // a moving source/destination rect — one small blur per focus move instead of
            // re-rasterizing the card every animation frame.
            if let HomeFocus::Grid(idx) = self.home_focus {
                if let Some(pin_id) = layout.pin_id_at(&self.library.games, idx) {
                    let (title, art) = self.grid_card_content(idx, columns);
                    // Keyed by card identity like the card tiles themselves (`CardIds`),
                    // not by title — two games can share one.
                    let overridden = self.game_has_overrides(pin_id);
                    let version = cache::version(&(pin_id, card_w, card_h, art.is_some(), overridden));
                    if tiles.ensure(tile::CARD_TITLE, version, || {
                        ui::rasterize(
                            ui::tiles::CardTitleTile {
                                card_w,
                                card_h,
                                title,
                                art,
                                overridden,
                            },
                            text_cache,
                            fonts,
                        )
                    })? {
                        updated.push(tile::CARD_TITLE);
                    }

                    // The submenu panel a hold raises: the same strip grown to carry the
                    // rows (see `ui::tiles::CardMenuTile`), on the same wipe.
                    //
                    // Built when the card takes focus, not when the menu opens — it costs a
                    // full-card art rescale plus a radius-6 blur, and paying that on the
                    // frame the rise starts is what made the panel appear to wait for the
                    // button to come back up. Held off until the grid has settled so a fast
                    // scroll doesn't pay it per card, unless a menu is already up (which
                    // can only happen on a settled grid anyway).
                    let menu_open = self.card_menu.as_ref().is_some_and(|m| m.pin_id == pin_id);
                    if menu_open || (self.render.grid.reveal.is_revealed() && !pending) {
                        let rows = self.card_menu_rows(pin_id);
                        // No focused row in the key: the selection is a `DrawCmd::Fill` laid
                        // over this tile, so moving between the menu's rows rebuilds nothing.
                        let version = cache::version(&(pin_id, card_w, card_h, art.is_some(), &rows, overridden));
                        if tiles.ensure(tile::CARD_MENU, version, || {
                            ui::rasterize(
                                ui::tiles::CardMenuTile {
                                    card_w,
                                    card_h,
                                    title,
                                    art,
                                    rows: &rows,
                                },
                                text_cache,
                                fonts,
                            )
                        })? {
                            updated.push(tile::CARD_MENU);
                        }
                        if tiles.ensure(tile::CARD_MENU_ROWS, version, || {
                            ui::rasterize(
                                ui::tiles::CardMenuRowsTile {
                                    card_w,
                                    card_h,
                                    rows: &rows,
                                    // The dot follows what owns it: the title while the strip is
                                    // collapsed, the Settings row once the panel is up.
                                    marked: overridden.then_some(crate::app::state::cardmenu::ROW_SETTINGS),
                                },
                                text_cache,
                                fonts,
                            )
                        })? {
                            updated.push(tile::CARD_MENU_ROWS);
                        }
                        if tiles.ensure(tile::CARD_MENU_TITLE, version, || {
                            ui::rasterize(
                                ui::tiles::CardMenuTitleTile { card_w, card_h, title },
                                text_cache,
                                fonts,
                            )
                        })? {
                            updated.push(tile::CARD_MENU_TITLE);
                        }
                        // The selection band's rounded-bottom variant, for the row that ends
                        // on the card's bottom edge. Keyed by width alone: it is one flat
                        // colour, so nothing else about the card changes it.
                        if tiles.ensure(tile::CARD_MENU_BAND, cache::version(&card_w), || {
                            ui::rasterize(ui::tiles::CardMenuBandTile { card_w }, text_cache, fonts)
                        })? {
                            updated.push(tile::CARD_MENU_BAND);
                        }
                    }
                }
            }

            // The grid's section headings. Built unconditionally like the pin badge: two
            // lines of text, and whether they are *drawn* is the compose path's call.
            for (id, label) in [
                (tile::SECTION_PINNED, crate::app::view::home::SECTION_PINNED_LABEL),
                (tile::SECTION_LIBRARY, crate::app::view::home::SECTION_LIBRARY_LABEL),
            ] {
                if tiles.ensure_static(id, || {
                    ui::rasterize(
                        ui::tiles::TextTile {
                            font: fonts.title,
                            text: label,
                            color: ui::style::theme().muted,
                        },
                        text_cache,
                        fonts,
                    )
                })? {
                    updated.push(id);
                }
            }

            // The pinned badge tile — built once, composited over the focused
            // card in `draw_list` rather than baked into individual card tiles.
            if tiles.ensure_static(tile::PIN_BADGE, || {
                ui::rasterize(ui::tiles::PinBadgeTile, text_cache, fonts)
            })? {
                updated.push(tile::PIN_BADGE);
            }

            if !self.render.grid.reveal.is_revealed() {
                // Rechecks the whole window rather than trusting `!pending`, since a card built
                // earlier can still be waiting behind a re-dirtied sibling; requires `art_ready`
                // too so a placeholder built this tick can't count as revealed.
                let window_ready = || {
                    build_window.all(|idx| {
                        layout.pin_id_at(&self.library.games, idx).is_none_or(|id| {
                            self.render.grid.card_ids.get(id).is_some_and(|t| tiles.contains(t)) && art_ready(idx)
                        })
                    })
                };
                let next_frame = self.render.grid.reveal.advance(self.library_fetch_in_flight(), window_ready);
                match next_frame {
                    Some(idx) => updated.push(tile::spinner(idx)),
                    // Everything built behind the spinner becomes visible in this one frame, so
                    // it all zooms in off a single clock.
                    None if self.render.grid.reveal.is_revealed() => {
                        let now = Instant::now();
                        let ids: Vec<String> = self.render.grid.card_ids.pin_ids().map(str::to_string).collect();
                        for id in ids {
                            self.render.grid.arm_card_pop_if_idle(&id, now);
                        }
                    }
                    None => {}
                }
            }

            // One shared tile at the current card size, so the card size *is* the
            // version — a resolution change rebuilds it, nothing else does.
            let size = cache::version(&(card_w, card_h));
            if tiles.ensure(tile::RING, size, || {
                ui::rasterize(ui::tiles::FocusRingTile { w: card_w, h: card_h }, text_cache, fonts)
            })? {
                updated.push(tile::RING);
            }
            if tiles.ensure(tile::CARD_SHADOW, size, || {
                ui::rasterize(ui::tiles::CardShadowTile { w: card_w, h: card_h }, text_cache, fonts)
            })? {
                updated.push(tile::CARD_SHADOW);
            }
            if tiles.ensure(tile::CARD_OUTLINE, size, || {
                ui::rasterize(ui::tiles::CardOutlineTile { w: card_w, h: card_h }, text_cache, fonts)
            })? {
                updated.push(tile::CARD_OUTLINE);
            }
        } else {
            self.render.grid.reveal.reveal();
            if tiles.ensure_static(tile::NO_HOST, || {
                ui::rasterize(
                    ui::tiles::TextTile {
                        font: fonts.label,
                        text: "No host selected — pick one from the list, or add one.",
                        color: ui::style::theme().muted,
                    },
                    text_cache,
                    fonts,
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
    fn prepare_hero(&mut self, ctx: &mut RenderCtx<'_>) {
        let updated = &mut ctx.updated;
        if self.launch_anim.is_none() {
            return;
        }
        let Some(id) = self.render.hero.pending_upload() else { return };
        // One hero slot: the upload replaces whatever it held (`Compositor::upload_raw`),
        // reusing the texture when the two images happen to share a size.
        self.render.hero.mark_uploaded(id);
        updated.push(tile::HERO);
    }

    /// Copies the leaving modal's pixels aside so it can fade out while the entering one
    /// rebuilds `tile::MODAL` — on the frame `advance_frame` started a close fade, only.
    /// Cloned rather than re-rendered: the left screen's state may already be torn down
    /// (a cleared `host_menu_index`), and these pixels are what was on screen last frame.
    fn snapshot_closing_modal(&mut self, ctx: &mut RenderCtx<'_>) {
        let RenderCtx {
            tiles,
            fonts,
            screen: size,
            screen_changed,
            updated,
            ..
        } = ctx;
        let screen_changed = *screen_changed;
        let (screen_w, screen_h) = (size.w, size.h);
        let closing = self.render.modal.fade.closing_frame(MODAL_FADE_OUT);
        if closing.is_none() {
            // Fade over (or cancelled by reopening the same screen) — drop the copies
            // rather than keep two card-sized textures alive for nothing.
            if self.render.modal.prev.take().is_some() {
                tiles.remove(tile::MODAL_PREV);
                tiles.remove(tile::MODAL_PREV_CONTENT);
                self.render.evicted_tiles.push(tile::MODAL_PREV);
                self.render.evicted_tiles.push(tile::MODAL_PREV_CONTENT);
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
        let region = self.render.modal.tile_region;
        // The crop `compose_modal` was drawing for this screen last frame, frozen. Settings
        // has no single body tile to freeze — its rows are a tile each — so one is stitched
        // from them at the geometry they were just drawn at. That is a blit per row and no
        // rasterization, and it keeps the fade-out a snapshot rather than a live list the
        // leaving screen would have to keep its tiles alive for.
        let body = match left {
            Screen::Settings(_) => self.stitch_settings_body(left, tiles, screen_w, screen_h, fonts),
            _ => tiles.get(tile::SCROLL_CONTENT).cloned(),
        };
        let content = self
            .scroll_src_rect(left, screen_w, screen_h, fonts)
            .zip(body)
            .map(|((src, dst), body)| (body, src, dst));
        tiles.put(tile::MODAL_PREV, cache::STATIC, shell);
        updated.push(tile::MODAL_PREV);
        let content = content.map(|(body, src, dst)| {
            tiles.put(tile::MODAL_PREV_CONTENT, cache::STATIC, body);
            updated.push(tile::MODAL_PREV_CONTENT);
            (src, dst)
        });
        self.render.modal.prev = Some(crate::app::render::ModalSnapshot { region, content });
    }

    /// The settings list as one painter, at the full unscrolled height `scroll_src_rect`
    /// crops against — the single body tile the row band deliberately does not keep. Built
    /// only when a settings screen is being left (see `snapshot_closing_modal`).
    ///
    /// `screen` is the one being *left*, not `self.nav.screen` — that has already moved on, and
    /// asking it for this geometry answers `None` (or the wrong scope's row count), which
    /// drops the row list out of the fade instead of freezing it.
    fn stitch_settings_body(
        &self,
        screen: Screen,
        tiles: &ui::cache::TileStore,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<Painter> {
        let (total, _, _, content) = self.scroll_geometry_for(screen, screen_w, screen_h, fonts)?;
        let stride = ui::widgets::focus_row_stride();
        let mut body = Painter::new(content.width().max(1), (total as u32 * stride).max(1));
        for i in 0..total {
            let Some(row) = tile::settings_row(i).and_then(|id| tiles.get(id)) else {
                continue;
            };
            body.draw_painter(0, i as i32 * stride as i32, row);
        }
        Some(body)
    }

    /// The version [`tile::MODAL`] is valid at — a hash of everything the open screen's
    /// chrome reads, plus the close-button hover every shell shares. `None` for a screen with
    /// no shell key of its own (`AddHost` and friends redraw on any `content_dirty` tick
    /// instead; see [`ModalShellKey`]).
    ///
    /// The key never leaves this function, which is what lets it borrow labels straight out of
    /// `App`: only the hash is kept, so a shell that is re-entered with different content
    /// differs by version rather than by a stored clone of every string it draws.
    fn modal_shell_version(&self, host_menu_actions: &[crate::app::state::hostmenu::HostAction]) -> Option<u64> {
        // The derived strings the key borrows — bound here so they outlive it, and built only
        // on the screen that actually reads each one.
        let host_menu_title = matches!(self.nav.screen, Screen::HostMenu)
            .then(|| self.host_menu_title())
            .unwrap_or_default();
        let host_menu_subtitle = matches!(self.nav.screen, Screen::HostMenu)
            .then(|| self.host_menu_subtitle())
            .unwrap_or_default();
        let wake_settings_title = matches!(self.nav.screen, Screen::WakeSettings)
            .then(|| view::wakesettings::title(&self.host_menu_title()))
            .unwrap_or_default();
        let speed_test_status = matches!(self.nav.screen, Screen::SpeedTest)
            .then(|| view::speedtest::status(self.screens.speed_test.as_ref(), &self.screens.speed_test_name))
            .unwrap_or_default();
        let key = match self.nav.screen {
            Screen::Settings(_) => Some(ModalShellKey::Settings {
                game: self.editing_game().map(|gs| gs.title.as_str()),
            }),
            Screen::Wake => self.screens.wake.as_ref().map(|w| ModalShellKey::Wake {
                name: &w.name,
                mac_empty: w.mac.is_empty(),
                sent: w.sent,
            }),
            Screen::Pairing => Some(ModalShellKey::Pairing {
                digits: self.screens.pin_digits,
                status: self.screens.pairing_status.as_deref(),
                busy: self.screens.pairing_busy,
            }),
            Screen::ForgetHost => Some(ModalShellKey::ForgetHost {
                name: self
                    .screens.host_menu_index
                    .and_then(|i| self.hosts.entries.get(i))
                    .map(HostEntry::name),
            }),
            Screen::HostMenu => Some(ModalShellKey::HostMenu {
                name: &host_menu_title,
                subtitle: &host_menu_subtitle,
                rows: host_menu_actions.len(),
            }),
            Screen::WakeSettings => Some(ModalShellKey::WakeSettings {
                title: &wake_settings_title,
                auto: self.wake_settings_host().is_some_and(|h| h.wol_auto),
            }),
            Screen::About => Some(ModalShellKey::About),
            // The whole shell is derived from the status sentence, which already encodes
            // the phase and the latest measurement.
            Screen::SpeedTest => Some(ModalShellKey::SpeedTest {
                status: &speed_test_status,
            }),
            Screen::Diagnostics => Some(ModalShellKey::Diagnostics {
                log_level: self.settings_ui.settings.log_level_override,
                stats_overlay: self.settings_ui.settings.stats_overlay,
                show_logs: self.settings_ui.settings.show_logs,
            }),
            Screen::Experimental => Some(ModalShellKey::Experimental {
                ndl_audio_offload: self.settings_ui.settings.ndl_audio_offload,
                game_mode: self.settings_ui.settings.game_mode,
                rooted: self.hosts.rooted,
            }),
            Screen::CursorSettings(_) => Some(ModalShellKey::CursorSettings {
                cursor_capture: self.settings_target().cursor_capture,
                cursor_gestures: self.settings_target().cursor_gestures,
                over: self.editing_override(),
            }),
            Screen::SendLogs => Some(ModalShellKey::SendLogs),
            // `EditHost` joins `AddHost` in having no shell key: its typed-digit
            // display has no separate focus tile to protect, so it just redraws on
            // any `content_dirty` tick — same for `PinLimit`, which is a fixed
            // message plus one always-focused button.
            Screen::Home | Screen::AddHost | Screen::EditHost | Screen::PinLimit => None,
        };
        // Hashed with the key rather than carried inside it: the close-button hover changes
        // every shell alike (see `ModalShellKey`).
        key.as_ref().map(|k| cache::version(&(self.render.hover_close, k)))
    }

    /// The version [`tile::MODAL_FOCUS`] is valid at — a hash of the open modal's focused
    /// widget *and its value*, so a value change invalidates the tile just as a focus move
    /// does. `None` for a screen with no single focused widget. Borrowed like
    /// [`modal_shell_version`](Self::modal_shell_version), and for the same reason.
    fn modal_focus_version(&self, host_menu_actions: &[crate::app::state::hostmenu::HostAction]) -> Option<u64> {
        // Borrowed by the key below, so it outlives it.
        let speed_test_label = matches!(self.nav.screen, Screen::SpeedTest)
            .then(|| view::speedtest::apply_label(view::speedtest::recommendation(self.screens.speed_test.as_ref())))
            .unwrap_or_default();
        let key = match self.nav.screen {
            Screen::Settings(_) => Some(ModalFocusKey::SettingsRow(
                self.nav.cursor(ScreenKey::Settings),
                *self.settings_target(),
                self.editing_override(),
                self.detected_gamepad_type,
            )),
            Screen::Wake => self
                .screens.wake
                .as_ref()
                .filter(|w| !w.mac.is_empty())
                .map(|w| ModalFocusKey::WakeButton(w.focused)),
            Screen::Pairing => Some(match self.screens.pairing_focus {
                PairingFocus::Pin => {
                    ModalFocusKey::PairingDigit(self.screens.pin_digit_index, self.screens.pin_digits[self.screens.pin_digit_index])
                }
                PairingFocus::RequestAccess => ModalFocusKey::PairingButton,
            }),
            Screen::ForgetHost => Some(ModalFocusKey::ForgetButton(self.nav.cursor(ScreenKey::ForgetHost))),
            Screen::HostMenu => host_menu_actions
                .get(self.nav.cursor(ScreenKey::HostMenu))
                .map(|&action| {
                    ModalFocusKey::MenuRow(
                        self.nav.cursor(ScreenKey::HostMenu),
                        action,
                        self.host_menu_paired(),
                        self.screens.host_menu_dots,
                    )
                }),
            Screen::WakeSettings => Some(ModalFocusKey::WakeToggle(
                self.wake_settings_host().is_some_and(|h| h.wol_auto),
            )),
            // Only once there are buttons to focus — while measuring there is nothing
            // on the card but text.
            Screen::SpeedTest => view::speedtest::finished(self.screens.speed_test.as_ref())
                .then(|| ModalFocusKey::SpeedTestButton(self.nav.cursor(ScreenKey::SpeedTest), &speed_test_label)),
            Screen::Diagnostics => Some(ModalFocusKey::DiagnosticsRow(
                self.nav.cursor(ScreenKey::Diagnostics),
                self.settings_ui.settings.log_level_override,
                self.settings_ui.settings.stats_overlay,
                self.settings_ui.settings.show_logs,
            )),
            Screen::Experimental => Some(ModalFocusKey::ExperimentalRow(
                self.nav.cursor(ScreenKey::Experimental),
                self.settings_ui.settings.ndl_audio_offload,
                self.settings_ui.settings.game_mode,
                self.hosts.rooted,
            )),
            Screen::CursorSettings(_) => Some(ModalFocusKey::CursorSettingsRow(
                self.nav.cursor(ScreenKey::CursorSettings),
                self.settings_target().cursor_capture,
                self.settings_target().cursor_gestures,
                self.editing_override(),
            )),
            Screen::SendLogs => Some(ModalFocusKey::SendLogsButton(self.nav.cursor(ScreenKey::SendLogs))),
            // Neither has a single focused widget: the address form is one always-active
            // field, About is a scrolling document, and `PinLimit`'s one button is
            // always drawn focused directly in `render_pin_limit`.
            Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => None,
        };
        key.as_ref().map(cache::version)
    }

    /// Modal family: the open modal's full-screen shell tile (rebuilt only on content
    /// change, keyed by `ModalShellKey`) and its single focused-widget tile (keyed by
    /// `ModalFocusKey`). Extracted from `prepare_tiles` (A2 staging).
    fn prepare_modal(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        self.snapshot_closing_modal(ctx);
        let (content_dirty, screen_changed) = (ctx.content_dirty, ctx.screen_changed);
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            screen: size,
            updated,
            settings_rows,
            ..
        } = ctx;
        let (screen_w, screen_h) = (size.w, size.h);
        let modal_open = !matches!(self.nav.screen, Screen::Home);
        // Every modal's shell only reacts to *content* changes — not to
        // `content_dirty`, which is also `true` on plain focus movement (see
        // `ModalShellKey`'s docs). `AddHost` has no `ModalShellKey` variant
        // (no split focus tile to protect) and just redraws on any
        // `content_dirty` tick, same as every modal did before this split.
        // Built once for both version fns rather than per call.
        let host_menu_actions = matches!(self.nav.screen, Screen::HostMenu)
            .then(|| self.host_menu_actions())
            .unwrap_or_default();
        let shell_version = self.modal_shell_version(&host_menu_actions);
        let modal_stale = match shell_version {
            Some(_) => !tiles.contains(tile::MODAL) || self.render.modal.shell_version != shell_version,
            None => content_dirty || !tiles.contains(tile::MODAL),
        };
        self.render.modal.shell_version = shell_version;
        if modal_open && (screen_changed || modal_stale) {
            // Sized to the card's bounding box, not the whole screen: the render fns
            // below draw at absolute, screen-centered coordinates, and the painter's
            // origin shift (see `Painter::set_origin`) maps that geometry into the
            // smaller buffer.
            // Taken, not left to be dropped when the rebuilt tile replaces it: the modal is
            // re-rasterized at the same size every time, so its pixmap is reusable.
            let recycled = tiles.take(tile::MODAL);
            let mut p = self.modal_painter(recycled, screen_w, screen_h, fonts);
            let c = &mut ui::Canvas::new(&mut p, text_cache, fonts, screen_w, screen_h);
            let hover_close = self.render.hover_close;
            self.with_modal_screen(|s| s.render(c, hover_close)).transpose()?;
            // Staleness was already decided above, against `modal.shell_version` rather than
            // against the store — the keyless screens have no version to compare, so they turn
            // on `content_dirty` instead. Hence `STATIC` here: the store is told to keep this
            // until something removes it, not to arbitrate.
            tiles.put(tile::MODAL, cache::STATIC, p);
            updated.push(tile::MODAL);
        }
        // Whichever modal is open has at most one focused, zoom-animated widget
        // (`ModalFocusKey`'s docs) — `None` for screens with no such widget
        // (Home, AddHost) or when Wake has nothing to focus (no MAC on record,
        // see `handle_wake_event`'s matching guard).
        if let Some(version) = self.modal_focus_version(&host_menu_actions) {
            // Also stale on every tick of an in-flight `switch_anim`: the knob's
            // position depends on elapsed time, not on the key, which doesn't
            // change mid-flip.
            let stale = self.render.modal.switch_anim.is_some() || !tiles.is_fresh(tile::MODAL_FOCUS, version);
            if stale {
                // `None` where the screen turns out to have no focused widget after all —
                // the descriptor that proves the arm reachable is the same value it draws
                // from, so an arm cannot assert its way past a `None` any more.
                let tile = match self.nav.screen {
                    Screen::Settings(_) => {
                        let (_, content) = view::settings::layout(self.settings_scope(), screen_w, screen_h);
                        let rows = settings_rows.get_or_insert_with(|| self.settings_rows());
                        let dropdown_open = self
                            .settings_ui.dropdown
                            .as_ref()
                            .is_some_and(|dd| dd.row == self.nav.cursor(ScreenKey::Settings));
                        let target_on = rows
                            .get(self.nav.cursor(ScreenKey::Settings))
                            .is_some_and(|r| r.value == "On");
                        Some(ui::rasterize(
                            ui::widgets::FocusRowTile {
                                rows,
                                content_width: content.width(),
                                index: self.nav.cursor(ScreenKey::Settings),
                                dropdown_open,
                                switch_frac: self.toggle_frac(target_on, self.nav.cursor(ScreenKey::Settings)),
                            },
                            text_cache,
                            fonts,
                        )?)
                    }
                    // Every two-button confirm dialog shares the button geometry (one subtitle
                    // sizes the card, so one button row falls out of it) and describes its own
                    // labels — one value, not a match arm per screen.
                    Screen::Wake | Screen::ForgetHost | Screen::SendLogs | Screen::SpeedTest => {
                        match (self.confirm_of(), self.confirm_focused()) {
                            (Some(confirm), Some(i)) => {
                                let rect =
                                    Self::confirm_focus_button_rect(screen_w, screen_h, fonts, &confirm.subtitle, i);
                                Some(ui::rasterize(
                                    ui::widgets::ConfirmButtonTile {
                                        button: &confirm.widgets()[i],
                                        w: rect.width(),
                                        h: rect.height(),
                                    },
                                    text_cache,
                                    fonts,
                                )?)
                            }
                            _ => None,
                        }
                    }
                    Screen::Pairing => Some(match self.screens.pairing_focus {
                        PairingFocus::Pin => {
                            let digit = self.screens.pin_digits[self.screens.pin_digit_index].to_string();
                            ui::rasterize(
                                ui::widgets::CardTextTile {
                                    font: fonts.title,
                                    text: &digit,
                                    w: view::pairing::DIGIT_W,
                                    h: view::pairing::DIGIT_H,
                                },
                                text_cache,
                                fonts,
                            )?
                        }
                        PairingFocus::RequestAccess => {
                            let card = view::pairing::card_rect(screen_w, screen_h, fonts);
                            let btn = view::pairing::request_button_rect(card, fonts);
                            ui::rasterize(
                                view::pairing::RequestButtonTile {
                                    w: btn.width(),
                                    h: btn.height(),
                                },
                                text_cache,
                                fonts,
                            )?
                        }
                    }),
                    // Every plain list modal: same tile, same geometry, built from whichever
                    // rows the screen lists. Only Diagnostics can have a dropdown open, and
                    // only the host menu has a ⋯ to light.
                    Screen::HostMenu
                    | Screen::WakeSettings
                    | Screen::Diagnostics
                    | Screen::Experimental
                    | Screen::CursorSettings(_) => {
                        let rows = self.list_focus_rows().unwrap_or_default();
                        let content = self.modal_list_content(screen_w, screen_h, fonts);
                        self.list_modal_focused()
                            .map(|focused| {
                                let dropdown_open = self.settings_ui.dropdown.as_ref().is_some_and(|dd| dd.row == focused);
                                let target_on = rows.get(focused).is_some_and(|r| r.value == "On");
                                ui::rasterize(
                                    ui::widgets::FocusRowTile {
                                        rows: &rows,
                                        content_width: content.width(),
                                        index: focused,
                                        dropdown_open,
                                        switch_frac: self.toggle_frac(target_on, focused),
                                    },
                                    text_cache,
                                    fonts,
                                )
                            })
                            .transpose()?
                    }
                    // No single focused widget to draw — `modal_focus_version` is `None` on
                    // these, so this is the arm that never runs rather than one that panics.
                    Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => None,
                };
                if let Some(tile) = tile {
                    tiles.put(tile::MODAL_FOCUS, version, tile);
                    updated.push(tile::MODAL_FOCUS);
                }
            }
        } else {
            tiles.remove(tile::MODAL_FOCUS);
        }
        Ok(())
    }

    /// Dropdown family: the overlay panel + focused-option tile for an open Settings/Diagnostics dropdown; cleared when closed (unless a close-fade still needs them). Extracted from `prepare_tiles`.
    fn prepare_dropdown(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            screen: size,
            updated,
            ..
        } = ctx;
        let (screen_w, screen_h) = (size.w, size.h);
        if let Some(dd) = &self.settings_ui.dropdown {
            let options = self.dropdown_options(dd.row);
            // The overlay hangs inside whichever viewport its list is drawn in.
            let content_w = match self.nav.screen {
                Screen::Diagnostics => self.modal_list_content(screen_w, screen_h, fonts).width(),
                _ => view::settings::layout(self.settings_scope(), screen_w, screen_h)
                    .1
                    .width(),
            };

            // Keyed by screen as well as row: row 0 means a different setting on Settings
            // than it does on Diagnostics.
            let overlay = cache::version(&(self.nav.screen, dd.row));
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

            let focused = cache::version(&(self.nav.screen, dd.row, dd.focused));
            if tiles.ensure(tile::DROPDOWN_FOCUS, focused, || {
                let option = options.get(dd.focused).map_or("", String::as_str);
                ui::rasterize(
                    ui::widgets::DropdownOptionTile {
                        option,
                        width: content_w,
                    },
                    text_cache,
                    fonts,
                )
            })? {
                updated.push(tile::DROPDOWN_FOCUS);
            }
        } else if self.settings_ui.dropdown_fade.closing_frame(DROPDOWN_FADE).is_none() {
            // Keep the tiles cached while a close-fade is in flight — `draw_list`
            // still composites them at falling alpha.
            tiles.remove(tile::DROPDOWN_OVERLAY);
            tiles.remove(tile::DROPDOWN_FOCUS);
        }
        Ok(())
    }

    /// Releases settings-row tiles from `first` on: the tail of a list that just got
    /// shorter, or the whole band once the settings screens are left.
    fn evict_settings_rows_from(&mut self, first: usize, tiles: &mut ui::cache::TileStore) {
        for i in first..tile::SETTINGS_ROW_SLOTS {
            let Some(id) = tile::settings_row(i) else { break };
            if tiles.remove(id) {
                self.render.evicted_tiles.push(id);
            }
        }
    }

    /// Scroll family: the indicator, edge-fade ramps, and windowed content tile for whichever modal overflows (Settings rows / About document). Extracted from `prepare_tiles`.
    fn prepare_scroll(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            screen: size,
            updated,
            settings_rows,
            ..
        } = ctx;
        let (screen_w, screen_h) = (size.w, size.h);
        // The settings-row band belongs to the settings screens alone; leaving them releases
        // it rather than holding a list's worth of textures behind whatever is on screen now.
        if !matches!(self.nav.screen, Screen::Settings(_)) {
            self.evict_settings_rows_from(0, tiles);
        }
        // Whichever modal's content overflows its viewport (Settings' rows, About's
        // document) gets its scroll indicator and content tile refreshed here — see
        // `scroll_geometry`'s docs for why this one block covers every such modal
        // instead of being hand-copied per screen.
        if matches!(self.nav.screen, Screen::About) {
            // Mutates `about_wrapped` only — must happen before `scroll_geometry`
            // (a `&self` read) can report a non-zero total for this frame.
            let card = view::about::card_rect(screen_w, screen_h);
            let body = view::about::body_rect(card, fonts);
            self.ensure_about_wrapped(fonts, body.width());
        }
        if let Some((total, visible, _, content)) = self.scroll_geometry(screen_w, screen_h, fonts) {
            let scroll = self.render.scroll.clamped(total, visible);
            let ind = cache::version(&(self.nav.screen, total, visible, scroll, content.height()));
            if tiles.ensure(tile::SCROLL_INDICATOR, ind, || {
                ui::rasterize(
                    ui::widgets::ListScrollbarTile {
                        w: SCROLL_INDICATOR_TILE_W,
                        h: content.height(),
                        total,
                        visible,
                        scroll,
                    },
                    text_cache,
                    fonts,
                )
            })? {
                updated.push(tile::SCROLL_INDICATOR);
            }
            // Static ramps, so these are once-per-run bakes rather than keyed rebuilds —
            // scrolling and resizing both leave them valid (the GPU restretches them).
            if tiles.ensure_static(tile::SCROLL_FADE, || {
                ui::rasterize(
                    ui::widgets::ScrollFadeTile {
                        edge: ui::widgets::FadeEdge::Bottom,
                    },
                    text_cache,
                    fonts,
                )
            })? {
                updated.push(tile::SCROLL_FADE);
            }
            if tiles.ensure_static(tile::SCROLL_FADE_TOP, || {
                ui::rasterize(
                    ui::widgets::ScrollFadeTile {
                        edge: ui::widgets::FadeEdge::Top,
                    },
                    text_cache,
                    fonts,
                )
            })? {
                updated.push(tile::SCROLL_FADE_TOP);
            }
            let stride = self.scroll_stride(fonts);
            self.sync_modal_scroll(self.nav.screen, total, visible, content.height(), stride);

            match self.nav.screen {
                Screen::Settings(_) => {
                    let dropdown_row = self.settings_ui.dropdown.as_ref().map(|dd| dd.row);
                    let row_count = menu::settings_row_count(self.settings_scope());
                    // What the whole list is derived from (see `App::settings_rows`). Checked
                    // before the list is built at all: the per-row keys below still arbitrate
                    // which row rebuilds, but on a pure animation frame — the common case
                    // while Settings is open — this comparison is the entire cost.
                    let rows_version = cache::version(&(
                        self.nav.screen,
                        *self.settings_target(),
                        self.editing_override(),
                        self.detected_gamepad_type,
                        // The Controller row's caption turns on whether the pad is actually
                        // bound to hid-playstation, which a hotplug can change on its own.
                        crate::platform::webos::dualsense::hid_playstation_bound(),
                        // The focused row carries the override-clear hint (`decorate_override`).
                        self.nav.cursor(ScreenKey::Settings),
                        dropdown_row,
                        content.width(),
                    ));
                    let cached = self.render.modal.settings_rows_version == Some(rows_version)
                        && (0..row_count).all(|i| tile::settings_row(i).is_some_and(|id| tiles.contains(id)));
                    if !cached {
                        let rows = settings_rows.get_or_insert_with(|| self.settings_rows());
                        // One tile per row, each keyed on that row's own content. Rebuilding the
                        // whole list as one strip cost 25-60ms on armv7 every time a single value
                        // moved; this pays for the row that actually changed and reads the rest
                        // straight out of the cache.
                        for (i, row) in rows.iter().enumerate() {
                            let Some(id) = tile::settings_row(i) else { break };
                            let key = cache::version(&(self.nav.screen, i, row.key(), dropdown_row == Some(i)));
                            if tiles.is_fresh(id, key) {
                                continue;
                            }
                            let tile = ui::rasterize(
                                ui::widgets::RowTile {
                                    row,
                                    width: content.width(),
                                    dropdown_open: dropdown_row == Some(i),
                                },
                                text_cache,
                                fonts,
                            )?;
                            tiles.put(id, key, tile);
                            updated.push(id);
                        }
                        // Slots past the end of a list that just got shorter (a sub-page is a
                        // shorter list on the same screen) would otherwise keep drawing.
                        self.evict_settings_rows_from(rows.len(), tiles);
                        self.render.modal.settings_rows_version = Some(rows_version);
                    }
                    // Every row is baked, so the window is the whole list — the crop
                    // rebase in `scroll_src_rect` has nothing to shift.
                    self.render.content_window = ui::scroll::ContentWindow {
                        start: 0,
                        len: row_count,
                    };
                }
                Screen::About => {
                    if let Some(new_start) = self.render.content_window.recenter_if_needed(
                        scroll,
                        visible,
                        total,
                        ABOUT_WINDOW_BUDGET,
                        ABOUT_WINDOW_MARGIN,
                    ) {
                        let len = ABOUT_WINDOW_BUDGET.min(total.saturating_sub(new_start));
                        if let Some((_, wrapped)) = &self.render.about_wrapped {
                            let stride = self.scroll_stride(fonts) as u32;
                            let mut p = Painter::new(content.width().max(1), (len as u32 * stride).max(1));
                            let mut c = ui::Canvas::tile(&mut p, text_cache, fonts);
                            view::about::draw_window(&mut c, fonts.value, wrapped, new_start, len)?;
                            self.render.content_window = ui::scroll::ContentWindow { start: new_start, len };
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
            menu::SettingsScope::Global,
            None,
            self.render.hover_close,
        )?;
        let (_, content) = view::settings::layout(menu::SettingsScope::Global, screen_w, screen_h);
        let rows = self.settings_rows();
        // One row is enough to warm the glyph cache the whole list draws from — the rest are
        // the same fonts at the same sizes.
        if let Some(row) = rows.first() {
            let _ = ui::rasterize(
                ui::widgets::RowTile {
                    row,
                    width: content.width(),
                    dropdown_open: false,
                },
                text_cache,
                fonts,
            )?;
        }
        let _ = ui::rasterize(
            ui::widgets::FocusRowTile {
                rows: &rows,
                content_width: content.width(),
                index: 0,
                dropdown_open: false,
                switch_frac: 0.0,
            },
            text_cache,
            fonts,
        )?;
        Ok(())
    }

    /// Rasterizes every stale tile (tiny-skia, CPU — the only place rasterization
    /// happens) and returns which tiles need their GPU texture re-uploaded.
    /// `content_dirty` is the main loop's "an event/drain changed something this
    /// tick" flag — it forces the open modal's tile to re-rasterize, since modal
    /// content has no finer dirty tracking of its own. Pure animation frames pass
    /// `false` and rasterize nothing at all. Call `advance_frame` first.
    pub fn prepare_tiles(&mut self, ctx: &mut RenderCtx<'_>) -> Result<Vec<TileId>> {
        // The transition frame's cost is what decides whether the open fade is visible.
        let started = ctx.screen_changed.then(Instant::now);
        let screen_w = ctx.screen.w;

        self.prepare_sidebar(ctx)?;
        self.prepare_grid(ctx)?;
        self.prepare_hero(ctx);

        // Status line block — built whenever `home_status` is set, independent of
        // whether a host is selected (the "Send logs" result shows here too).
        match &self.home_status {
            Some(s) => {
                if ctx.tiles.ensure(tile::STATUS, cache::version(s), || {
                    let avail = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
                    let max_w = avail.saturating_sub(2 * view::home::GRID_PAD as u32);
                    ui::rasterize(
                        ui::tiles::WrappedTextTile {
                            font: ctx.fonts.label,
                            text: s,
                            max_w,
                            color: ui::style::theme().muted,
                            line_gap: 6,
                        },
                        ctx.text,
                        ctx.fonts,
                    )
                })? {
                    ctx.updated.push(tile::STATUS);
                }
            }
            None => {
                ctx.tiles.remove(tile::STATUS);
            }
        }

        self.prepare_modal(ctx)?;
        self.prepare_dropdown(ctx)?;
        self.prepare_scroll(ctx)?;

        // Entering a modal rasterizes it — shell, rows, and (Settings/About) the full
        // content strip: tens of ms on this SoC, more than the fade itself. `advance_frame`
        // started the clocks before all that, so re-stamp them here and the fade is
        // measured from the first frame that can show it.
        if let Some(started) = started {
            tracing::debug!(
                "entered {:?}: {} tiles rasterized in {:?}",
                self.nav.screen,
                ctx.updated.len(),
                started.elapsed()
            );
            self.render.modal.fade.restart();
        }
        Ok(std::mem::take(&mut ctx.updated))
    }
}
