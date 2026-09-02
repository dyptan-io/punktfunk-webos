//! Composition: this frame's draw list, in paint order.
//!
//! The GPU half — pure bookkeeping over already-rasterized tiles. Position, scroll, every
//! focus pop and every fade is a texture-copy parameter here, never a re-raster (see
//! `platform::webos::compositor`). Split out of `app/mod.rs` alongside `prepare`.
use std::ops::Range;
use std::time::Instant;

use crate::app::grid::GridLayout;
use crate::app::render::tile;
use crate::app::render::SnapshotBody;
use crate::app::screens;
use crate::app::{
    hero, render_input, view, App, HomeFocus, Screen, CARD_GROWTH, LAUNCH_GROWTH, SCROLL_INDICATOR_TILE_W,
    STATUS_BG_PAD,
};
use crate::ui;
use crate::ui::cache::TileStore;
use crate::ui::render::{DrawCmd, Rect};

/// Height of each fade ramp step. SDL can't batch across alpha changes, so this divides
/// `SCROLL_FADE_H` into draw commands: 8→~12 steps/band, ~32 alpha apart. Drop if banding shows.
const FADE_STEP: u32 = 8;

/// A scrolling viewport's two edge fade bands, top then bottom, each present only while there
/// is content past that edge — the fade *is* the "there is more" signal.
type Fades = [Option<Rect>; 2];

/// [`Fades`] for a viewport that wants none.
const NO_FADES: Fades = [None; 2];

impl App {
    /// Assembles the read-only view of state the render path consumes (see
    /// `render_input::RenderInput`). Grows as families migrate off direct `self` reads.
    pub fn render_input(&self) -> render_input::RenderInput<'_> {
        render_input::RenderInput {
            home_focus: self.home_focus,
            entries: &self.hosts.entries,
            host_selected: self.library.selected_host.is_some(),
            status_alpha: self.home_status_alpha(),
            grid_reveal_ready: self.render.grid.reveal.is_revealed(),
            press: self.press_dip(Screen::Home),
            focus_anim: self.render.focus_anim,
        }
    }

    /// Compose open modal: fade scrim+shell, content crop with fades, dropdown, focus zoom, scroll indicator.
    fn compose_modal(
        &self,
        tiles: &TileStore,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
        cmds: &mut Vec<DrawCmd>,
    ) {
        // A left modal keeps drawing from its snapshot while its fade runs (see
        // `snapshot_closing_modal`) — the entering one owns `tile::MODAL` from this frame.
        // The two overlap, so leaving one modal for another cross-fades.
        let screen = self.nav.screen;
        // Over live video the card is all there is: the full-screen scrim and the frost pane are
        // graphics that would cover the pattern being measured. No open animation either — the
        // card must not move while a step changes, or the eye reads the motion as the pattern.
        if screens::over_video(screen) {
            self.compose_modal_card(
                tiles,
                screen,
                ui::render::Size::new(screen_w, screen_h),
                fonts,
                1.0,
                cmds,
            );
            return;
        }
        let m = if matches!(screen, Screen::Home) {
            0.0
        } else {
            self.render.modal.fade.open_alpha()
        };
        // m is cross-fade's leaving card inverse (see ModalFade).
        let closing = self
            .render
            .modal
            .prev
            .zip(self.render.modal.fade.closing_frame_against(m))
            .map(|(prev, (alpha, _))| (alpha, prev));
        // Backdrop is "modal up", not per-card; only fades when modal layer appears/disappears.
        let scrim = if closing.is_some() && !matches!(screen, Screen::Home) {
            1.0
        } else {
            m.max(closing.map_or(0.0, |(alpha, _)| alpha))
        };
        // Frost panes before scrim (compositor captures blur at first Frost; ordering matters
        // to avoid two-phase blur of dimmed screen when opening from sidebar vs card).
        // Invariant: nothing tinting whole screen before a frost pane.
        if !matches!(screen, Screen::Home) {
            let region = self.render.modal.tile_region.offset(0, ui::animation::modal_rise(m));
            Self::push_frost(cmds, region, ui::widgets::MODAL_RADIUS, (255.0 * m) as u8);
        }
        if let Some((alpha, prev)) = closing {
            Self::push_frost(
                cmds,
                prev.region.offset(0, ui::animation::modal_rise(alpha)),
                ui::widgets::MODAL_RADIUS,
                (255.0 * alpha) as u8,
            );
        }
        if scrim > 0.0 {
            cmds.push(DrawCmd::Fill {
                rect: Rect::new(0, 0, screen_w, screen_h),
                color: ui::theme::palette().scrim.with_alpha_scaled(scrim),
            });
        }
        if !matches!(screen, Screen::Home) {
            self.compose_modal_card(tiles, screen, ui::render::Size::new(screen_w, screen_h), fonts, m, cmds);
        }
        // Last, so it fades away *over* what it uncovers: the entering card is often the
        // larger (a submenu returning to Settings) and would otherwise hide it entirely.
        if let Some((alpha, prev)) = closing {
            let dy = ui::animation::modal_rise(alpha);
            let a = (255.0 * alpha) as u8;
            // The snapshot is a copy of `tile::MODAL`, which no longer carries a shadow of its
            // own — so the leaving card needs the same nine draws the entering one gets.
            let region = prev.region.offset(0, dy);
            ui::painter::push_shadow(cmds, tile::MODAL_SHADOW, ui::widgets::MODAL_RADIUS, region, a);
            cmds.push(DrawCmd::Tex {
                tile: tile::MODAL_PREV,
                dst: region,
                alpha: a,
            });
            // The leaving body, through whichever of the two live paths drew it: a crop of
            // its own baked tile, or the settings rows still in their own tiles. No edge
            // fades on the way out — the card is dissolving, and a ramp on top of a ramp
            // reads as the list going first.
            match prev.content {
                Some(SnapshotBody::Cropped(src, dst)) => cmds.push(DrawCmd::TexCropped {
                    tile: tile::MODAL_PREV_CONTENT,
                    src,
                    dst: dst.offset(0, dy),
                    alpha: a,
                }),
                Some(SnapshotBody::Rows(total, content, scroll_px)) => {
                    Self::push_list_rows(cmds, total, content, scroll_px, dy, a, &NO_FADES);
                }
                None => {}
            }
        }
    }

    /// Frosted pane under modal card (blur masked to rounded shape). Menu only, not in-stream.
    fn push_frost(cmds: &mut Vec<DrawCmd>, tile_region: Rect, radius: i32, alpha: u8) {
        if alpha == 0 {
            return;
        }
        let Some(glass) = ui::theme::glass() else {
            return;
        };
        cmds.push(DrawCmd::Frost(Box::new(ui::render::FrostPane::whole(
            tile_region,
            ui::render::FrostMask {
                radius,
                corners: ui::render::Corners::All,
            },
            glass.blur,
            alpha,
            Some(ui::theme::palette().panel),
        ))));
    }

    /// Whole glass surface: pane+scrim+fill (one call, materials match card, not comments).
    fn push_glass_surface(cmds: &mut Vec<DrawCmd>, rect: Rect, radius: i32, alpha: f32) {
        Self::push_frost(cmds, rect, radius, (255.0 * alpha) as u8);
        cmds.push(DrawCmd::Fill {
            rect,
            color: ui::theme::glass_fill()
                .over(ui::theme::palette().scrim)
                .with_alpha_scaled(alpha),
        });
    }

    /// Nav column strip — opaque on every theme, so no frost pane under it.
    fn compose_sidebar(cmds: &mut Vec<DrawCmd>, screen_h: u32) {
        cmds.push(DrawCmd::Tex {
            tile: tile::SIDEBAR,
            dst: Rect::new(0, 0, ui::widgets::SIDEBAR_W, screen_h),
            alpha: 0xff,
        });
    }

    /// Open modal card, content, dropdown, focus widget. m is modal fade/rise (rides everything).
    fn compose_modal_card(
        &self,
        tiles: &TileStore,
        screen: Screen,
        size: ui::render::Size,
        fonts: &ui::text::Fonts,
        m: f32,
        cmds: &mut Vec<DrawCmd>,
    ) {
        let (screen_w, screen_h) = (size.w, size.h);
        let dy = ui::animation::modal_rise(m);
        // Tile covers card region only; opening plays fade+rise (reverse of closing snapshot).
        let modal_base = self.render.modal.tile_region.offset(0, dy);
        ui::painter::push_shadow(
            cmds,
            tile::MODAL_SHADOW,
            ui::widgets::MODAL_RADIUS,
            modal_base,
            (255.0 * m) as u8,
        );
        cmds.push(DrawCmd::Tex {
            tile: tile::MODAL,
            dst: modal_base,
            alpha: (255.0 * m) as u8,
        });
        // Scrollable content geometry (Settings rows or About document), computed
        // once and reused. Scrolling crops the full baked tile, never re-rasterizes.
        let scroll_geom = self.scroll_geometry_for(screen, screen_w, screen_h, fonts);
        if let Some((total, _, _, content)) = scroll_geom {
            let stride = self.scroll_stride_for(screen, fonts);
            let scroll_px = self.clamped_scroll_px(total, stride, content.height());
            let alpha = (255.0 * m) as u8;
            // The viewport's edge fades, resolved before the content is pushed: they ramp the
            // content's own alpha rather than being painted over it (see `push_faded`).
            //
            // Shown only while there is something past that edge — the fade is the "there is
            // more" signal, so it has to vanish exactly when scrolling reaches the end, or it
            // reads as content that can never be got to. Keyed off pixels, not rows: at either
            // end the offset is clamped mid-row, so a row-based test would keep claiming there
            // is more beyond.
            // Halved, not just clamped: at full height the two bands overlap in a viewport
            // shorter than twice a band, and `push_faded` takes the first match — so the
            // bottom edge would ramp content *in* and hard-clip where it should dissolve.
            let fade_h = ui::widgets::SCROLL_FADE_H.min(content.height() / 2);
            // Top band then bottom, each present only while there is content past that edge.
            // A stack array, not a `Vec`: this runs every frame a modal is open.
            let fades = &[
                (scroll_px > 0).then(|| Rect::new(content.x(), content.y() + dy, content.width(), fade_h)),
                (scroll_px < Self::max_scroll_px(total, stride, content.height())).then(|| {
                    Rect::new(
                        content.x(),
                        content.y() + dy + (content.height() - fade_h) as i32,
                        content.width(),
                        fade_h,
                    )
                }),
            ];
            // A row list's body is one tile per row (see `tile::list_row`), so it is placed
            // row by row; every other scrolling modal crops its single baked tile.
            if crate::app::screens::is_scroll_list(screen) {
                Self::push_list_rows(cmds, total, content, scroll_px, dy, alpha, fades);
            } else if let Some((src, dst)) = self.scroll_src_rect(screen, screen_w, screen_h, fonts) {
                Self::push_faded(cmds, tile::SCROLL_CONTENT, src, dst.offset(0, dy), alpha, fades);
            }
        }
        // An open dropdown's panel and its focused option, resolved once — the two are
        // drawn either side of the focus tile below, which is the only reason they are
        // not one block.
        let dropdown = self.dropdown_draw_state().and_then(|(row, focused, dd_alpha)| {
            let (content, scroll_px) = self.dropdown_geom(screen_w, screen_h, fonts)?;
            let overlay_rect = view::scrolllist::dropdown_overlay_rect_at_px(content, row, scroll_px);
            Some((row, focused, overlay_rect, (255.0 * m * dd_alpha) as u8))
        });
        // Dropdown overlay (Settings or Diagnostics).
        if let Some((row, _, overlay_rect, dd_alpha)) = dropdown {
            let options_len = self.dropdown_len(row);
            let panel = Rect::new(
                overlay_rect.x(),
                overlay_rect.y() + dy,
                overlay_rect.width(),
                options_len as u32 * ui::widgets::DROPDOWN_OPTION_H,
            );
            // The popup lifts off the row behind it, same as the card lifts off the screen.
            ui::painter::push_shadow(cmds, tile::PANEL_SHADOW, ui::widgets::CARD_RADIUS, panel, dd_alpha);
            cmds.push(DrawCmd::Tex {
                tile: tile::DROPDOWN_OVERLAY,
                dst: panel,
                alpha: dd_alpha,
            });
        }
        // Focused widget of the active modal (setting row, button, etc.);
        // composites on shell at its on-screen position (no re-rasterize on move).
        // The entering screen's only — the snapshot has its own focused row baked in.
        let focus_rect = self.modal_focus_rect(screen, screen_w, screen_h, fonts);
        if let Some(rect) = focus_rect {
            let base = rect.offset(0, dy);
            // The zoom-in: same GPU-scale-around-center technique as the
            // grid's card focus pop (see above) — `modal_focus_tile` is
            // rasterized once at its literal size, never re-rendered for
            // this (except while `switch_anim` animates its content, see
            // `prepare_tiles`).
            let dst = ui::animation::focus_row_tile_rect(base, self.render.modal.focus_anim, self.press_dip(screen));
            let alpha = (255.0 * m) as u8;
            // In a scrolling modal the focused row can hang past the viewport's bottom
            // edge mid-glide (the crop lags the row offset by up to one stride), so it is
            // clipped rather than left to paint over the card's chrome. Every other modal
            // keeps the plain unclipped path — none of them scrolls.
            let tile_size = tiles.get(tile::MODAL_FOCUS).map(|p| (p.width(), p.height()));
            match (scroll_geom, tile_size) {
                (Some((_, _, _, content)), Some((tw, th))) => {
                    let viewport = content.inflate(ui::tiles::ROW_TILE_PAD).offset(0, dy);
                    if let Some((src, visible)) = Self::clip_tile(dst, viewport, tw, th) {
                        cmds.push(DrawCmd::TexCropped {
                            tile: tile::MODAL_FOCUS,
                            src,
                            dst: visible,
                            alpha,
                        });
                    }
                }
                _ => cmds.push(DrawCmd::Tex {
                    tile: tile::MODAL_FOCUS,
                    dst,
                    alpha,
                }),
            }
        }
        // The open dropdown's focused option — same idea, composited on
        // top of the shell's unfocused option list at its actual
        // position, so navigating dropdown options needs no modal
        // re-rasterize either. `Settings` or `Diagnostics`.
        if let Some((_, focused, overlay_rect, dd_alpha)) = dropdown {
            let option_rect = ui::widgets::dropdown_option_rect(overlay_rect, focused);
            cmds.push(DrawCmd::Tex {
                tile: tile::DROPDOWN_FOCUS,
                dst: Rect::new(
                    option_rect.x(),
                    option_rect.y() + dy,
                    option_rect.width(),
                    option_rect.height(),
                ),
                alpha: dd_alpha,
            });
        }
        // Whichever modal is scrollable, its indicator (names kept from when only Settings
        // had one; every scrollable modal now shares the same timing and the same
        // `self.render.scroll.shown_at` clock, since only one is ever open at a time).
        if let Some((total, visible, card, content)) = scroll_geom {
            if total > visible {
                let scroll_alpha = self.scroll_indicator_alpha().unwrap_or(0.0);
                if scroll_alpha > 0.0 {
                    // Sits nearer the card's edge than the content's, so it doesn't
                    // overlap a Settings row's dropdown pill/slider/switch. The `26`
                    // offset isn't derived from either modal's own width fraction —
                    // re-check both if either changes.
                    let dst = Rect::new(
                        card.right() - 26,
                        content.y() + dy,
                        SCROLL_INDICATOR_TILE_W,
                        content.height(),
                    );
                    cmds.push(DrawCmd::Tex {
                        tile: tile::SCROLL_INDICATOR,
                        dst,
                        alpha: (255.0 * m * scroll_alpha) as u8,
                    });
                }
            }
        }
    }

    fn push_list_rows(
        cmds: &mut Vec<DrawCmd>,
        total: usize,
        content: Rect,
        scroll_px: i32,
        dy: i32,
        alpha: u8,
        fades: &Fades,
    ) {
        let viewport = content.offset(0, dy);
        for i in 0..total {
            let Some(id) = tile::list_row(i) else { break };
            let dst = ui::widgets::focus_row_rect_at_px(content, i, scroll_px).offset(0, dy);
            // Rows scrolled fully out of the viewport cost nothing but this test; the ones
            // straddling an edge are cropped rather than allowed to paint over the chrome.
            let Some((src, visible)) = Self::clip_tile(dst, viewport, content.width(), ui::widgets::FOCUS_ROW_H) else {
                continue;
            };
            Self::push_faded(cmds, id, src, visible, alpha, fades);
        }
    }

    /// One crop of scrolled content, sliced and alpha-ramped where it crosses a viewport edge
    /// fade. See [`Fades`]: the top band fades content *in* down its height, the bottom out.
    ///
    /// The fade used to be a band of card glass painted over the outgoing row. On the frosted
    /// theme that band is a second frosted surface — its own blur, its own tint, sampled and
    /// masked separately — and its seams showed against the card it was supposed to be part
    /// of. Dissolving the *content* instead adds no surface at all: the row thins out into
    /// whatever the card already is, on either theme, and there is nothing left to seam.
    fn push_faded(cmds: &mut Vec<DrawCmd>, tile: ui::render::TileId, src: Rect, dst: Rect, alpha: u8, fades: &Fades) {
        let strip = |y: i32, h: u32, alpha: u8| DrawCmd::TexCropped {
            tile,
            src: Rect::new(src.x(), src.y() + (y - dst.y()), src.width(), h),
            dst: Rect::new(dst.x(), y, dst.width(), h),
            alpha,
        };
        let mut y = dst.y();
        while y < dst.bottom() {
            let band = fades
                .iter()
                .enumerate()
                .filter_map(|(i, b)| b.map(|b| (i == 0, b)))
                .find(|(_, b)| y >= b.y() && y < b.bottom());
            let Some((dense_top, band)) = band else {
                // Clear of every band: one command up to the next one, not a run of strips.
                let next = fades
                    .iter()
                    .flatten()
                    .map(Rect::y)
                    .filter(|&top| top > y)
                    .chain(std::iter::once(dst.bottom()))
                    .min()
                    .unwrap_or(dst.bottom());
                cmds.push(strip(y, (next - y) as u32, alpha));
                y = next;
                continue;
            };
            let h = FADE_STEP.min((dst.bottom() - y) as u32).min((band.bottom() - y) as u32);
            // The app's one edge ramp, so this reads as the label fade turned ninety degrees.
            let mid = (y + h as i32 / 2 - band.y()).max(0) as usize;
            let eased = ui::painter::fade_step(mid, band.height() as usize);
            let eased = if dense_top { eased } else { 255 - eased };
            let a = ((u16::from(alpha) * u16::from(eased)) / 255) as u8;
            if a > 0 {
                cmds.push(strip(y, h, a));
            }
            y += h as i32;
        }
    }

    /// Sidebar family compose: the focused-row highlight overlay (the strip itself
    /// is an unconditional `tile::SIDEBAR` blit in `draw_list`). Reads only the
    /// `RenderInput` slice — a template for the per-family `TileCache::compose` split.
    fn compose_sidebar_focus(input: &render_input::RenderInput<'_>, screen_h: u32, cmds: &mut Vec<DrawCmd>) {
        let sidebar_focus_row = match input.home_focus {
            HomeFocus::Sidebar(i) | HomeFocus::SidebarMenu(i) => Some(i),
            HomeFocus::Grid(_) => None,
        };
        if let Some(i) = sidebar_focus_row {
            let rect = view::sidebar::nav_row_rect(i, input.entries.len() + 2, screen_h);
            cmds.push(DrawCmd::TexF {
                tile: tile::FOCUS_ROW,
                dst: ui::animation::focus_row_tile_rect_f(rect, input.focus_anim, input.press),
                alpha: 0xff,
            });
        }
    }

    /// Grid family compose: the card tiles at their scrolled positions, the pinned
    /// divider, and the focused card with its ring/title-strip/pin-badge pop. Only reached
    /// once the grid is revealed. Extracted from `draw_list` (A2 staging).
    fn compose_grid(&self, tiles: &TileStore, screen: ui::render::Size, cmds: &mut Vec<DrawCmd>) {
        // Derived here rather than passed down: all three follow from the screen's width, and
        // `draw_list` was handing them over one by one purely because this used to live in it.
        let screen_h = screen.h;
        let grid_x = ui::widgets::SIDEBAR_W as i32;
        let available_w = screen.w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);
        let count = self.grid_len(columns);
        let focused = match self.home_focus {
            HomeFocus::Grid(i) if i < count => Some(i),
            HomeFocus::Grid(_) | HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_) => None,
        };
        let pad = ui::tiles::CARD_SHADOW_PAD;
        // One layout for the whole frame: every card rect below would otherwise rebuild it
        // (see `home_focus_map`).
        let layout = self.library.layout(columns);
        let card_rect = |idx| view::home::scrolled_card_rect(idx, grid_x, available_w, layout, self.render.grid.scroll);
        // The on-screen window, computed rather than found by testing every card in the
        // library once per frame (`view::home::visible_cards`).
        let visible = view::home::visible_cards(available_w, layout, self.render.grid.scroll, screen_h as i32, pad);
        // While a held card's new place in its collection is unwritten, the rest of that
        // collection is dimmed to the modal scrim's level: the block whose order is in flux,
        // and nothing else. A scale on the card's own alpha rather than a `Fill` over it — a
        // fill is a square rect and would square off the card's rounded corners.
        let unfixed = self.reordering_slots(layout);
        // One clock read for the whole grid pass — every card's arrival is measured against it.
        let now = Instant::now();
        let dimmed = 1.0 - f32::from(ui::theme::palette().scrim.a) / 255.0;
        for idx in visible {
            if Some(idx) == focused {
                continue; // drawn last, on top of its neighbors
            }
            // padding after a partial pinned row — nothing to draw
            let Some(pin_id) = layout.pin_id_at(&self.library.games, idx) else {
                continue;
            };
            let r = card_rect(idx);
            // Tile and pop clock in one lookup — this runs per visible card per frame.
            let Some(slot) = self.render.grid.card_ids.slot(pin_id) else {
                continue; // not rasterized yet — outside the build window
            };
            let card = slot.id;
            // A card that just landed is still fading (reveal) or zooming (later build) in.
            let (pop, shrink) = tile::entrance_progress(slot.pop, now);
            let dim = if unfixed.as_ref().is_some_and(|s| s.contains(&idx)) {
                dimmed
            } else {
                1.0
            };
            let alpha = (255.0 * pop * dim) as u8;
            // A fully transparent card — one the reveal wave has not reached — is two
            // full-size blits and two alpha-mod changes that draw nothing.
            if alpha == 0 {
                continue;
            }
            cmds.push(DrawCmd::Tex {
                tile: tile::CARD_SHADOW,
                dst: ui::animation::pop_in_rect(r.inflate(pad), pop, shrink),
                alpha,
            });
            cmds.push(DrawCmd::Tex {
                tile: card,
                dst: ui::animation::pop_in_rect(r, pop, shrink),
                alpha,
            });
        }
        // One heading per section, in place of the hairline that used to divide the blocks —
        // scrolled with everything else (there's no separate fixed region), so they are just
        // tiles at their own scrolled positions, culled the same way. Bottom-aligned in their
        // band, sitting on the block they name.
        for (i, (first_idx, _)) in layout.headings().enumerate() {
            let Some(id) = tile::section(i) else { break };
            let band =
                view::home::section_heading_rect(first_idx, grid_x, available_w, layout, self.render.grid.scroll);
            if band.bottom() < 0 || band.y() > screen_h as i32 {
                continue;
            }
            let Some(tile) = tiles.get(id) else { continue };
            let (w, h) = (tile.width(), tile.height());
            cmds.push(DrawCmd::Tex {
                tile: id,
                dst: Rect::new(
                    band.x(),
                    band.bottom() - h as i32 - view::home::SECTION_HEADING_PAD,
                    w,
                    h,
                ),
                alpha: 0xff,
            });
        }
        if let Some(idx) = focused {
            if let Some(pin_id) = layout.pin_id_at(&self.library.games, idx) {
                // The dip on the card's own rect, so the panel, its frost and the ring all
                // ride it: on Home nothing else arms one (a card is not `pressable`), and
                // an in-collection swap with nowhere to go is what does (see
                // `App::swap_card_in_collection`).
                let r = self.press_dip(Screen::Home).rect(card_rect(idx));
                self.compose_focused_card(tiles, cmds, pin_id, r, pad, now);
            }
        }
        // The reveal's dissolve: every card above is already fully built and drawn opaque —
        // covered here by a background-coloured mask whose own alpha falls away as the wave
        // passes, so the page uncovers as one surface instead of each card fading in on its
        // own clock (see `spinner::GridReveal::dissolve_mask` for why this is a fading cover
        // rather than the launch backdrop's erase-based technique).
        if self.render.grid.reveal.dissolving() {
            let cover = Rect::new(grid_x, 0, available_w, screen_h);
            cmds.push(DrawCmd::Tex {
                tile: tile::GRID_REVEAL_MASK,
                dst: cover,
                alpha: 0xff,
            });
        }
    }

    /// The grid slots of the collection whose order a held card has changed and not yet
    /// fixed, if any — what `compose_grid` dims. `None` is the usual case, and costs one
    /// `Option` test.
    fn reordering_slots(&self, layout: GridLayout<'_>) -> Option<Range<usize>> {
        let menu = self.card_menu.as_ref().filter(|m| m.moved)?;
        layout
            .placed()
            .find(|p| p.slots().contains(&menu.idx))
            .map(|p| p.slots())
    }

    /// The focused card, drawn last and on top of its neighbours: its glow, contact shadow,
    /// focus pop and title strip (or the submenu panel a hold grew out of it), plus the pin
    /// badge. `r` is its unscaled rect — everything here scales about the card's own centre, so
    /// the pops can't fight over position.
    fn compose_focused_card(
        &self,
        tiles: &TileStore,
        cmds: &mut Vec<DrawCmd>,
        pin_id: &str,
        r: Rect,
        pad: i32,
        now: Instant,
    ) {
        // The focus pop: the GPU scales the (unfocused) card tile up
        // around its center as the pop progresses, with the shared glow
        // tile fading in behind it at the same scale.
        let f = ui::animation::anim_frac_smooth(self.render.focus_anim, ui::animation::CARD_FOCUS_POP);
        let Some(slot) = self.render.grid.card_ids.slot(pin_id) else {
            return; // not rasterized yet
        };
        let card = slot.id;
        let (pop, shrink) = tile::entrance_progress(slot.pop, now);
        let popped = |base: Rect| ui::animation::pop_in_rect(base, pop, shrink);
        // The card's total scale, for anything composited on top of it that has to
        // fold in the same transform about the card's centre rather than its own.
        // Shared with the pointer path (`card_menu_rows_rect`), so a click can't land
        // on a row other than the one drawn under it.
        let card_scale = self.focused_card_scale(pin_id);
        // Glow first — a halo behind the card, not an outline on top of it. Its
        // alpha rides the same eased `f` as the zoom, so it blooms over the whole
        // travel instead of snapping on ahead of it. No fade-out to match: focus
        // leaving a card kills its glow in one frame, since the card moved *to* is
        // already blooming and two lit cards read as ambiguous focus.
        let ring_base = r.inflate(ui::tiles::FOCUS_RING_PAD);
        cmds.push(DrawCmd::Tex {
            tile: tile::RING,
            dst: popped(ui::animation::zoom_rect(ring_base, f, CARD_GROWTH)),
            alpha: (255.0 * f * pop) as u8,
        });
        // Then the shadow, over the glow rather than under it — it is the card's
        // own contact shadow, so the halo must not wash it out.
        cmds.push(DrawCmd::Tex {
            tile: tile::CARD_SHADOW,
            dst: popped(ui::animation::zoom_rect(r.inflate(pad), f, CARD_GROWTH)),
            alpha: (255.0 * pop) as u8,
        });
        // The focused card zooms in on first appearance like any other,
        // composed with its focus pop — both scale around the card's own
        // center, so they can't fight over position.
        cmds.push(DrawCmd::Tex {
            tile: card,
            dst: popped(ui::animation::zoom_rect(r, f, CARD_GROWTH)),
            alpha: (255.0 * pop) as u8,
        });
        self.compose_card_strip(tiles, cmds, r, pop, card_scale);
        // The lit edge last, over the art *and* the strip, so the card ends on one
        // unbroken line. It gives the glow behind a hard boundary to end on, so
        // the halo reads as light off the edge rather than a smudge fading into
        // the art. `CARD_RADIUS`-rounded like the rest of the stack.
        cmds.push(DrawCmd::Tex {
            tile: tile::CARD_OUTLINE,
            dst: popped(ui::animation::zoom_rect(
                r.inflate(ui::tiles::CARD_OUTLINE_PAD),
                f,
                CARD_GROWTH,
            )),
            alpha: (255.0 * f * pop) as u8,
        });
    }

    /// The blur under a focused card's glass, whether that is the one-line title strip or the
    /// submenu panel grown out of it: the card as this frame has already composed it —
    /// zoomed, scrolled, whatever art finished loading — blurred and cut to the card's own
    /// rounded bottom edge.
    ///
    /// Drawn only where the look has glass, like [`push_frost`](Self::push_frost). The strip
    /// stays readable with it off because `ui::widgets::card_glass` goes opaque at the same
    /// time — a blur is what lets a *translucent* strip carry a title over cover art, so the
    /// two have to move together or the title lands on bare art.
    ///
    /// This is also where the switch earns its cost: a focused card pushes a pane on every
    /// menu frame, and that is what makes the compositor capture and minify the whole screen.
    ///
    /// `panel_h` is the glass's full height and `shown` how much of it the wipe has revealed,
    /// both unscaled. The mask and the blur scratch are built at the unscaled size and the
    /// zoom is applied in the blit, so a focus pop rebuilds neither (see `FrostPane::shape`).
    fn push_card_frost(&self, cmds: &mut Vec<DrawCmd>, r: Rect, card_scale: f32, panel_h: u32, shown: u32, alpha: u8) {
        let Some(glass) = ui::theme::glass() else {
            return;
        };
        let panel = Rect::new(r.x(), r.bottom() - panel_h as i32, r.width(), panel_h);
        let window = Rect::new(r.x(), r.bottom() - shown as i32, r.width(), shown);
        cmds.push(DrawCmd::Frost(Box::new(ui::render::FrostPane {
            shape: ui::render::Size::new(r.width(), panel_h),
            at: ui::animation::scale_about(panel, r, card_scale),
            dst: ui::animation::scale_about(window, r, card_scale),
            mask: ui::render::FrostMask {
                radius: ui::widgets::CARD_RADIUS,
                corners: ui::render::Corners::Bottom,
            },
            blur: glass.blur,
            alpha,
            // The flat fallback where this renderer has no blur: the glass tint above still
            // covers the art, just without softening it.
            fallback: Some(ui::theme::palette().panel),
        })));
    }

    /// The focused card's title strip, or the taller submenu panel a hold grew out of it.
    /// `pop` and `card_scale` are the card's own transform: everything here rides the card
    /// rather than animating on its own, or the frost drifts off the cover baked into it.
    fn compose_card_strip(&self, tiles: &TileStore, cmds: &mut Vec<DrawCmd>, r: Rect, pop: f32, card_scale: f32) {
        // The title strip wipes up the card's bottom edge — a wipe, not a slide:
        // the tile's bottom `shown` rows go to the card's bottom `shown` rows, so
        // the frosted art baked into it stays registered with the art beneath.
        // Sliding the whole tile dragged that baked cover fragment up with it,
        // reading as the card shifting under the glass. `r` is the pivot: a band
        // scaled about its own center drifts off the edge it sits flush to.
        // A held card's submenu grows the same frost into a taller panel, and its
        // title and rows are separate transparent tiles (see `render_card_menu_tile`)
        // so they can ride the growing window's top edge. Falls back to the plain
        // strip until all three are built, which is what the first frames after
        // launch see.
        // Collapsed to a bare title strip while the card is being reordered: the panel names
        // what Confirm does, and mid-reorder Confirm means "leave it there" rather than any
        // of its rows (see `App::fix_card_position`). Dropping to `None` here is the whole
        // collapse — the arm below draws the plain strip.
        let menu = self.card_menu.as_ref().filter(|m| !m.moved).and_then(|_| {
            tiles.get(tile::CARD_MENU_TITLE)?;
            Some((
                tiles.get(tile::CARD_MENU).map(ui::Painter::height)?,
                tiles.get(tile::CARD_MENU_ROWS).map(ui::Painter::height)?,
            ))
        });
        let title_h = tiles.get(tile::CARD_TITLE).map(ui::Painter::height);
        match (menu, title_h) {
            // Menu open: the window grows from the title strip's height — already on
            // screen and already focused — up to the full panel, rather than
            // restarting from the card's bottom edge.
            (Some((panel_h, rows_h)), Some(title_h)) if panel_h > title_h => {
                // On its own clock: `focus_anim` is re-armed by every focus move, so
                // the panel would pop open the instant the menu is dismissed by
                // moving off the card.
                let clock = self
                    .card_menu
                    .as_ref()
                    .map_or(self.render.focus_anim, |m| Some(m.since));
                // Smoothstep, not the cubic ease-out the plain strip's short wipe
                // uses: over a whole panel's travel `1-(1-t)³` is 87% done at the
                // halfway point, so the tail reads as a small late move after the
                // panel has apparently stopped. Same reason `CARD_FOCUS_POP`'s zoom
                // is on `anim_frac_smooth` (see `ui::animation`).
                let wipe = ui::animation::anim_frac_smooth(clock, ui::animation::CARD_MENU_RISE);
                let shown = title_h + ((panel_h - title_h) as f32 * wipe) as u32;
                // Panel-local coordinates throughout: local y maps to
                // `r.bottom() - (panel_h - y)`, and the revealed window is
                // `[panel_h - shown, panel_h]`.
                let window_top = (panel_h - shown) as i32;
                self.push_card_frost(cmds, r, card_scale, panel_h, shown, (255.0 * pop) as u8);
                cmds.push(DrawCmd::TexCropped {
                    tile: tile::CARD_MENU,
                    src: Rect::new(0, window_top, r.width(), shown),
                    dst: ui::animation::scale_about(
                        Rect::new(r.x(), r.bottom() - shown as i32, r.width(), shown),
                        r,
                        card_scale,
                    ),
                    alpha: (255.0 * pop) as u8,
                });
                // Title and rows both hang off the window's top edge, so the title
                // continues upward from exactly where the plain strip had it and the
                // rows follow it in. Nothing restarts from the bottom.
                cmds.push(DrawCmd::Tex {
                    tile: tile::CARD_MENU_TITLE,
                    dst: ui::animation::scale_about(
                        Rect::new(r.x(), r.bottom() - shown as i32, r.width(), title_h),
                        r,
                        card_scale,
                    ),
                    alpha: (255.0 * pop) as u8,
                });
                let rows_top = window_top + title_h as i32;
                // The selection sits under the row text, not over it: the band is an opaque
                // surface fill, so a label drawn beneath it would be covered outright.
                // Held back until the rise lands: the band's shadow margin and its focus pop are
                // added after `card_menu_band` clipped to the panel, so on a half-revealed row
                // they hang the lit pill below the card's bottom edge. It still takes `rows_top`
                // so the geometry stays one path with the labels.
                if let Some((band, tile)) = self
                    .card_menu_band(r, panel_h, rows_top)
                    .filter(|_| wipe >= 1.0)
                    .zip(tiles.get(tile::CARD_MENU_BAND))
                {
                    // The rows block hangs off the *top* of the revealed window, so a row that
                    // has not been reached yet is clipped at its bottom by the panel's own
                    // edge — never at its top. The crop therefore starts at the tile's y=0,
                    // exactly as `tile::CARD_MENU_ROWS` does below. Height is the visible
                    // band plus `ROW_TILE_PAD` on every side, the margin its shadow lives in
                    // (see `CardMenuBandTile`).
                    let pad = ui::tiles::ROW_TILE_PAD;
                    // The focus pop, through the same helper every other focused widget in
                    // the app is placed by — about the band's own centre, inside the card,
                    // and then the card's transform on top of that. The inset the band is
                    // drawn at (`CARD_MENU_BAND_INSET`) is what the growth spends, so a
                    // focused row stays within the cover art.
                    let popped = ui::animation::focus_row_tile_rect(
                        band,
                        self.card_menu.as_ref().and_then(|m| m.focus_anim),
                        ui::animation::Press::default(),
                    );
                    cmds.push(DrawCmd::TexCropped {
                        tile: tile::CARD_MENU_BAND,
                        src: Rect::new(0, 0, tile.width(), band.height() + 2 * pad as u32),
                        dst: ui::animation::scale_about(popped, r, card_scale),
                        alpha: (255.0 * pop) as u8,
                    });
                }
                let vis_bottom = (rows_top + rows_h as i32).min(panel_h as i32);
                if vis_bottom > rows_top {
                    let visible_h = (vis_bottom - rows_top) as u32;
                    cmds.push(DrawCmd::TexCropped {
                        tile: tile::CARD_MENU_ROWS,
                        src: Rect::new(0, 0, r.width(), visible_h),
                        dst: ui::animation::scale_about(
                            Rect::new(r.x(), r.bottom() - (panel_h as i32 - rows_top), r.width(), visible_h),
                            r,
                            card_scale,
                        ),
                        alpha: (255.0 * pop) as u8,
                    });
                }
            }
            // No menu: the plain title strip, wiping up from the card's bottom edge
            // on the card's own focus clock.
            (_, Some(strip_h)) => {
                let wipe = ui::animation::anim_frac(self.render.focus_anim, ui::animation::CARD_FOCUS_POP);
                let shown = (strip_h as f32 * wipe) as u32;
                if shown > 0 {
                    self.push_card_frost(cmds, r, card_scale, strip_h, shown, (255.0 * pop) as u8);
                    cmds.push(DrawCmd::TexCropped {
                        tile: tile::CARD_TITLE,
                        src: Rect::new(0, (strip_h - shown) as i32, r.width(), shown),
                        dst: ui::animation::scale_about(
                            Rect::new(r.x(), r.bottom() - shown as i32, r.width(), shown),
                            r,
                            card_scale,
                        ),
                        alpha: (255.0 * pop) as u8,
                    });
                }
            }
            (_, None) => {}
        }
    }

    /// Builds this frame's draw list (paint order) from the current state and animation
    /// clocks — pure bookkeeping, no rasterization. The font params are geometry only
    /// (`ui::text::modal_header_end_y` and friends), needed to place a modal's
    /// focused-widget tile without re-rendering its header. The GPU executes the result
    /// (`Compositor::execute`).
    pub fn draw_list(
        &self,
        tiles: &TileStore,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> ui::render::DrawList {
        let input = self.render_input();
        let mut cmds = Vec::new();
        let grid_x = ui::widgets::SIDEBAR_W as i32;
        let available_w = screen_w.saturating_sub(ui::widgets::SIDEBAR_W);
        let columns = view::home::grid_columns(available_w);

        // Over the video plane nothing composes behind the card: the sidebar, the grid and the
        // status block are graphics that would cover the very thing the user is looking at.
        // The launch backdrop's dissolve is the same case — the menu it faded from is behind
        // the picture the wave is uncovering (see `App::over_video_layers`).
        if !self.over_video_layers() {
            if !input.host_selected {
                if let Some(p) = tiles.get(tile::NO_HOST) {
                    cmds.push(DrawCmd::Tex {
                        tile: tile::NO_HOST,
                        dst: Rect::new(
                            grid_x + view::home::GRID_PAD,
                            view::home::GRID_TOP_Y,
                            p.width(),
                            p.height(),
                        ),
                        alpha: 0xff,
                    });
                }
            } else if !input.grid_reveal_ready {
                let (idx, frame) = crate::app::assets::spinner_frame_at(self.render.grid.reveal.phase());
                let (fw, fh) = (frame.width(), frame.height());
                let x = grid_x + (available_w as i32 - fw as i32) / 2;
                // 40% down rather than dead-center, which reads as slightly low on a TV.
                let area_h = screen_h as i32 - view::home::GRID_TOP_Y;
                let y = view::home::GRID_TOP_Y + (area_h - fh as i32) * 2 / 5;
                cmds.push(DrawCmd::Tex {
                    tile: tile::spinner(idx),
                    dst: Rect::new(x, y, fw, fh),
                    alpha: 0xff,
                });
            } else {
                self.compose_grid(tiles, ui::render::Size::new(screen_w, screen_h), &mut cmds);
            }
            if let Some(alpha) = input.status_alpha {
                if let Some(p) = tiles.get(tile::STATUS) {
                    let line_h = fonts.raster.height(fonts.label) + 6;
                    let box_h = 2 * line_h as u32 + 2 * STATUS_BG_PAD as u32;
                    let box_y = screen_h as i32 - box_h as i32;
                    let a = (255.0 * alpha) as u8;
                    let block = Rect::new(grid_x, box_y, available_w, box_h);
                    // Square-cornered: the band is a full-width cut across the bottom edge,
                    // not a card.
                    Self::push_glass_surface(&mut cmds, block, 0, alpha);
                    let y = box_y + (box_h as i32 - p.height() as i32) / 2;
                    cmds.push(DrawCmd::Tex {
                        tile: tile::STATUS,
                        dst: Rect::new(grid_x + view::home::GRID_PAD, y, p.width(), p.height()),
                        alpha: a,
                    });
                }
            }

            Self::compose_sidebar(&mut cmds, screen_h);
            Self::compose_sidebar_focus(&input, screen_h, &mut cmds);
        }

        self.compose_modal(tiles, screen_w, screen_h, fonts, &mut cmds);
        // The launch transition: the confirmed card zooms in around its own
        // center (same `zoom_rect` technique as the focus pop, so its aspect
        // ratio never changes) while a black scrim blends in over it, both driven
        // by the same clock — the card keeps zooming for the whole fade.
        if let (Some(t), Some(idx)) = (self.launch_anim, self.launch_anim_idx) {
            // Both of these are the fade *to* the loading screen. Once the backdrop is
            // dissolving off it again there is live video behind them, which a zooming card
            // and a full-screen black would cover back up.
            if !self.over_video_layers() {
                let f = ui::animation::anim_frac(Some(t), hero::LAUNCH_FADE);
                let layout = self.library.layout(columns);
                let base = view::home::scrolled_card_rect(idx, grid_x, available_w, layout, self.render.grid.scroll);
                if let Some(card) = self
                    .pin_id_at_grid_idx(idx, columns)
                    .and_then(|pin_id| self.render.grid.card_ids.get(pin_id))
                {
                    cmds.push(DrawCmd::Tex {
                        tile: card,
                        dst: ui::animation::zoom_rect(base, f, LAUNCH_GROWTH),
                        alpha: 0xff,
                    });
                }
                cmds.push(DrawCmd::Fill {
                    rect: Rect::new(0, 0, screen_w, screen_h),
                    color: crate::ui::render::Color::RGBA(0, 0, 0, (255.0 * f) as u8),
                });
            }
            // With wide art for this game, the loading screen is that art instead of the
            // bare black: it fades in over the scrim above (so a hero arriving mid-
            // handshake still eases in rather than snapping), then drifts slowly left to
            // right for as long as the stream takes to come up.
            self.compose_hero(screen_w, screen_h, &mut cmds);
        }
        cmds
    }

    /// The connecting screen's hero backdrop: fade in/out, slow pan, and its dimming
    /// scrim. Fading rather than cutting at both ends, over the same black the launch faded
    /// to, so a hero arriving mid-handshake eases in and live video arrives out of black
    /// rather than from a lit image.
    fn compose_hero(&self, screen_w: u32, screen_h: u32, cmds: &mut ui::render::DrawList) {
        let Some(hero) = self.render.hero.visible() else { return };
        // Leaving over live video, the wave in the mask below is the fade — the image itself
        // holds whatever it faded in to, so the two are not fighting over one alpha.
        let dissolving = self.render.hero.dissolving();
        let f = if dissolving {
            self.render.hero.fade_in()
        } else {
            self.render.hero.opacity()
        };
        cmds.push(DrawCmd::TexF {
            tile: tile::HERO,
            dst: hero::hero_pan_dst(
                hero.width,
                hero.height,
                screen_w,
                screen_h,
                self.render.hero.panned_for(),
            ),
            alpha: (255.0 * f) as u8,
        });
        // Two motions at once on the way out: the scrim deepens to black over the whole
        // image while the wave takes it away piece by piece.
        let scrim = if dissolving {
            self.render.hero.exit_scrim()
        } else {
            hero::HERO_SCRIM_ALPHA * f
        };
        cmds.push(DrawCmd::Fill {
            rect: Rect::new(0, 0, screen_w, screen_h),
            color: crate::ui::render::Color::RGBA(0, 0, 0, scrim as u8),
        });
        if dissolving {
            // Both of the above taken away again, per pixel, as the wave passes: the scrim
            // goes with the art it was dimming, and what is left is the picture on the video
            // plane behind the graphics plane.
            cmds.push(DrawCmd::Erase {
                tile: tile::HERO_MASK,
                dst: Rect::new(0, 0, screen_w, screen_h),
            });
        }
    }
}
