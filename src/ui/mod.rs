//! Portable widget library: the pre-stream UI's paint surface, widgets, layout and theme.
//! Renders via [`Painter`] (`tiny_skia` software rasterizer) into tiles the platform layer
//! uploads as textures. Text in Geist font; icons from a bundled subsetted font.
//!
//! Namespaced rather than flat (Ratatui's own division): [`layout`] splits rects,
//! [`widgets`] draws into them, [`theme`] says in what colour, [`text`] measures and
//! rasterizes glyphs. [`Canvas`] is the surface all of them draw through, and
//! [`Widget`]/[`StatefulWidget`] the contract they implement — plus [`TileWidget`] for one
//! that sizes and owns its own surface, which is what every entry in [`tiles`] is. Screens
//! live in `app::view`.
//!
//! Inside this module the names stay flat — widgets compose each other constantly — via
//! the crate-internal [`prelude`].

pub mod animation;
pub mod cache;
pub mod fade;
pub mod focus;
pub mod layout;
pub mod painter;
pub mod render;
pub mod scroll;
pub mod spinner;
pub mod text;
pub mod text_raster;
pub mod theme;
pub mod tiles;
pub mod widgets;

mod canvas;
mod screen;
mod widget;

pub use canvas::Canvas;
pub use painter::Painter;
pub use screen::{ModalMetrics, ModalScreen};
pub use widget::{rasterize, rasterize_into, StatefulWidget, TileWidget, Widget};

/// Every `ui` name, flat — for `ui`'s own modules only. A widget reaches for the theme, the
/// text cache, the layout solver and two neighbouring widgets in the same function; making
/// each of those a qualified path buys nothing inside the library that draws them all.
pub(crate) mod prelude {
    pub(crate) use crate::ui::animation::*;
    pub(crate) use crate::ui::fade::*;
    pub(crate) use crate::ui::focus::Dir;
    pub(crate) use crate::ui::layout::*;
    pub(crate) use crate::ui::painter::*;
    pub(crate) use crate::ui::render::{Color, Rect};
    pub(crate) use crate::ui::text::*;
    pub(crate) use crate::ui::text_raster::{FontId, TextRaster};
    pub(crate) use crate::ui::theme::{icons, palette};
    pub(crate) use crate::ui::tiles::*;
    pub(crate) use crate::ui::widgets::*;
    pub(crate) use crate::ui::{Canvas, StatefulWidget, TileWidget, Widget};
}
