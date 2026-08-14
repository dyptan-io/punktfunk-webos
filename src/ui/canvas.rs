//! `Canvas` — the paint surface every screen draws through.
use crate::ui::text::{Fonts, TextCache};
use crate::ui::Painter;

/// The target painter, the glyph cache it rasterizes through, the fonts, and the panel size.
///
/// Drawing goes through inherent `Canvas` methods rather than free functions: every one of
/// them wants some subset of painter/cache/fonts, and passing that trio by hand put each
/// call at or past clippy's `too_many_arguments` threshold before a single screen-specific
/// argument was added. Those methods live next to the widgets they draw — `impl Canvas`
/// blocks in `rows`, `cards`, `modal`, `text`, … — not here, so a widget's geometry, its
/// paint code and its docs stay in one file. Shapes that need only the surface are
/// [`Painter`] methods instead (see `cards`), so a tile builder holding no fonts can still
/// draw them.
///
/// The fields stay public for callers that paint straight onto the painter
/// (`fill_rect`, `draw_pixmap`) or measure through `fonts.raster`.
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
}
