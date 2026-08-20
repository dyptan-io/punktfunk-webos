//! About screen logic: scroll/paging state. Rendering lives in `app::view::about`.
use crate::app::view;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::{Screen, SettingsScope};
use crate::ui;

impl App {
    /// Lazy-initialize about lines on first open.
    pub(crate) fn open_about(&mut self) {
        if self.render.about_lines.is_empty() {
            self.render.about_lines = view::about::lines();
        }
        // `scroll` is shared with Settings' row list — stash it (see `settings_scroll`).
        self.render.settings_scroll = self.render.scroll;
        self.render.scroll = ui::scroll::ScrollWindow::new();
        self.render.content_window = ui::scroll::ContentWindow::new();
        self.nav.screen = Screen::About;
    }

    /// Navigate: Up/Down scroll by line, Left/Right by page.
    pub(crate) fn handle_about_event(&mut self, ev: MenuEvent, screen_w: u32, screen_h: u32, fonts: &ui::text::Fonts) {
        // Only the scrolling events need the document measured (wrapping it is the
        // expensive part), so leaving is not made to pay for it.
        match ev {
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Left | MenuEvent::Right => {
                let (total, visible) = self.about_scroll_geometry(screen_w, screen_h, fonts);
                // Page step with anchor: show last few lines of previous page
                let page_step = visible.saturating_sub(2).max(1);
                match ev {
                    MenuEvent::Up => self.render.scroll.scroll_by(-1, total, visible),
                    MenuEvent::Down => self.render.scroll.scroll_by(1, total, visible),
                    MenuEvent::Left => self.render.scroll.page(page_step, false, total, visible),
                    _ => self.render.scroll.page(page_step, true, total, visible),
                };
            }
            // Return to Settings (not Home) to preserve settings context
            MenuEvent::Back | MenuEvent::Confirm => {
                self.nav.resume(Screen::Settings(SettingsScope::Global));
                self.render.scroll = self.render.settings_scroll;
            }
            MenuEvent::Secondary => {}
        }
    }

    /// Scroll by pixels (Magic Remote wheel).
    pub(crate) fn scroll_about_by(
        &mut self,
        dy_px: i32,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> bool {
        let (total, visible) = self.about_scroll_geometry(screen_w, screen_h, fonts);
        let step = view::about::line_stride(fonts.raster, fonts.value).max(1);
        let lines = dy_px / step;
        if lines == 0 {
            return false;
        }
        self.render.scroll.scroll_by(i64::from(lines), total, visible)
    }

    /// Total and visible line counts.
    pub(crate) fn about_scroll_geometry(
        &mut self,
        screen_w: u32,
        screen_h: u32,
        fonts: &ui::text::Fonts,
    ) -> (usize, usize) {
        let card = view::about::card_rect(screen_w, screen_h);
        let body = view::about::body_rect(card, fonts);
        self.ensure_about_wrapped(fonts, body.width());
        let total = self.render.about_wrapped.as_ref().map_or(0, |(_, v)| v.len());
        let visible = view::about::visible_lines(body, fonts.raster, fonts.value);
        (total, visible)
    }

    /// Defer text wrapping until width is known.
    pub(crate) fn ensure_about_wrapped(&mut self, fonts: &ui::text::Fonts, width: u32) {
        let stale = !matches!(&self.render.about_wrapped, Some((w, _)) if *w == width);
        if stale {
            self.render.about_wrapped = Some((
                width,
                view::about::wrap_document(fonts.raster, fonts.value, &self.render.about_lines, width),
            ));
        }
    }
}
