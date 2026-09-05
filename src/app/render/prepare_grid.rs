//! The grid family's rasterization: which card tiles exist this frame, and what is in them.
//!
//! Split out of `prepare.rs` because it is the one pass whose cost scales with what is on
//! screen. Every step here is windowed — an index range, never a walk of the library — and each
//! is its own method so that stays checkable: release, evict, build, the focused card's own
//! tiles, the shared card-sized tiles, and the reveal. The windowing arithmetic itself lives in
//! `prepare_grid` alone.
use std::time::Instant;

use anyhow::Result;

use crate::app::grid::{GridLayout, CARD_BUILD_BUDGET, CARD_BUILD_BURST, CARD_KEEP_ROWS, CARD_PREFETCH_ROWS};
use crate::app::library::Library;
use crate::app::render::ctx::RenderCtx;
use crate::app::render::tile;
use crate::app::spinner::PageReady;
use crate::app::state::cardmenu::CardMenuRow;
use crate::app::{view, App, HomeFocus, Screen};
use crate::ui;
use crate::ui::cache;

/// Nothing more can arrive for this card (cover in library.art or game has none).
/// Free function so build pass and reveal check can use it while &mut self is live elsewhere.
fn art_ready(library: &Library, layout: GridLayout, idx: usize) -> bool {
    layout.card_at(&library.games, idx).is_none_or(|game| {
        library.art.contains_key(&game.id) || (game.art.portrait.is_none() && game.art.header.is_none())
    })
}

impl App {
    /// Whether the windowed card pass can be skipped entirely this frame.
    fn grid_window_frozen(&self) -> bool {
        !matches!(self.nav.screen, Screen::Home)
            && self.render.grid.reveal.is_revealed()
            && !self.render.grid.dirty
            && self.render.grid.cards_dirty.is_empty()
    }

    /// Grid rasterization. Everything below is O(visible) via index ranges, not library scans.
    pub(super) fn prepare_grid(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let (screen_w, screen_h) = (ctx.screen.w, ctx.screen.h);
        // Grid geometry from width (same three numbers as advance_frame).
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        let (card_w, card_h) = view::home::grid_card_size(available_w, columns);
        // Reset before branch (set only inside, stale true = full-rate render loop).
        self.render.grid.tiles_pending = false;
        if self.library.selected_host.is_none() {
            return self.prepare_no_host_tile(ctx);
        }
        // Modal open: grid neither scrolls/focuses, so skip windowed pass unless invalidated.
        if self.grid_window_frozen() {
            return Ok(());
        }
        let count = self.grid_len(columns);
        self.release_stale_cards(ctx.tiles);

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
        let build_window = window(
            first_visible_row - CARD_PREFETCH_ROWS,
            first_visible_row + visible_rows + CARD_PREFETCH_ROWS,
        );
        // What the reveal waits for and what its wave sweeps: the cards actually on screen,
        // without the prefetch rows above and below them.
        let page_window = window(first_visible_row, first_visible_row + visible_rows);
        let keep_window = window(
            first_visible_row - CARD_KEEP_ROWS,
            first_visible_row + visible_rows + CARD_KEEP_ROWS,
        );

        // Layout by value (maps indices without borrowing self). Rebuilt per helper.
        self.evict_cards_outside(keep_window, columns, ctx.tiles);
        let pending = self.build_card_window(build_window, columns, card_w, card_h, ctx)?;
        self.prepare_focused_card_tiles(columns, card_w, card_h, pending, ctx)?;
        self.prepare_grid_shared_tiles(card_w, card_h, ctx)?;
        self.advance_grid_reveal(page_window, columns, ctx);
        Ok(())
    }

    /// Drop stale card tiles (whole set on library/host change, else just invalidated ones).
    fn release_stale_cards(&mut self, tiles: &mut ui::cache::TileStore) {
        if self.render.grid.dirty {
            // Fresh library: drop all textures (stale), re-arm spinner (avoid stranding tail).
            for id in self.render.grid.card_ids.release_all() {
                tiles.remove(id);
                self.render.evicted_tiles.push(id);
            }
            self.render.grid.card_pop_until = None;
            self.render.grid.dirty = false;
            self.render.grid.cards_dirty.clear();
            self.render.grid.reveal.restart();
        } else {
            // Texture stale only; slot/arrival stays. Release→build would re-pop on reveal.
            for id in std::mem::take(&mut self.render.grid.cards_dirty) {
                if let Some(t) = self.render.grid.card_ids.get(&id) {
                    tiles.remove(t);
                }
            }
        }
    }

    /// Free cards outside keep window (+ covers). Evict first, before new ones built.
    fn evict_cards_outside(
        &mut self,
        keep_window: std::ops::Range<usize>,
        columns: usize,
        tiles: &mut ui::cache::TileStore,
    ) {
        // Off resident set (windowed), not whole library (no scaling with library size).
        // By tile id: sorted vector + binary search vs HashSet re-hash every frame.
        let mut keep = std::mem::take(&mut self.render.grid.scratch.keep);
        keep.clear();
        let layout = self.library.layout(columns);
        keep.extend(
            keep_window
                .filter_map(|idx| layout.pin_id_at(&self.library.games, idx))
                .filter_map(|id| self.render.grid.card_ids.get(id)),
        );
        keep.sort_unstable();
        let mut dropped = std::mem::take(&mut self.render.grid.scratch.dropped);
        dropped.clear();
        dropped.extend(
            self.render
                .grid
                .card_ids
                .entries()
                .filter(|(_, t)| keep.binary_search(t).is_err())
                .map(|(id, _)| id.to_string()),
        );
        self.render.grid.scratch.keep = keep;
        for id in dropped.drain(..) {
            if let Some(t) = self.render.grid.card_ids.release(&id) {
                // Pixmap recycled for cards built this frame (avoids realloc on scroll).
                if let Some(painter) = tiles.take(t) {
                    self.render.grid.free_cards.push(painter);
                }
                self.render.evicted_tiles.push(t);
            }
            // Drop decoded cover (several×tile size). Re-request from disk cache on scroll back.
            self.library.art.remove(&id);
            if let Some(loader) = &mut self.jobs.art {
                loader.forget(&id);
            }
        }
        self.render.grid.scratch.dropped = dropped;
    }

    /// Rasterize cards in build window (art-ready first, on time budget).
    /// Returns true if budget ran out (caller defers settled-grid work).
    fn build_card_window(
        &mut self,
        build_window: std::ops::Range<usize>,
        columns: usize,
        card_w: u32,
        card_h: u32,
        ctx: &mut RenderCtx<'_>,
    ) -> Result<bool> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            updated,
            ..
        } = ctx;
        // Art-ready first (avoid re-dirty when cover lands). Two lists, not sorted (indices).

        let mut ready = std::mem::take(&mut self.render.grid.scratch.ready);
        let mut waiting = std::mem::take(&mut self.render.grid.scratch.waiting);
        ready.clear();
        waiting.clear();
        let layout = self.library.layout(columns);
        for idx in build_window {
            // Nothing in padding after partial pinned row.
            let Some(id) = layout.pin_id_at(&self.library.games, idx) else {
                continue;
            };
            // Request cover as it enters window (not whole library at once).
            if let (Some(loader), Some(game)) = (&mut self.jobs.art, layout.card_at(&self.library.games, idx)) {
                loader.request(game);
            }
            if self.render.grid.card_ids.get(id).is_some_and(|t| tiles.contains(t)) {
                continue;
            }
            if art_ready(&self.library, layout, idx) {
                ready.push(idx);
            } else {
                waiting.push(idx);
            }
        }

        let mut pending = false;
        let budget_from = Instant::now();
        let mut built = 0usize;
        let icon_side = ui::widgets::icon_side(card_w, card_h);
        for idx in ready.iter().copied().chain(waiting.iter().copied()) {
            // Budget counted on rasterized cards, not candidates (padding costs nothing).
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
                let game = self.grid_card_entry(idx, columns);
                let art = self.library.art.get(&game.id);
                let icon = game
                    .icon
                    .as_deref()
                    .and_then(|token| crate::app::assets::card_icon(token, icon_side));
                ui::rasterize_into(
                    ui::tiles::CardTile {
                        w: card_w,
                        h: card_h,
                        title: &game.title,
                        art,
                        icon: icon.as_deref(),
                    },
                    recycled,
                    text_cache,
                    fonts,
                )?
            };
            // Existing slot: texture/arrival untouched (art refresh). New: gets slot here.
            let (tile_id, is_new) = self.render.grid.card_ids.id_new(&id);
            tiles.put(tile_id, cache::static_version(), tile);
            // New card on settled grid: one arrival. Art refresh: no arrival (swap in place).
            if is_new && self.render.grid.reveal.is_revealed() {
                self.render.grid.arm_card_pop(&id, Instant::now());
            }
            updated.push(tile_id);
        }
        // Anything left in free_cards was evicted with no replacement (surplus pixmaps).
        self.render.grid.free_cards.clear();
        self.render.grid.scratch.ready = ready;
        self.render.grid.scratch.waiting = waiting;
        self.render.grid.tiles_pending = pending;
        Ok(pending)
    }

    /// Focused card tiles: hero prefetch, title strip, submenu panel.
    fn prepare_focused_card_tiles(
        &mut self,
        columns: usize,
        card_w: u32,
        card_h: u32,
        pending: bool,
        ctx: &mut RenderCtx<'_>,
    ) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            updated,
            ..
        } = ctx;
        // Prefetch focused card's hero (ready on OK press). Only when window settled.
        if self.render.grid.reveal.is_revealed() && !pending {
            if let HomeFocus::Grid(focus_idx) = self.home_focus {
                if let Some(game) = self.library.layout(columns).card_at(&self.library.games, focus_idx) {
                    if let Some(loader) = &mut self.jobs.art {
                        loader.request_hero(game);
                    }
                    self.render.hero.want(&game.id);
                }
            }
        }

        // Focused card title strip (own tile for moving wipe, not re-raster per frame).
        if let HomeFocus::Grid(idx) = self.home_focus {
            if let Some(pin_id) = self.library.pin_id_at(idx, columns) {
                let title = self.grid_card_entry(idx, columns).title.as_str();
                // Keyed by card identity like the card tiles themselves (`CardIds`),
                // not by title — two games can share one. The dot says the title is bound
                // to a settings profile (plan D5).
                let overridden = self.game_is_bound(pin_id);
                let version = cache::version(&(pin_id, card_w, card_h, overridden));
                if tiles.ensure(tile::CARD_TITLE, version, || {
                    ui::rasterize(
                        ui::tiles::CardTitleTile {
                            card_w,
                            card_h,
                            title,
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
                // Still built when the card takes focus rather than when the menu opens.
                // It is only a tint now, but the rows and title tiles are keyed with it
                // and they are not free, and having the whole set ready before the rise
                // starts is what keeps the panel from appearing to wait for the button
                // to come back up.
                let menu_open = self.card_menu.as_ref().is_some_and(|m| m.pin_id == pin_id);
                if menu_open || (self.render.grid.reveal.is_revealed() && !pending) {
                    let kinds = self.card_menu_row_kinds(pin_id);
                    let rows = self.card_menu_rows(pin_id);
                    // No focused row in this key: the glass and the title are composited
                    // under the selection, so moving between the menu's rows rebuilds
                    // neither. The two row tiles are the exception — the focused row moved
                    // out of the list and into the band — and get their own keys below.
                    let version = cache::version(&(pin_id, card_w, card_h, &rows, overridden));
                    let focused = self.card_menu.as_ref().map_or(0, |m| m.focused);
                    let rows_version = cache::version(&(pin_id, card_w, card_h, &rows, overridden, focused));
                    // The dot follows what owns it: the title while the strip is collapsed,
                    // the Settings row once the panel is up.
                    let marked = overridden
                        .then(|| kinds.iter().position(|k| *k == CardMenuRow::Settings))
                        .flatten();
                    if tiles.ensure(tile::CARD_MENU, version, || {
                        ui::rasterize(
                            ui::tiles::CardMenuTile {
                                card_w,
                                card_h,
                                rows: &rows,
                            },
                            text_cache,
                            fonts,
                        )
                    })? {
                        updated.push(tile::CARD_MENU);
                    }
                    if tiles.ensure(tile::CARD_MENU_ROWS, rows_version, || {
                        ui::rasterize(
                            ui::tiles::CardMenuRowsTile {
                                card_w,
                                card_h,
                                rows: &rows,
                                marked,
                                focused,
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
                    // The focused row, content and all — so keyed by that row, not by width
                    // alone as it was while it held nothing but a flat fill. Built only with
                    // the menu actually up, unlike its three siblings: nothing composites it
                    // before the panel rises, and prefetching it for every focused card would
                    // raster an icon and a label on each step across the grid.
                    let band = menu_open.then(|| rows.get(focused).copied()).flatten();
                    if let Some(row) = band {
                        let band_marked = marked == Some(focused);
                        if tiles.ensure(
                            tile::CARD_MENU_BAND,
                            cache::version(&(card_w, row, band_marked)),
                            || {
                                ui::rasterize(
                                    ui::tiles::CardMenuBandTile {
                                        card_w,
                                        row,
                                        marked: band_marked,
                                    },
                                    text_cache,
                                    fonts,
                                )
                            },
                        )? {
                            updated.push(tile::CARD_MENU_BAND);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The tiles every card shares: the section headings, the pin badge, and the ring, shadow
    /// and outline at the current card size.
    fn prepare_grid_shared_tiles(&mut self, card_w: u32, card_h: u32, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            updated,
            ..
        } = ctx;
        // The grid's section headings, one slot per drawn section. Versioned by the label
        // rather than static: the names are the user's collections now, and a rename must
        // re-raster exactly the one slot that carries it.
        for (i, group) in self.library.groups.iter().enumerate() {
            let Some(id) = tile::section(i) else { break };
            let label = group.name.as_str();
            if tiles.ensure(id, cache::version(&label), || {
                ui::rasterize(
                    ui::tiles::TextTile {
                        font: fonts.title,
                        text: label,
                        color: ui::theme::palette().muted,
                    },
                    text_cache,
                    fonts,
                )
            })? {
                updated.push(id);
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
        Ok(())
    }

    /// Advances the loading spinner, and sweeps the whole page in on one wave the frame it is
    /// finally complete.
    fn advance_grid_reveal(&mut self, page_window: std::ops::Range<usize>, columns: usize, ctx: &mut RenderCtx<'_>) {
        let RenderCtx { tiles, updated, .. } = ctx;
        let layout = self.library.layout(columns);
        if self.render.grid.reveal.is_revealed() {
            return;
        }
        // Rechecks the whole page rather than trusting `!pending`, since a card built earlier
        // can still be waiting behind a re-dirtied sibling. Tiles and art are answered
        // separately: art that never arrives is what the spinner's cap exists for, where a
        // card with no tile yet is simply not ready to be revealed (see `PageReady`).
        let page_ready = || {
            let mut art_pending = false;
            for idx in page_window.clone() {
                let Some(id) = layout.pin_id_at(&self.library.games, idx) else {
                    continue;
                };
                if !self.render.grid.card_ids.get(id).is_some_and(|t| tiles.contains(t)) {
                    return PageReady::Building;
                }
                art_pending |= !art_ready(&self.library, layout, idx);
            }
            if art_pending {
                PageReady::Tiles
            } else {
                PageReady::All
            }
        };
        // Everything built behind the spinner becomes visible in this one frame: `reveal()`
        // (inside `advance`) starts the dissolve that uncovers it — one mask, not an
        // entrance per card (see `spinner::GridReveal::dissolve_mask`).
        if let Some(idx) = self
            .render
            .grid
            .reveal
            .advance(self.library_fetch_in_flight() || self.wake_wait_in_flight(), page_ready)
        {
            updated.push(tile::spinner(idx));
        }
    }

    /// The empty-state line, in place of a grid.
    fn prepare_no_host_tile(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let RenderCtx {
            tiles,
            text: text_cache,
            fonts,
            updated,
            ..
        } = ctx;
        self.render.grid.reveal.reveal();
        if tiles.ensure_static(tile::NO_HOST, || {
            ui::rasterize(
                ui::tiles::TextTile {
                    font: fonts.label,
                    text: "No host selected — pick one from the list, or add one.",
                    color: ui::theme::palette().muted,
                },
                text_cache,
                fonts,
            )
        })? {
            updated.push(tile::NO_HOST);
        }
        Ok(())
    }
}
