//! `Canvas` — the paint surface every screen draws through.
use crate::ui::render::{Color, Rect};
use crate::ui::text_raster::FontId;
use crate::ui::{cards, listmodal, modal, rows, sidebar, text, ConfirmButton, FocusRow, Fonts, Painter, TextCache};
use anyhow::Result;

/// The target painter, the glyph cache it rasterizes through, the fonts, and the panel size.
///
/// Drawing is the inherent methods below rather than free functions: every one of them
/// wants some subset of painter/cache/fonts, and passing that trio by hand put each call
/// at or past clippy's `too_many_arguments` threshold before a single screen-specific
/// argument was added. The fields stay public for the few callers that paint straight onto
/// the painter (`fill_rect`, `draw_pixmap`) or measure through `fonts.raster`.
pub struct Canvas<'a, 'f> {
    pub painter: &'a mut Painter,
    pub text_cache: &'a mut TextCache,
    pub fonts: &'a Fonts<'f>,
    pub screen_w: u32,
    pub screen_h: u32,
}

impl<'a, 'f> Canvas<'a, 'f> {
    /// A canvas over the full panel — what `app::view::*::render` receives.
    pub fn new(
        painter: &'a mut Painter,
        text_cache: &'a mut TextCache,
        fonts: &'a Fonts<'f>,
        screen_w: u32,
        screen_h: u32,
    ) -> Self {
        Self {
            painter,
            text_cache,
            fonts,
            screen_w,
            screen_h,
        }
    }

    /// A canvas over a standalone tile painter. Screen size reports the tile's own, since
    /// a tile's geometry comes from the rect its caller passes, never from the panel.
    pub fn tile(painter: &'a mut Painter, text_cache: &'a mut TextCache, fonts: &'a Fonts<'f>) -> Self {
        let (w, h) = (painter.width(), painter.height());
        Self::new(painter, text_cache, fonts, w, h)
    }

    // ------------------------------- text -------------------------------

    pub fn text(&mut self, font: FontId, s: &str, x: i32, y: i32, color: Color) -> Result<u32> {
        text::draw_text(self.painter, self.text_cache, self.fonts.raster, font, s, x, y, color)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_wrapped(
        &mut self,
        font: FontId,
        s: &str,
        x: i32,
        y: i32,
        max_w: u32,
        color: Color,
        line_gap: i32,
    ) -> Result<i32> {
        text::draw_text_wrapped(
            self.painter,
            self.text_cache,
            self.fonts.raster,
            font,
            s,
            x,
            y,
            max_w,
            color,
            line_gap,
        )
    }

    /// One line centred horizontally within `within`, at `y`. The `x + (w - measure) / 2`
    /// every centred label was spelling out for itself.
    pub fn text_centered(&mut self, font: FontId, s: &str, within: Rect, y: i32, color: Color) -> Result<u32> {
        let w = self.fonts.raster.measure(font, s).0 as i32;
        self.text(font, s, within.x() + (within.width() as i32 - w) / 2, y, color)
    }

    /// Always drawn in the label/value font pair.
    pub fn modal_header(
        &mut self,
        card: Rect,
        title: &str,
        title_color: Color,
        subtitle: &str,
        subtitle_color: Color,
    ) -> Result<i32> {
        text::draw_modal_header(
            self.painter,
            self.text_cache,
            self.fonts.raster,
            self.fonts.label,
            self.fonts.value,
            card,
            title,
            title_color,
            subtitle,
            subtitle_color,
        )
    }

    // ------------------------------ modal chrome ------------------------------

    pub fn modal_shell(&mut self, card: Rect, hover_close: bool) -> Result<()> {
        modal::draw_modal_shell(self.painter, self.text_cache, self.fonts, card, hover_close)
    }

    pub fn rule(&mut self, x: i32, y: i32, width: u32) {
        modal::draw_rule(self.painter, x, y, width);
    }

    /// Drawn in the value font, like the body copy it separates.
    pub fn or_divider(&mut self, content: Rect, y: i32, word: &str) -> Result<()> {
        let font = self.fonts.value;
        modal::draw_or_divider(self.painter, self.text_cache, self.fonts.raster, font, content, y, word)
    }

    /// Labelled in the label font, like every other button.
    pub fn primary_button(&mut self, rect: Rect, label: &str) -> Result<()> {
        let font = self.fonts.label;
        modal::draw_primary_button(self.painter, self.text_cache, self.fonts.raster, font, rect, label)
    }

    pub fn card(&mut self, rect: Rect, focused: bool) -> Rect {
        cards::draw_card(self.painter, rect, focused)
    }

    // ------------------------------ list modals ------------------------------

    /// A whole list-modal screen: card chrome plus the unfocused header and rows. Every
    /// list-modal screen's `render` is this one call — see `app::view::hostmenu`.
    pub fn list_modal_screen(
        &mut self,
        card: Rect,
        title: &str,
        subtitle: &str,
        rows: &[FocusRow],
        hover_close: bool,
    ) -> Result<()> {
        self.modal_shell(card, hover_close)?;
        self.list_modal(card, title, subtitle, rows)
    }

    pub fn list_modal(&mut self, card: Rect, title: &str, subtitle: &str, rows: &[FocusRow]) -> Result<()> {
        listmodal::render_list_modal(self.painter, self.text_cache, self.fonts, card, title, subtitle, rows)
    }

    // -------------------------------- rows ----------------------------------

    pub fn dropdown_overlay(&mut self, options: &[String], focused_index: usize, rect: Rect) -> Result<()> {
        let font_value = self.fonts.value;
        rows::draw_dropdown_overlay(
            self.painter,
            self.text_cache,
            self.fonts.raster,
            font_value,
            options,
            focused_index,
            rect,
        )
    }

    pub fn confirm_buttons(&mut self, content: Rect, buttons: &[ConfirmButton; 2], focused_index: usize) -> Result<()> {
        rows::draw_confirm_buttons(
            self.painter,
            self.text_cache,
            self.fonts,
            content,
            buttons,
            focused_index,
        )
    }

    pub fn confirm_button(&mut self, button: &ConfirmButton<'_>, focused: bool, rect: Rect) -> Result<()> {
        rows::draw_confirm_button(self.painter, self.text_cache, self.fonts, button, focused, rect)
    }

    // ------------------------------- sidebar --------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn sidebar_row(
        &mut self,
        rect: Rect,
        glyph: &str,
        label: &str,
        focused: bool,
        selected: bool,
        reserve_right: u32,
    ) -> Result<()> {
        sidebar::draw_sidebar_row(
            self.painter,
            self.text_cache,
            self.fonts,
            rect,
            glyph,
            label,
            focused,
            selected,
            reserve_right,
        )
    }

    pub fn sidebar_menu_button(&mut self, row_rect: Rect, row_focused: bool, menu_focused: bool) -> Result<()> {
        sidebar::draw_sidebar_menu_button(
            self.painter,
            self.text_cache,
            self.fonts,
            row_rect,
            row_focused,
            menu_focused,
        )
    }
}
