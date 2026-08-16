//! Composition: this frame's draw list, in paint order.
//!
//! The GPU half — pure bookkeeping over already-rasterized tiles. Position, scroll, every
//! focus pop and every fade is a texture-copy parameter here, never a re-raster (see
//! `platform::webos::compositor`). Split out of `app/mod.rs` alongside `prepare`.
use crate::app::render::tile;
use crate::ui;
use crate::ui::cache::TileStore;
use crate::ui::render::{DrawCmd, Rect};

// A glob, deliberately: these are `impl App` blocks lifted out of `app/mod.rs`, and
// they read the same private tuning constants the rest of that module does.
use crate::app::*;

/// How far a modal card slides down as it fades out (and up as it fades in), in px.
const MODAL_RISE: f32 = 26.0;

impl App {
    /// Assembles the read-only view of state the render path consumes (see
    /// `render_input::RenderInput`). Grows as families migrate off direct `self` reads.
    pub fn render_input(&self) -> render_input::RenderInput<'_> {
        render_input::RenderInput {
            home_focus: self.home_focus,
            entries: &self.entries,
            host_selected: self.selected_host.is_some(),
            has_status: self.home_status.is_some(),
            grid_reveal_ready: self.grid_reveal_ready,
        }
    }

    /// Modal family compose: the fade-in scrim + shell, scrollable content crop with
    /// its edge fades, the dropdown overlay, the focused-widget zoom, and the scroll
    /// indicator — all driven by the modal fade clock. Extracted from `draw_list`.
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
        let closing = self
            .modal_fade
            .closing_frame(MODAL_FADE_OUT)
            .and_then(|(alpha, _)| Some((alpha, self.modal_prev?)));
        let screen = self.screen;
        let m = if matches!(screen, Screen::Home) {
            0.0
        } else {
            self.modal_fade.open_alpha(MODAL_FADE)
        };
        // The backdrop belongs to "a modal is up", not to either card: re-fading it
        // mid-step would brighten the whole screen and read as a blink. It only fades when
        // the modal layer itself appears or disappears.
        let scrim = if closing.is_some() && !matches!(screen, Screen::Home) {
            1.0
        } else {
            m.max(closing.map_or(0.0, |(alpha, _)| alpha))
        };
        if scrim > 0.0 {
            cmds.push(DrawCmd::Fill {
                rect: Rect::new(0, 0, screen_w, screen_h),
                color: crate::ui::render::Color::RGBA(0, 0, 0, (f32::from(ui::style::theme().scrim.a) * scrim) as u8),
            });
        }
        if !matches!(screen, Screen::Home) {
            let dy = ((1.0 - m) * MODAL_RISE) as i32;
            // The tile now covers only the card region (see `prepare_modal`), so it
            // composites there rather than full-screen. Opening plays the same motion
            // `compose_modal`'s closing snapshot uses below, in reverse — fade + rise, no
            // scale.
            let modal_base = self.modal_tile_region.offset(0, dy);
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
                let scroll_px = self
                    .modal_scroll_px
                    .clamp(0, Self::max_scroll_px(total, stride, content.height()));
                if let Some((src, dst)) = self.scroll_src_rect(screen, screen_w, screen_h, fonts) {
                    cmds.push(DrawCmd::TexCropped {
                        tile: tile::SCROLL_CONTENT,
                        src,
                        dst: dst.offset(0, dy),
                        alpha: (255.0 * m) as u8,
                    });
                }
                // Bottom fade, only while rows remain below the viewport — it is the
                // "there is more" signal, so it has to vanish exactly when scrolling has
                // reached the end, or it reads as content that can never be got to.
                //
                // Pushed here, between the content and the focused-row tile below, on
                // purpose: focus must never look dimmed just because it sits on the last
                // visible row, and an open dropdown (pushed next) must cover the band
                // rather than show through it.
                // Keyed off pixels, not rows: at either end of the list the offset is clamped
                // mid-row, so a row-based test would keep claiming there is more beyond.
                let fade_h = ui::widgets::SCROLL_FADE_H.min(content.height());
                if scroll_px > 0 {
                    cmds.push(DrawCmd::Tex {
                        tile: tile::SCROLL_FADE_TOP,
                        dst: Rect::new(content.x(), content.y() + dy, content.width(), fade_h),
                        alpha: (255.0 * m) as u8,
                    });
                }
                if scroll_px < Self::max_scroll_px(total, stride, content.height()) {
                    cmds.push(DrawCmd::Tex {
                        tile: tile::SCROLL_FADE,
                        dst: Rect::new(
                            content.x(),
                            content.y() + dy + (content.height() - fade_h) as i32,
                            content.width(),
                            fade_h,
                        ),
                        alpha: (255.0 * m) as u8,
                    });
                }
            }
            // Dropdown overlay (Settings or Diagnostics).
            if let Some((row, _, dd_alpha)) = self.dropdown_draw_state() {
                if let Some((content, scroll_px)) = self.dropdown_geom(screen_w, screen_h, fonts) {
                    let overlay_rect = view::settings::dropdown_overlay_rect_at_px(content, row, scroll_px);
                    let options_len = self.dropdown_options_len(row);
                    cmds.push(DrawCmd::Tex {
                        tile: tile::DROPDOWN_OVERLAY,
                        dst: Rect::new(
                            overlay_rect.x(),
                            overlay_rect.y() + dy,
                            overlay_rect.width(),
                            options_len as u32 * ui::widgets::DROPDOWN_OPTION_H,
                        ),
                        alpha: (255.0 * m * dd_alpha) as u8,
                    });
                }
            }
            // Focused widget of the active modal (setting row, button, etc.);
            // composites on shell at its on-screen position (no re-rasterize on move).
            // The entering screen's only — the snapshot has its own focused row baked in.
            let focus_rect = match screen {
                Screen::Settings => {
                    let (total, _, _, content) = scroll_geom.expect("screen is Screen::Settings");
                    // Positioned from the animated pixel offset, not the row index: the baked
                    // list is cropped at that offset, and the focus tile *is* the focused row
                    // re-rendered — so anchoring it to the quantized row would show that row's
                    // content twice, in two places, for the length of every scroll.
                    let stride = ui::widgets::focus_row_stride() as i32;
                    let px = self
                        .modal_scroll_px
                        .clamp(0, Self::max_scroll_px(total, stride, content.height()));
                    Some(ui::widgets::focus_row_rect_at_px(content, self.settings_focused, px))
                }
                // Every two-button confirm dialog: one subtitle drives the card, so one
                // button-row geometry serves all four.
                Screen::Wake | Screen::ForgetHost | Screen::SendLogs | Screen::SpeedTest => self
                    .confirm_subtitle()
                    .zip(self.confirm_focused())
                    .map(|(subtitle, i)| Self::confirm_focus_button_rect(screen_w, screen_h, fonts, &subtitle, i)),
                Screen::Pairing => {
                    let card = view::pairing::card_rect(screen_w, screen_h, fonts);
                    Some(match self.pairing_focus {
                        PairingFocus::Pin => {
                            let digit_y = view::pairing::pin_row_y(card, fonts);
                            view::pairing::digit_rect(card, digit_y, self.pin_digit_index)
                        }
                        PairingFocus::RequestAccess => view::pairing::request_button_rect(card, fonts),
                    })
                }
                // Every plain list modal: one geometry, measured off the `ModalScreen`
                // the painter draws, indexed by that screen's own focus cursor.
                Screen::HostMenu
                | Screen::WakeSettings
                | Screen::Diagnostics
                | Screen::Experimental
                | Screen::CursorSettings => self.list_modal_focus_rect(screen_w, screen_h, fonts),
                Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => None,
            };
            if let Some(rect) = focus_rect {
                let pad = ui::tiles::ROW_TILE_PAD;
                let base = rect.inflate(pad).offset(0, dy);
                // The zoom-in: same GPU-scale-around-center technique as the
                // grid's card focus pop (see above) — `modal_focus_tile` is
                // rasterized once at its literal size, never re-rendered for
                // this (except while `switch_anim` animates its content, see
                // `prepare_tiles`).
                let f = ui::animation::anim_frac(self.modal_focus_anim, ui::animation::FOCUS_POP);
                let dst = ui::animation::zoom_rect(base, f, 0.02);
                let alpha = (255.0 * m) as u8;
                // In a scrolling modal the focused row can hang past the viewport's bottom
                // edge mid-glide (the crop lags the row offset by up to one stride), so it is
                // clipped rather than left to paint over the card's chrome. Every other modal
                // keeps the plain unclipped path — none of them scrolls.
                let tile_size = tiles.get(tile::MODAL_FOCUS).map(|p| (p.width(), p.height()));
                match (scroll_geom, tile_size) {
                    (Some((_, _, _, content)), Some((tw, th))) => {
                        let viewport = content.inflate(pad).offset(0, dy);
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
            if let Some((row, focused, dd_alpha)) = self.dropdown_draw_state() {
                if let Some((content, scroll_px)) = self.dropdown_geom(screen_w, screen_h, fonts) {
                    let overlay_rect = view::settings::dropdown_overlay_rect_at_px(content, row, scroll_px);
                    let option_rect = ui::widgets::dropdown_option_rect(overlay_rect, focused);
                    cmds.push(DrawCmd::Tex {
                        tile: tile::DROPDOWN_FOCUS,
                        dst: Rect::new(
                            option_rect.x(),
                            option_rect.y() + dy,
                            option_rect.width(),
                            option_rect.height(),
                        ),
                        alpha: (255.0 * m * dd_alpha) as u8,
                    });
                }
            }
            // Whichever modal is scrollable, its indicator — full opacity for
            // `SCROLL_INDICATOR_HOLD`, then a linear fade over `SCROLL_INDICATOR_FADE`
            // (names kept from when only Settings had one; every scrollable modal now
            // shares the same timing and the same `self.scroll.shown_at` clock, since
            // only one is ever open at a time).
            if let Some((total, visible, card, content)) = scroll_geom {
                if total > visible {
                    let scroll_alpha = self.scroll.shown_at.map_or(0.0, |t| {
                        let elapsed = t.elapsed();
                        if elapsed < SCROLL_INDICATOR_HOLD {
                            1.0
                        } else {
                            let fading = (elapsed - SCROLL_INDICATOR_HOLD).as_secs_f32();
                            1.0 - (fading / SCROLL_INDICATOR_FADE.as_secs_f32()).clamp(0.0, 1.0)
                        }
                    });
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
        // Last, so it fades away *over* what it uncovers: the entering card is often the
        // larger (a submenu returning to Settings) and would otherwise hide it entirely.
        if let Some((alpha, prev)) = closing {
            let dy = ((1.0 - alpha) * MODAL_RISE) as i32;
            let a = (255.0 * alpha) as u8;
            cmds.push(DrawCmd::Tex {
                tile: tile::MODAL_PREV,
                dst: prev.region.offset(0, dy),
                alpha: a,
            });
            if let Some((src, dst)) = prev.content {
                cmds.push(DrawCmd::TexCropped {
                    tile: tile::MODAL_PREV_CONTENT,
                    src,
                    dst: dst.offset(0, dy),
                    alpha: a,
                });
            }
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
            let pad = ui::tiles::ROW_TILE_PAD;
            cmds.push(DrawCmd::Tex {
                tile: tile::FOCUS_ROW,
                dst: rect.inflate(pad),
                alpha: 0xff,
            });
        }
    }

    /// Grid family compose: the card tiles at their scrolled positions, the pinned
    /// divider, and the focused card with its ring/title-strip/pin-badge pop. Only reached
    /// once the grid is revealed. Extracted from `draw_list` (A2 staging).
    #[allow(clippy::too_many_arguments)]
    fn compose_grid(
        &self,
        screen_h: u32,
        grid_x: i32,
        available_w: u32,
        columns: usize,
        tiles: &TileStore,
        cmds: &mut Vec<DrawCmd>,
    ) {
        let count = self.grid_len(columns);
        let focused = match self.home_focus {
            HomeFocus::Grid(i) if i < count => Some(i),
            HomeFocus::Grid(_) | HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_) => None,
        };
        let pad = ui::tiles::CARD_SHADOW_PAD;
        let layout = self.grid_layout(columns);
        for idx in 0..count {
            if Some(idx) == focused {
                continue; // drawn last, on top of its neighbors
            }
            // padding after a partial pinned row — nothing to draw
            let Some(pin_id) = layout.pin_id_at(&self.games, idx) else {
                continue;
            };
            let r = self.scrolled_card_rect(idx, columns, grid_x, available_w);
            if r.bottom() + pad < 0 || r.y() - pad > screen_h as i32 {
                continue; // culled — fully off-screen at this scroll offset
            }
            let Some(card) = self.card_ids.get(pin_id) else {
                continue; // not rasterized yet — outside the build window
            };
            // A card that just landed is still zooming up to full size.
            let pop = self.card_pop_frac(pin_id);
            let alpha = (255.0 * pop) as u8;
            cmds.push(DrawCmd::Tex {
                tile: tile::CARD_SHADOW,
                dst: ui::animation::pop_in_rect(r.inflate(pad), pop, CARD_POP_SHRINK),
                alpha,
            });
            cmds.push(DrawCmd::Tex {
                tile: card,
                dst: ui::animation::pop_in_rect(r, pop, CARD_POP_SHRINK),
                alpha,
            });
        }
        // The divider between pinned games and the rest — scrolled with
        // everything else (there's no separate fixed region), so it's just
        // another rect at its own scrolled position, culled the same way.
        if let Some(sep) = self.pinned_separator_rect(columns, grid_x, available_w) {
            if sep.y() >= 0 && sep.y() <= screen_h as i32 {
                cmds.push(DrawCmd::Fill {
                    rect: sep,
                    color: crate::ui::render::Color::RGBA(0xff, 0xff, 0xff, 0x20),
                });
            }
        }
        if let Some(idx) = focused {
            if let Some(pin_id) = layout.pin_id_at(&self.games, idx) {
                // The focus pop: the GPU scales the (unfocused) card tile up
                // around its center as the pop progresses, with the shared glow
                // tile fading in behind it at the same scale.
                let f = ui::animation::anim_frac_smooth(self.focus_anim, ui::animation::CARD_FOCUS_POP);
                let r = self.scrolled_card_rect(idx, columns, grid_x, available_w);
                let Some(card) = self.card_ids.get(pin_id) else {
                    return; // not rasterized yet
                };
                let pop = self.card_pop_frac(pin_id);
                let popped = |base: Rect| ui::animation::pop_in_rect(base, pop, CARD_POP_SHRINK);
                // The card's total scale, for anything composited on top of it that has to
                // fold in the same transform about the card's centre rather than its own.
                let card_scale =
                    ui::animation::zoom_scale(f, CARD_GROWTH) * ui::animation::pop_in_scale(pop, CARD_POP_SHRINK);
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
                // The title strip wipes up the card's bottom edge — a wipe, not a slide:
                // the tile's bottom `shown` rows go to the card's bottom `shown` rows, so
                // the frosted art baked into it stays registered with the art beneath.
                // Sliding the whole tile dragged that baked cover fragment up with it,
                // reading as the card shifting under the glass. `r` is the pivot: a band
                // scaled about its own center drifts off the edge it sits flush to.
                if let Some(strip_h) = tiles.get(tile::CARD_TITLE).map(ui::Painter::height) {
                    let wipe = ui::animation::anim_frac(self.focus_anim, ui::animation::CARD_FOCUS_POP);
                    let shown = (strip_h as f32 * wipe) as u32;
                    if shown > 0 {
                        let visible = Rect::new(r.x(), r.bottom() - shown as i32, r.width(), shown);
                        cmds.push(DrawCmd::TexCropped {
                            tile: tile::CARD_TITLE,
                            src: Rect::new(0, (strip_h - shown) as i32, r.width(), shown),
                            dst: ui::animation::scale_about(visible, r, card_scale),
                            alpha: (255.0 * pop) as u8,
                        });
                    }
                }
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
                if self.selected_known_host().is_some_and(|h| h.is_pinned(pin_id)) {
                    let badge = ui::tiles::PIN_BADGE_SIZE;
                    let badge_base = Rect::new(
                        r.right() - badge as i32 - PIN_BADGE_MARGIN,
                        r.y() + PIN_BADGE_MARGIN,
                        badge,
                        badge,
                    );
                    // Corner-anchored, so it only fades — scaling it around its
                    // own center would drift it off the shrunken card.
                    cmds.push(DrawCmd::Tex {
                        tile: tile::PIN_BADGE,
                        dst: ui::animation::zoom_rect(badge_base, f, CARD_GROWTH),
                        alpha: (255.0 * pop) as u8,
                    });
                }
            }
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

        cmds.push(DrawCmd::Tex {
            tile: tile::SIDEBAR,
            dst: Rect::new(0, 0, ui::widgets::SIDEBAR_W, screen_h),
            alpha: 0xff,
        });

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
            let phase = self.spinner_since.map_or(0.0, |s| s.elapsed().as_secs_f32());
            let (idx, frame) = crate::assets::spinner_frame_at(phase);
            let x = grid_x + (available_w as i32 - frame.width as i32) / 2;
            // 40% down rather than dead-center, which reads as slightly low on a TV.
            let area_h = screen_h as i32 - view::home::GRID_TOP_Y;
            let y = view::home::GRID_TOP_Y + (area_h - frame.height as i32) * 2 / 5;
            cmds.push(DrawCmd::Tex {
                tile: tile::spinner(idx),
                dst: Rect::new(x, y, frame.width, frame.height),
                alpha: 0xff,
            });
        } else {
            self.compose_grid(screen_h, grid_x, available_w, columns, tiles, &mut cmds);
        }
        if input.has_status {
            if let Some(p) = tiles.get(tile::STATUS) {
                let line_h = fonts.raster.height(fonts.label) + 6;
                let box_h = 2 * line_h as u32 + 2 * STATUS_BG_PAD as u32;
                let box_y = screen_h as i32 - box_h as i32;
                cmds.push(DrawCmd::Fill {
                    rect: Rect::new(grid_x, box_y, available_w, box_h),
                    color: ui::style::theme().scrim,
                });
                let y = box_y + (box_h as i32 - p.height() as i32) / 2;
                cmds.push(DrawCmd::Tex {
                    tile: tile::STATUS,
                    dst: Rect::new(grid_x + view::home::GRID_PAD, y, p.width(), p.height()),
                    alpha: 0xff,
                });
            }
        }

        Self::compose_sidebar_focus(&input, screen_h, &mut cmds);

        self.compose_modal(tiles, screen_w, screen_h, fonts, &mut cmds);
        // The launch transition: the confirmed card zooms in around its own
        // center (same `zoom_rect` technique as the focus pop, so its aspect
        // ratio never changes) while a black scrim blends in over it, both driven
        // by the same clock — the card keeps zooming for the whole fade.
        if let (Some(t), Some(idx)) = (self.launch_anim, self.launch_anim_idx) {
            let f = ui::animation::anim_frac(Some(t), hero::LAUNCH_FADE);
            let base = self.scrolled_card_rect(idx, columns, grid_x, available_w);
            if let Some(card) = self
                .pin_id_at_grid_idx(idx, columns)
                .and_then(|pin_id| self.card_ids.get(pin_id))
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
        let Some(hero) = self.hero.visible() else { return };
        let f = self.hero.opacity();
        cmds.push(DrawCmd::TexF {
            tile: tile::HERO,
            dst: hero::hero_pan_dst(hero.width, hero.height, screen_w, screen_h, self.hero.panned_for()),
            alpha: (255.0 * f) as u8,
        });
        cmds.push(DrawCmd::Fill {
            rect: Rect::new(0, 0, screen_w, screen_h),
            color: crate::ui::render::Color::RGBA(0, 0, 0, (hero::HERO_SCRIM_ALPHA * f) as u8),
        });
    }
}
