//! The widget contract: draw yourself into a given `area` on a given [`Canvas`].
//!
//! Immediate mode, borrowed from Ratatui: no retained widget tree, no reactive graph, no
//! ids. A widget is a plain value describing what to draw; `render` consumes it. Retention
//! here lives one layer down, in the `TileId` texture cache — a widget is rasterized once
//! into a tile and composited from then on (see `ui::tiles`), so there is nothing for a
//! widget object to be worth keeping.
//!
//! `self` by value is what makes builder-style config work without lifetime friction:
//! `SidebarRow::new(rect).selected(true).render(area, c)` needs no intermediate binding.
use crate::ui::render::Rect;
use crate::ui::text::{Fonts, TextCache};
use crate::ui::{Canvas, Painter};
use anyhow::Result;

/// A widget whose whole appearance is in the value itself.
pub trait Widget {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()>;
}

impl Canvas<'_, '_> {
    /// `widget.render(area, self)`, spelled from the canvas end — reads better where the
    /// canvas is the subject of the surrounding code.
    pub fn render<W: Widget>(&mut self, widget: W, area: Rect) -> Result<()> {
        widget.render(area, self)
    }
}

/// A [`Widget`] that also knows how big a surface it wants — so it can be rasterized into a
/// tile of its own rather than into an area someone else already owns.
///
/// This is the second half of the one drawing idiom. A tile source used to be a free
/// `render_*_tile` function that measured, called `Painter::new`, wrapped it in a
/// `Canvas::tile` and only *then* did the one thing that distinguished it from its
/// neighbours — the same four lines fifteen times over, in a vocabulary parallel to
/// [`Widget`]'s. Here the distinguishing part is `render` and the rest is
/// [`rasterize`].
///
/// `size` takes `&self` because the surface has to exist before there is a canvas to draw
/// through, and `fonts` because most tiles are sized by the text they will hold.
pub trait TileWidget: Widget {
    fn size(&self, fonts: &Fonts) -> (u32, u32);
}

/// Rasterizes a [`TileWidget`] into its own painter, sized by the widget and handed to it as
/// the whole area. The one place a standalone tile's surface is created.
pub fn rasterize<W: TileWidget>(widget: W, text_cache: &mut TextCache, fonts: &Fonts) -> Result<Painter> {
    let (w, h) = widget.size(fonts);
    let (w, h) = (w.max(1), h.max(1));
    let mut painter = Painter::new(w, h);
    widget.render(
        Rect::new(0, 0, w, h),
        &mut Canvas::tile(&mut painter, text_cache, fonts),
    )?;
    Ok(painter)
}
