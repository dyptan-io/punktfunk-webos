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
        // While closing, `self.screen` has already moved on — render the fade's
        // captured screen instead, so the still-uploaded tiles keep drawing for one
        // more `MODAL_FADE` with alpha running in reverse (see `ui::fade::ModalFade`).
        let closing_frame = self.modal_fade.closing_frame(MODAL_FADE);
        let (screen, m) = match closing_frame {
            Some((alpha, s)) => (s, alpha),
            None => (self.screen, self.modal_fade.open_alpha(MODAL_FADE)),
        };
        if !matches!(screen, Screen::Home) {
            cmds.push(DrawCmd::Fill {
                rect: Rect::new(0, 0, screen_w, screen_h),
                color: crate::ui::render::Color::RGBA(0, 0, 0, (f32::from(ui::style::theme().scrim.a) * m) as u8),
            });
            let dy = ((1.0 - m) * 26.0) as i32;
            // The tile now covers only the card region (see `prepare_modal`), so it
            // composites there rather than full-screen. `pop_in_rect` scaling around this
            // rect's center is the card's own center — the same visual pop as before.
            let modal_base = self.modal_tile_region.offset(0, dy);
            let modal_dst = if closing_frame.is_some() {
                modal_base
            } else {
                ui::animation::pop_in_rect(modal_base, m, MODAL_POP_SHRINK)
            };
            cmds.push(DrawCmd::Tex {
                tile: tile::MODAL,
                dst: modal_dst,
                alpha: (255.0 * m) as u8,
            });
            // Scrollable content geometry (Settings rows or About document), computed
            // once and reused. Scrolling crops the full baked tile, never re-rasterizes.
            let scroll_geom = self.scroll_geometry_for(screen, screen_w, screen_h, fonts);
            if let Some((total, _, _, content)) = scroll_geom {
                // About uses a bounded window; for other screens, window_start is 0.
                let window_start = match screen {
                    Screen::About => self.content_window.start,
                    _ => 0,
                };
                let stride = self.scroll_stride_for(screen, fonts);
                // The animated offset (see `sync_modal_scroll`), in absolute content pixels,
                // rebased onto whatever slice is currently baked into the tile.
                let scroll_px = self
                    .modal_scroll_px
                    .clamp(0, Self::max_scroll_px(total, stride, content.height()));
                let src_y = scroll_px - window_start as i32 * stride;
                cmds.push(DrawCmd::TexCropped {
                    tile: tile::SCROLL_CONTENT,
                    src: Rect::new(0, src_y, content.width(), content.height()),
                    dst: content.offset(0, dy),
                    alpha: (255.0 * m) as u8,
                });
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
                if let Some((content, scroll_px)) = self.dropdown_geom(screen, screen_w, screen_h, fonts) {
                    let overlay_rect = view::settings::dropdown_overlay_rect_at_px(content, row, scroll_px);
                    let options_len = match screen {
                        Screen::Diagnostics => menu::LOG_LEVEL_OPTIONS.len(),
                        _ => menu::dropdown_option_count(menu::settings_logical_row(&self.settings, row)),
                    };
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
            //
            // Skipped entirely once the modal is closing: the position is recomputed
            // from live per-screen state, which Back may have already torn down (e.g.
            // `host_menu_index` cleared, collapsing the host-menu card to the screen
            // centre and floating the highlight there). The shell and scroll-content
            // tiles still render the focused row through the fade, so dropping just the
            // zoom-highlight overlay is invisible — and correct.
            let focus_rect = if closing_frame.is_some() {
                None
            } else {
                match screen {
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
                    Screen::Wake => self.wake.as_ref().filter(|w| !w.mac.is_empty()).map(|w| {
                        Self::confirm_focus_button_rect(
                            screen_w,
                            screen_h,
                            fonts,
                            &view::wake::status_text(w),
                            w.focused,
                        )
                    }),
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
                    Screen::ForgetHost => {
                        let name = self
                            .host_menu_index
                            .and_then(|i| self.entries.get(i))
                            .map(HostEntry::name)
                            .unwrap_or_default();
                        Some(Self::confirm_focus_button_rect(
                            screen_w,
                            screen_h,
                            fonts,
                            &view::forget::subtitle(name),
                            self.host_menu_focused,
                        ))
                    }
                    Screen::HostMenu => {
                        let subtitle = self.host_menu_subtitle();
                        let rows = self.host_menu_actions().len();
                        let card = view::hostmenu::card_rect(screen_w, screen_h, fonts, &subtitle, rows);
                        let content = ui::widgets::list_modal_content_rect(card, fonts, &subtitle, rows);
                        Some(ui::widgets::focus_row_rect(content, self.menu_focused))
                    }
                    Screen::WakeSettings => {
                        let card = view::wakesettings::card_rect(screen_w, screen_h, fonts);
                        let content = ui::widgets::list_modal_content_rect(
                            card,
                            fonts,
                            view::wakesettings::SUBTITLE,
                            menu::DIAGNOSTICS_ROW_COUNT,
                        );
                        Some(ui::widgets::focus_row_rect(content, self.wake_settings_focused))
                    }
                    Screen::SpeedTest => view::speedtest::finished(self.speed_test.as_ref()).then(|| {
                        let card = view::speedtest::card_rect(
                            screen_w,
                            screen_h,
                            fonts,
                            self.speed_test.as_ref(),
                            &self.speed_test_name,
                        );
                        ui::widgets::confirm_button_rect(
                            view::speedtest::buttons_rect(card, fonts, self.speed_test.as_ref(), &self.speed_test_name),
                            self.speed_test_focused,
                        )
                    }),
                    Screen::Diagnostics => {
                        let card = view::diagnostics::card_rect(screen_w, screen_h, fonts);
                        let content = ui::widgets::list_modal_content_rect(
                            card,
                            fonts,
                            view::diagnostics::SUBTITLE,
                            menu::DIAGNOSTICS_ROW_COUNT,
                        );
                        Some(ui::widgets::focus_row_rect(content, self.diagnostics_focused))
                    }
                    Screen::Experimental => {
                        let rows = view::experimental::row_count(Self::rooted());
                        let card = view::experimental::card_rect(screen_w, screen_h, fonts, Self::rooted());
                        let content =
                            ui::widgets::list_modal_content_rect(card, fonts, view::experimental::SUBTITLE, rows);
                        Some(ui::widgets::focus_row_rect(content, self.experimental_focused))
                    }
                    Screen::CursorSettings => {
                        let card = view::cursorsettings::card_rect(screen_w, screen_h, fonts);
                        let content = ui::widgets::list_modal_content_rect(
                            card,
                            fonts,
                            view::cursorsettings::SUBTITLE,
                            menu::CURSOR_ROW_COUNT,
                        );
                        Some(ui::widgets::focus_row_rect(content, self.cursor_settings_focused))
                    }
                    Screen::SendLogs => Some(Self::confirm_focus_button_rect(
                        screen_w,
                        screen_h,
                        fonts,
                        view::sendlogs::SUBTITLE,
                        self.send_logs_focused,
                    )),
                    Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => None,
                }
            };
            if let Some(rect) = focus_rect {
                let pad = ui::tiles::ROW_TILE_PAD;
                let base = Rect::new(
                    rect.x() - pad,
                    rect.y() - pad + dy,
                    rect.width() + 2 * pad as u32,
                    rect.height() + 2 * pad as u32,
                );
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
                        let viewport = Rect::new(
                            content.x() - pad,
                            content.y() - pad + dy,
                            content.width() + 2 * pad as u32,
                            content.height() + 2 * pad as u32,
                        );
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
                if let Some((content, scroll_px)) = self.dropdown_geom(screen, screen_w, screen_h, fonts) {
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
                dst: Rect::new(
                    rect.x() - pad,
                    rect.y() - pad,
                    rect.width() + 2 * pad as u32,
                    rect.height() + 2 * pad as u32,
                ),
                alpha: 0xff,
            });
        }
    }

    /// Grid family compose: the card tiles at their scrolled positions, the pinned
    /// divider, and the focused card with its ring/outline/pin-badge pop. Only reached
    /// once the grid is revealed. Extracted from `draw_list` (A2 staging).
    #[allow(clippy::too_many_arguments)]
    fn compose_grid(&self, screen_h: u32, grid_x: i32, available_w: u32, columns: usize, cmds: &mut Vec<DrawCmd>) {
        let count = self.grid_len(columns);
        let focused = match self.home_focus {
            HomeFocus::Grid(i) if i < count => Some(i),
            HomeFocus::Grid(_) | HomeFocus::Sidebar(_) | HomeFocus::SidebarMenu(_) => None,
        };
        let pad = ui::tiles::CARD_TILE_PAD;
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
            let base = Rect::new(
                r.x() - pad,
                r.y() - pad,
                r.width() + 2 * pad as u32,
                r.height() + 2 * pad as u32,
            );
            cmds.push(DrawCmd::Tex {
                tile: card,
                dst: ui::animation::pop_in_rect(base, pop, CARD_POP_SHRINK),
                alpha: (255.0 * pop) as u8,
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
                let f = ui::animation::anim_frac(self.focus_anim, ui::animation::FOCUS_POP);
                let r = self.scrolled_card_rect(idx, columns, grid_x, available_w);
                let card_base = Rect::new(
                    r.x() - pad,
                    r.y() - pad,
                    r.width() + 2 * pad as u32,
                    r.height() + 2 * pad as u32,
                );
                let Some(card) = self.card_ids.get(pin_id) else {
                    return; // not rasterized yet
                };
                let pop = self.card_pop_frac(pin_id);
                let popped = |base: Rect| ui::animation::pop_in_rect(base, pop, CARD_POP_SHRINK);
                // Glow drawn first — it's a halo behind the card, not an outline
                // on top of it.
                let rp = ui::tiles::FOCUS_RING_PAD;
                let ring_base = Rect::new(
                    r.x() - rp,
                    r.y() - rp,
                    r.width() + 2 * rp as u32,
                    r.height() + 2 * rp as u32,
                );
                cmds.push(DrawCmd::Tex {
                    tile: tile::RING,
                    dst: popped(ui::animation::zoom_rect(ring_base, f, CARD_GROWTH)),
                    alpha: (255.0 * f * pop) as u8,
                });
                // The focused card zooms in on first appearance like any other,
                // composed with its focus pop — both scale around the card's own
                // center, so they can't fight over position.
                cmds.push(DrawCmd::Tex {
                    tile: card,
                    dst: popped(ui::animation::zoom_rect(card_base, f, CARD_GROWTH)),
                    alpha: (255.0 * pop) as u8,
                });
                // The crisp outline, on top of the card art — a clean edge
                // between it and the glow behind, unlike the glow's own
                // soft, blurred boundary.
                let op = ui::tiles::CARD_OUTLINE_PAD;
                let outline_base = Rect::new(
                    r.x() - op,
                    r.y() - op,
                    r.width() + 2 * op as u32,
                    r.height() + 2 * op as u32,
                );
                cmds.push(DrawCmd::Tex {
                    tile: tile::CARD_OUTLINE,
                    dst: popped(ui::animation::zoom_rect(outline_base, f, CARD_GROWTH)),
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
            self.compose_grid(screen_h, grid_x, available_w, columns, &mut cmds);
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
