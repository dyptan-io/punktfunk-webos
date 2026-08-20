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
use crate::app::{view, App, HomeFocus, Screen};
use crate::ui;
use crate::ui::cache;

/// Whether nothing more can arrive for this card: the cover is already in `library.art`, or the
/// game never had one to fetch (no `library.art` entry either way). "Desktop" and the padding
/// after a partial pinned row have no `games` entry and are always ready.
///
/// A free function rather than a closure so both the build pass and the reveal check can use it
/// while `&mut self` is live elsewhere in the same frame.
fn art_ready(library: &Library, layout: &GridLayout, idx: usize) -> bool {
    layout.game_at(&library.games, idx).is_none_or(|game| {
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

    /// The card grid. Everything below the window arithmetic here is O(visible): the windows are
    /// index ranges, and every pass iterates one of them rather than the library.
    pub(super) fn prepare_grid(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()> {
        let (screen_w, screen_h) = (ctx.screen.w, ctx.screen.h);
        // The same three numbers `advance_frame` sized `self.render.grid.card_size` from — the
        // grid's whole geometry follows from the width it has to fill.
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        let (card_w, card_h) = view::home::grid_card_size(available_w, columns);
        // Reset before the branch: it is only ever set inside it, and a stale `true` left
        // behind by a host that has since been deselected would spin the render loop at
        // full rate forever.
        self.render.grid.tiles_pending = false;
        if self.library.selected_host.is_none() {
            return self.prepare_no_host_tile(ctx);
        }
        // Nothing behind an open modal can come into view: the grid neither scrolls nor
        // moves focus while a modal owns input, so the whole windowed pass — the one cost
        // here that scales with the window — is skipped unless a card was actually
        // invalidated. The grid still composites under the modal's scrim from the tiles it
        // already holds.
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
        let keep_window = window(
            first_visible_row - CARD_KEEP_ROWS,
            first_visible_row + visible_rows + CARD_KEEP_ROWS,
        );

        // Held by value, not re-derived per index — and, unlike the `App`
        // helpers, it maps indices without borrowing all of `self`, so the art
        // lookups below can sit next to `&mut self.jobs.art`.
        let layout = self.grid_layout(columns);
        self.evict_cards_outside(keep_window, &layout, ctx.tiles);
        let pending = self.build_card_window(build_window.clone(), &layout, columns, card_w, card_h, ctx)?;
        self.prepare_focused_card_tiles(&layout, columns, card_w, card_h, pending, ctx)?;
        // Order against the reveal below doesn't matter: both only ensure tiles and record what
        // they rebuilt.
        self.prepare_grid_shared_tiles(card_w, card_h, ctx)?;
        self.advance_grid_reveal(build_window, &layout, ctx);
        Ok(())
    }

    /// Drops the card tiles that can no longer be right — the whole set on a library or host
    /// change, otherwise just the ones a pin toggle or an arriving cover invalidated.
    fn release_stale_cards(&mut self, tiles: &mut ui::cache::TileStore) {
        // A fresh library load is the only rebuild that also re-arms the spinner.
        if self.render.grid.dirty {
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
    }

    /// Frees every resident card outside the keep window, with its cover.
    fn evict_cards_outside(
        &mut self,
        keep_window: std::ops::Range<usize>,
        layout: &GridLayout,
        tiles: &mut ui::cache::TileStore,
    ) {
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
        self.render.grid.scratch.dropped = dropped;
    }

    /// Rasterizes the cards in the build window, newest-needed first and on a time budget.
    /// Returns whether it ran out of budget with work left — the caller holds off anything
    /// that should wait for a settled grid.
    fn build_card_window(
        &mut self,
        build_window: std::ops::Range<usize>,
        layout: &GridLayout,
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
        // Art-ready cards build first — building one before its cover arrives just
        // burns a second budget slot re-dirtying it once the cover shows up. Two lists
        // rather than one sorted one, and indices rather than ids: a candidate past the
        // budget is never built, so nothing should be copied on its behalf.
        let mut ready = std::mem::take(&mut self.render.grid.scratch.ready);
        let mut waiting = std::mem::take(&mut self.render.grid.scratch.waiting);
        ready.clear();
        waiting.clear();
        for idx in build_window {
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
            if art_ready(&self.library, layout, idx) {
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
        self.render.grid.scratch.ready = ready;
        self.render.grid.scratch.waiting = waiting;
        self.render.grid.tiles_pending = pending;
        Ok(pending)
    }

    /// The tiles that exist only for the focused card: its hero prefetch, its title strip and
    /// the submenu panel a hold raises over it.
    fn prepare_focused_card_tiles(
        &mut self,
        layout: &GridLayout,
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

    /// Advances the loading spinner, and pops the whole grid in at once on the frame the window
    /// is finally complete.
    fn advance_grid_reveal(
        &mut self,
        mut build_window: std::ops::Range<usize>,
        layout: &GridLayout,
        ctx: &mut RenderCtx<'_>,
    ) {
        let RenderCtx { tiles, updated, .. } = ctx;
        if !self.render.grid.reveal.is_revealed() {
            // Rechecks the whole window rather than trusting `!pending`, since a card built
            // earlier can still be waiting behind a re-dirtied sibling; requires `art_ready`
            // too so a placeholder built this tick can't count as revealed.
            let window_ready = || {
                build_window.all(|idx| {
                    layout.pin_id_at(&self.library.games, idx).is_none_or(|id| {
                        self.render.grid.card_ids.get(id).is_some_and(|t| tiles.contains(t))
                            && art_ready(&self.library, layout, idx)
                    })
                })
            };
            let next_frame = self
                .render
                .grid
                .reveal
                .advance(self.library_fetch_in_flight(), window_ready);
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
                    color: ui::style::theme().muted,
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
