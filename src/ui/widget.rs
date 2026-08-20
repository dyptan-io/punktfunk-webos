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

/// A widget that reads (and may advance) state its caller owns across frames — a focus
/// index, a [`ScrollWindow`](crate::ui::scroll::ScrollWindow). What used to ride as loose
/// arguments next to the content: what was `draw_focus_rows(…, focused, open_dropdown, …)`
/// is `FocusRows::new(rows).render(area, c, &mut state)`.
pub trait StatefulWidget {
    type State;
    fn render(self, area: Rect, c: &mut Canvas, state: &mut Self::State) -> Result<()>;
}

impl Canvas<'_, '_> {
    /// `widget.render(area, self)`, spelled from the canvas end — reads better where the
    /// canvas is the subject of the surrounding code.
    pub fn render<W: Widget>(&mut self, widget: W, area: Rect) -> Result<()> {
        widget.render(area, self)
    }

    /// [`render`](Self::render) for a [`StatefulWidget`].
    pub fn render_stateful<W: StatefulWidget>(&mut self, widget: W, area: Rect, state: &mut W::State) -> Result<()> {
        widget.render(area, self, state)
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
    rasterize_into(widget, None, text_cache, fonts)
}

/// [`rasterize`] onto a surface the caller already has, when it is the right size — the grid
/// hands back the pixmap of a card it evicted this frame rather than freeing one and
/// allocating an identical one a few lines later (see `GridState::free_cards`). A `recycled`
/// buffer of the wrong size is simply dropped.
pub fn rasterize_into<W: TileWidget>(
    widget: W,
    recycled: Option<Painter>,
    text_cache: &mut TextCache,
    fonts: &Fonts,
) -> Result<Painter> {
    let (w, h) = widget.size(fonts);
    let (w, h) = (w.max(1), h.max(1));
    let mut painter = match recycled {
        // Reused pixels are the previous card's, so the wipe is not optional: a card is
        // rounded, and its corners are never drawn over.
        Some(mut p) if p.width() == w && p.height() == h => {
            p.reset();
            p
        }
        _ => Painter::new(w, h),
    };
    widget.render(
        Rect::new(0, 0, w, h),
        &mut Canvas::tile(&mut painter, text_cache, fonts),
    )?;
    Ok(painter)
}
