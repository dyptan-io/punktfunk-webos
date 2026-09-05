//! Composition: this frame's draw list, in paint order.
//!
//! The GPU half — pure bookkeeping over already-rasterized tiles. Position, scroll, every
//! focus pop and every fade is a texture-copy parameter here, never a re-raster (see
//! `platform::webos::compositor`). Split out of `app/mod.rs` alongside `prepare`.
use crate::app::render::tile;
use crate::app::render::SnapshotBody;
use crate::app::screens;
use crate::app::{App, Screen, SCROLL_INDICATOR_TILE_W};
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
        // m is cross-fade's leaving card inverse (see ModalFade). The fade says whether a
        // card is leaving; `prev` says whether there are pixels of it to draw — a card drawn
        // on the kit fades out from state instead (`App::draw_modals`).
        let leaving = self.render.modal.fade.closing_frame_against(m);
        let closing = self
            .render
            .modal
            .prev
            .zip(leaving)
            .map(|(prev, (alpha, _))| (alpha, prev));
        // Backdrop is "modal up", not per-card; only fades when modal layer appears/disappears.
        let scrim = if leaving.is_some() && !matches!(screen, Screen::Home) {
            1.0
        } else {
            m.max(leaving.map_or(0.0, |(alpha, _)| alpha))
        };
        let tiled = !matches!(screen, Screen::Home) && !crate::app::draw::ported(screen);
        // Frost panes before scrim (compositor captures blur at first Frost; ordering matters
        // to avoid two-phase blur of dimmed screen when opening from sidebar vs card).
        // Invariant: nothing tinting whole screen before a frost pane.
        if tiled {
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
        if tiled {
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

    /// Builds this frame's draw list (paint order) for the tile-drawn modal still open, if
    /// any — pure bookkeeping, no rasterization. Home itself and the ported modals draw
    /// directly (`app::draw`).
    pub fn draw_list(
        &self,
        tiles: &TileStore,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> ui::render::DrawList {
        let mut cmds = Vec::new();
        self.compose_modal(tiles, screen_w, screen_h, fonts, &mut cmds);
        cmds
    }
}
