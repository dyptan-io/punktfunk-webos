//! What the menu is drawing with: the tile-backed animation state, the scroll windows, and the
//! dirty flags that decide what gets re-rastered.
//!
//! Split off `App` so a `&mut self.render` pass can run beside a `&self` read of the library or
//! the host list. Nothing here is domain state — it is all recoverable from a redraw.

use std::time::Instant;

use crate::app::{grid, hero, modal};
use crate::ui;
use crate::ui::render::TileId;

pub(crate) struct RenderState {
    /// The connecting screen's backdrop, and every clock it runs on.
    pub(crate) hero: hero::Hero,
    /// Scroll state for overflowing modal content.
    pub(crate) scroll: ui::scroll::ScrollWindow,
    /// The About document's source lines, built once on first open. ~10,000 static string
    /// slices; cheap to hold, wasteful to rebuild per frame.
    pub(crate) about_lines: Vec<&'static str>,
    /// `about_lines` wrapped to a body width, flattened into one list of visual lines (see
    /// `draw::about::wrap_document`) — the unit About scrolls over,
    /// since a source line's wrapped length varies and only the flattened list has a uniform
    /// per-unit stride. Keyed by the body width it was wrapped for, rebuilt if that width
    /// changes.
    pub(crate) about_wrapped: Option<(u32, Vec<String>)>,
    /// Whether the Magic Remote's pointer is currently hovering a modal's close (X) button.
    pub(crate) hover_close: bool,
    /// The grid's cover images by game id (see `app::draw::home`).
    pub(crate) covers: crate::app::draw::home::Covers,
    /// The launch backdrop as a Skia image, built when `hero` says its art is in hand.
    pub(crate) hero_image: Option<skia_safe::Image>,
    /// Tiles whose GPU texture this frame released — drained by the render loop, which does
    /// the actual `drop_tile`. Nothing to do with the style: a Theme pick stales tiles through
    /// `ui::theme::epoch` folded into every cache version, not through this list.
    pub(crate) evicted_tiles: Vec<TileId>,
    pub(crate) modal: modal::ModalState,
    pub(crate) grid: grid::GridState,
    pub(crate) focus_anim: Option<Instant>,
    pub(crate) press: ui::animation::Press,
    /// The kit list widget of the open ported list screen (`app::draw::list`), with the
    /// screen it was made for — a different screen gets a fresh one.
    pub(crate) list: Option<(crate::core::screen::Screen, pf_console_ui::widgets::MenuList)>,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            hero: hero::Hero::default(),
            scroll: ui::scroll::ScrollWindow::new(),
            about_lines: Vec::new(),
            about_wrapped: None,
            hover_close: false,
            covers: Default::default(),
            hero_image: None,
            evicted_tiles: Vec::new(),
            modal: modal::ModalState::default(),
            // Hand-written, not derived: `GridState`'s own `Default` starts the tile-id counter
            // past the fixed band (see `grid.rs`).
            grid: grid::GridState::default(),
            focus_anim: None,
            press: ui::animation::Press::default(),
            list: None,
        }
    }
}
