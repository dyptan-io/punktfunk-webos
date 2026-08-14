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
use crate::ui::Canvas;
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
