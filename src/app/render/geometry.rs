//! Geometry both render halves read: what scrolls, how far, and how a tile is cropped to
//! its viewport.
//!
//! Shared on purpose — `prepare`'s staleness checks and `compose`'s GPU-crop math have to
//! agree about a scrollable modal's extent, and deriving it twice is how they stop
//! agreeing.
use crate::app::{menu, view, App, PairingFocus, Screen, MODAL_TILE_PAD};
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::Painter;

impl App {
    /// `(total units, visible units, card rect, content/viewport rect)` for whichever
    /// scrollable modal is open — `None` if `self.screen` has no overflowing content.
    /// The one place this per-modal geometry lives, shared by `prepare_tiles`'s
    /// staleness checks and `draw_list`'s GPU-crop math so the two can't disagree.
    /// `About`'s `total` depends on `about_wrapped` already being fresh for this
    /// frame's body width — `prepare_tiles` ensures that before calling this;
    /// `draw_list` runs after `prepare_tiles` in the same frame, so it's already set.
    pub(crate) fn scroll_geometry(
        &self,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<(usize, usize, Rect, Rect)> {
        self.scroll_geometry_for(self.screen, screen_w, screen_h, fonts)
    }

    /// Same as `scroll_geometry`, but for an explicit screen — `snapshot_closing_modal`
    /// needs the screen being *left*, which `self.screen` has already moved off.
    pub(crate) fn scroll_geometry_for(
        &self,
        screen: Screen,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<(usize, usize, Rect, Rect)> {
        match screen {
            Screen::Settings(_) => {
                let set = self.settings_scope();
                let (card, content) = view::settings::layout(set, screen_w, screen_h);
                let visible = view::settings::visible_rows(set, screen_h);
                Some((menu::settings_row_count(set), visible, card, content))
            }
            Screen::About => {
                let card = view::about::card_rect(screen_w, screen_h);
                let body = view::about::body_rect(card, fonts);
                let total = self.about_wrapped.as_ref().map_or(0, |(_, v)| v.len());
                let visible = view::about::visible_lines(body, fonts.raster, fonts.value);
                Some((total, visible, card, body))
            }
            _ => None,
        }
    }

    /// Clips a tile's destination to `clip`, returning `(source crop, clipped destination)`.
    ///
    /// The tile's full extent is assumed to map onto `dst`, so the crop is proportional —
    /// which keeps this correct even while `dst` is being zoom-animated. `None` when nothing
    /// of it remains inside.
    pub(crate) fn clip_tile(dst: Rect, clip: Rect, tile_w: u32, tile_h: u32) -> Option<(Rect, Rect)> {
        let visible = dst.intersection(clip)?;
        if visible == dst {
            return Some((Rect::new(0, 0, tile_w, tile_h), dst));
        }
        if dst.width() == 0 || dst.height() == 0 {
            return None;
        }
        let fx = |v: i32| (f64::from(v) / f64::from(dst.width())) * f64::from(tile_w);
        let fy = |v: i32| (f64::from(v) / f64::from(dst.height())) * f64::from(tile_h);
        let src = Rect::new(
            fx(visible.x() - dst.x()).round() as i32,
            fy(visible.y() - dst.y()).round() as i32,
            (fx(visible.width() as i32).round() as u32).max(1),
            (fy(visible.height() as i32).round() as u32).max(1),
        );
        Some((src, visible))
    }

    /// The furthest the viewport may be cropped down: the last unit sits flush with the
    /// viewport's bottom edge rather than scrolling past it.
    ///
    /// This is why the rendered offset is pixels and not units — `offset * stride` overshoots
    /// by exactly the peek strip at the end of the list, which would show a dead band below
    /// the final row (and is what the row-quantized version did).
    pub(crate) fn max_scroll_px(total: usize, stride: i32, viewport_h: u32) -> i32 {
        (total as i32 * stride - viewport_h as i32).max(0)
    }

    /// The animated scroll offset, held inside the range this list can actually travel.
    ///
    /// `modal.scroll_px` is the raw target the ease writes; every reader wants it clamped, and
    /// the clamp used to be spelled out at each of them.
    pub(crate) fn clamped_scroll_px(&self, total: usize, stride: i32, viewport_h: u32) -> i32 {
        self.modal
            .scroll_px
            .clamp(0, Self::max_scroll_px(total, stride, viewport_h))
    }

    /// Which slice of `screen`'s baked `tile::SCROLL_CONTENT` is showing    /// Which slice of `screen`'s baked `tile::SCROLL_CONTENT` is showing, as `(src crop,
    /// dst rect)` — `None` for a screen whose body lives in its shell tile.
    ///
    /// The one place the window rebase lives: `compose_modal` draws the live modal with it,
    /// `snapshot_closing_modal` freezes the same crop for the fading one.
    pub(crate) fn scroll_src_rect(
        &self,
        screen: Screen,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<(Rect, Rect)> {
        let (total, _, _, content) = self.scroll_geometry_for(screen, screen_w, screen_h, fonts)?;
        // About uses a bounded window; for other screens, window_start is 0.
        let window_start = match screen {
            Screen::About => self.content_window.start,
            _ => 0,
        };
        let stride = self.scroll_stride_for(screen, fonts);
        // The animated offset (see `sync_modal_scroll`), in absolute content pixels,
        // rebased onto whatever slice is currently baked into the tile.
        let scroll_px = self.clamped_scroll_px(total, stride, content.height());
        let src = Rect::new(
            0,
            scroll_px - window_start as i32 * stride,
            content.width(),
            content.height(),
        );
        Some((src, content))
    }

    /// Re-derives `modal_scroll_target_px` from the integral offset, snapping rather than
    /// gliding when the scrolling modal changed. Called once per frame from `update_tiles`,
    /// which is where the geometry (and the fonts About's stride needs) is already in hand.
    ///
    /// Kept in absolute content pixels, *not* relative to the baked window: About re-bakes its
    /// window later in the same pass, and a window-relative target would jump by the whole
    /// window offset on the frame that happens — a full-document glide instead of a scroll.
    /// `draw_list` subtracts the window when it crops.
    pub(crate) fn sync_modal_scroll(
        &mut self,
        screen: Screen,
        total: usize,
        visible: usize,
        viewport_h: u32,
        stride: i32,
    ) {
        let offset = self.scroll.clamped(total, visible);
        // Biased back by one peek so the *top* edge also cuts mid-row: sitting on the row grid
        // would put nothing but the gap between rows under the top fade, which is invisible
        // (see `view::settings::PEEK`). The clamps then pin the first and last positions flush,
        // where there is genuinely nothing beyond the edge to hint at.
        let bias = match screen {
            Screen::Settings(_) => view::settings::PEEK as i32,
            _ => 0,
        };
        let target = (offset as i32 * stride - bias)
            .min(Self::max_scroll_px(total, stride, viewport_h))
            .max(0);
        self.modal.scroll_target_px = target;
        if self.modal.scroll_screen != Some(screen) {
            self.modal.scroll_screen = Some(screen);
            self.modal.scroll_px = target;
        }
    }

    /// Pixel stride between two consecutive units of whichever modal is scrolling —
    /// Settings' fixed row height, or About's wrapped-line height. Only meaningful
    /// when `scroll_geometry` returns `Some`.
    pub(crate) fn scroll_stride(&self, fonts: &ui::text::Fonts) -> i32 {
        self.scroll_stride_for(self.screen, fonts)
    }

    /// Same as `scroll_stride`, but for an explicit screen — see `scroll_geometry_for`.
    pub(crate) fn scroll_stride_for(&self, screen: Screen, fonts: &ui::text::Fonts) -> i32 {
        match screen {
            Screen::Settings(_) => ui::widgets::FOCUS_ROW_H as i32 + ui::widgets::FOCUS_ROW_GAP,
            Screen::About => view::about::line_stride(fonts.raster, fonts.value),
            _ => 1,
        }
    }

    /// Title and subtitle of the address form, which serves both Add host and Edit
    /// address — the only difference between the two screens.
    pub(crate) fn address_copy(&self) -> (&'static str, String) {
        match self.screen {
            Screen::EditHost => {
                let name = self
                    .edit_host_index
                    .and_then(|i| self.entries.get(i))
                    .map_or_else(String::new, |e| e.name().to_string());
                (view::addhost::EDIT_TITLE, view::addhost::edit_subtitle(&name))
            }
            _ => (view::addhost::ADD_TITLE, view::addhost::ADD_SUBTITLE.to_string()),
        }
    }

    /// The current screen's modal card rect, or `None` for a screen that draws no
    /// modal card (Home, or Wake before its payload is set). Measured off the same
    /// [`ui::ModalScreen`] value the renderer draws, so hover, click and the
    /// close-button hit-test can never drift from what is on screen.
    pub(crate) fn modal_card_rect(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<Rect> {
        self.with_modal_screen(|s| s.card_rect(screen_w, screen_h, fonts))
    }

    /// The open modal's row-list viewport, or an empty rect on a screen without one —
    /// for the tile builders, which only reach here on a screen that has one.
    pub(crate) fn modal_list_content(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Rect {
        self.modal_list_geometry(screen_w, screen_h, fonts)
            .map_or_else(|| Rect::new(0, 0, 0, 0), |(_, content)| content)
    }

    /// The open modal's card and row-list viewport, when it has one. The pair every hit
    /// test and focused-row tile needs, derived once from the screen that draws them.
    pub(crate) fn modal_list_geometry(
        &self,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<(Rect, Rect)> {
        self.with_modal_screen(|s| {
            let card = s.card_rect(screen_w, screen_h, fonts);
            s.content_rect(card, fonts).map(|content| (card, content))
        })?
    }

    /// The subtitle of the open two-button confirm modal — the string its card height and
    /// button-row rect are both measured from, so one value drives the whole dialog.
    ///
    /// `None` on any other screen, and on the two confirm screens whose buttons aren't up
    /// yet: a Wake with no MAC on record is a button-less message, and a speed test still
    /// running has nothing to apply.
    pub(crate) fn confirm_subtitle(&self) -> Option<String> {
        Some(match self.screen {
            Screen::ForgetHost => view::forget::subtitle(self.host_menu_host_name().unwrap_or_default()),
            Screen::SendLogs => view::sendlogs::SUBTITLE.to_string(),
            Screen::Wake => view::wake::status_text(self.wake.as_ref().filter(|w| !w.mac.is_empty())?),
            Screen::SpeedTest => {
                let state = self.speed_test.as_ref();
                view::speedtest::finished(state).then(|| view::speedtest::status(state, &self.speed_test_name))?
            }
            _ => return None,
        })
    }

    /// Which of the open confirm dialog's two buttons has focus. A field per screen so
    /// each remembers its own answer; `None` on a screen that isn't one.
    pub(crate) fn confirm_focused(&self) -> Option<usize> {
        Some(match self.screen {
            Screen::ForgetHost => self.host_menu_focused,
            Screen::SendLogs => self.send_logs_focused,
            Screen::Wake => self.wake.as_ref()?.focused,
            Screen::SpeedTest => self.speed_test_focused,
            _ => return None,
        })
    }

    /// Moves the open confirm dialog's focus onto button `index`, reporting whether it
    /// actually moved — the hover/click contract every focus setter here follows.
    pub(crate) fn set_confirm_focused(&mut self, index: usize) -> bool {
        let Some(focused) = (match self.screen {
            Screen::ForgetHost => Some(&mut self.host_menu_focused),
            Screen::SendLogs => Some(&mut self.send_logs_focused),
            Screen::Wake => self.wake.as_mut().map(|w| &mut w.focused),
            Screen::SpeedTest => Some(&mut self.speed_test_focused),
            _ => None,
        }) else {
            return false;
        };
        let changed = *focused != index;
        *focused = index;
        changed
    }

    /// Rect of the focused widget of whichever modal `screen` is — the setting row, the
    /// confirm button, the pairing digit, the list row. This is what `tile::MODAL_FOCUS`
    /// composites at, and what the press dip scales; `None` for the screens whose focus
    /// is baked into their shell (Home's grid has its own pop).
    pub(crate) fn modal_focus_rect(
        &self,
        screen: Screen,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> Option<Rect> {
        match screen {
            Screen::Settings(_) => {
                let (total, _, _, content) = self.scroll_geometry_for(screen, screen_w, screen_h, fonts)?;
                // Positioned from the animated pixel offset, not the row index: the baked
                // list is cropped at that offset, and the focus tile *is* the focused row
                // re-rendered — so anchoring it to the quantized row would show that row's
                // content twice, in two places, for the length of every scroll.
                let stride = ui::widgets::focus_row_stride() as i32;
                let px = self.clamped_scroll_px(total, stride, content.height());
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
            | Screen::CursorSettings(_) => self.list_modal_focus_rect(screen_w, screen_h, fonts),
            Screen::Home | Screen::AddHost | Screen::EditHost | Screen::About | Screen::PinLimit => None,
        }
    }

    /// The open list modal's focused row index — the cursor `focus_row_rect` indexes with.
    /// One field per screen so a nested menu keeps its place on the way back; `None` on a
    /// screen that has no plain row list (Settings scrolls, and owns its own geometry).
    pub(crate) fn list_modal_focused(&self) -> Option<usize> {
        Some(match self.screen {
            Screen::HostMenu => self.menu_focused,
            Screen::WakeSettings => self.wake_settings_focused,
            Screen::Diagnostics => self.diagnostics_focused,
            Screen::Experimental => self.experimental_focused,
            Screen::CursorSettings(_) => self.cursor_settings_focused,
            _ => return None,
        })
    }

    /// [`list_modal_focused`](Self::list_modal_focused)'s field itself, for the pointer's
    /// click-moves-focus rule — same table, so the two can't name different fields.
    pub(crate) fn list_modal_focused_mut(&mut self) -> Option<&mut usize> {
        Some(match self.screen {
            Screen::HostMenu => &mut self.menu_focused,
            Screen::WakeSettings => &mut self.wake_settings_focused,
            Screen::Diagnostics => &mut self.diagnostics_focused,
            Screen::Experimental => &mut self.experimental_focused,
            Screen::CursorSettings(_) => &mut self.cursor_settings_focused,
            _ => return None,
        })
    }

    /// The open list modal's focused-row rect, positioned on screen. `None` unless the
    /// current screen is one of the plain list modals.
    pub(crate) fn list_modal_focus_rect(&self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Option<Rect> {
        let (_, content) = self.modal_list_geometry(screen_w, screen_h, fonts)?;
        Some(ui::widgets::focus_row_rect(content, self.list_modal_focused()?))
    }

    /// How many options the open dropdown lists. Read by both the overlay's drawn height
    /// and its hit test, so the two can't disagree about where the last option ends.
    pub(crate) fn dropdown_options_len(&self, row: usize) -> usize {
        match self.screen {
            Screen::Diagnostics => menu::LOG_LEVEL_OPTIONS.len(),
            _ => menu::dropdown_option_count(menu::settings_logical_row(self.settings_scope(), row)),
        }
    }

    /// Calls `f` with the open modal as a [`ui::ModalScreen`], built from the state it
    /// shows. `None` on Home, and on a screen whose payload isn't set yet (Wake before its
    /// host is known).
    ///
    /// By closure rather than by return value: the hit tests below run this on every Magic
    /// Remote `MouseMotion`, and a returned `Box<dyn ModalScreen>` would put a heap
    /// allocation on that path for what is a geometry query.
    pub(crate) fn with_modal_screen<R>(&self, f: impl FnOnce(&dyn ui::ModalScreen) -> R) -> Option<R> {
        Some(match self.screen {
            Screen::Home => return None,
            // One screen, two scopes: the dim title suffix is the only thing the per-game
            // one adds, and it comes from the scratch copy that scope implies.
            Screen::Settings(set) => f(&view::settings::Modal {
                set,
                game: self.editing_game().map(|gs| gs.title.as_str()),
            }),
            Screen::Pairing => f(&view::pairing::Modal {
                pin_digits: &self.pin_digits,
                status: self.pairing_status.as_ref(),
                busy: self.pairing_busy,
            }),
            Screen::AddHost | Screen::EditHost => {
                let (title, subtitle) = self.address_copy();
                f(&view::addhost::Modal {
                    title,
                    subtitle,
                    typed: self.add_host.text(),
                    keyboard_shown: self.keyboard_shown,
                })
            }
            Screen::Wake => f(&view::wake::Modal {
                wake: self.wake.as_ref()?,
            }),
            Screen::ForgetHost => f(&view::forget::Modal {
                host_name: self.host_menu_host_name().unwrap_or_default(),
            }),
            Screen::HostMenu => f(&view::hostmenu::Modal {
                title: self.host_menu_host_name().unwrap_or_default(),
                subtitle: self.host_menu_subtitle(),
                rows: self.host_menu_rows(),
            }),
            Screen::WakeSettings => f(&view::wakesettings::Modal {
                host_name: self.host_menu_host_name().unwrap_or_default(),
                auto_send: self.wake_settings_host().is_some_and(|h| h.wol_auto),
            }),
            Screen::About => f(&view::about::Modal),
            Screen::SpeedTest => f(&view::speedtest::Modal {
                state: self.speed_test.as_ref(),
                host_name: &self.speed_test_name,
            }),
            Screen::PinLimit => f(&view::pinlimit::Modal {
                message: Self::PIN_LIMIT_MESSAGE,
            }),
            Screen::Diagnostics => f(&view::diagnostics::Modal {
                settings: &self.settings,
            }),
            Screen::Experimental => f(&view::experimental::Modal {
                settings: &self.settings,
                rooted: self.rooted,
            }),
            Screen::CursorSettings(_) => f(&view::cursorsettings::Modal {
                settings: self.settings_target(),
                over: &self.editing_override(),
            }),
            Screen::SendLogs => f(&view::sendlogs::Modal),
        })
    }

    /// A painter for the current screen's modal, sized and positioned to its *tile*
    /// region — the card rect grown by [`MODAL_TILE_PAD`] for the shadow — rather than
    /// to the whole screen. Records the region in `modal_tile_region`, which is where
    /// `compose_modal` composites the tile. Falls back to full-screen on a screen with
    /// no card (shouldn't happen with one open).
    pub(crate) fn modal_painter(&mut self, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) -> Painter {
        let card = self.modal_card_rect(screen_w, screen_h, fonts);
        let pad = MODAL_TILE_PAD;
        let region = card.map_or_else(|| Rect::new(0, 0, screen_w, screen_h), |c| c.inflate(pad));
        self.modal.tile_region = region;
        let mut p = Painter::new(region.width(), region.height());
        p.set_origin(region.x(), region.y());
        p
    }
}
