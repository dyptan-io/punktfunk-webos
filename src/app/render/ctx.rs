//! [`RenderCtx`] — what every `prepare_*` pass writes through.
//!
//! The rasterization passes all need the same five things: the tile store, the rasterized-text
//! cache, the fonts, the size of the screen they are laying out for, and somewhere to record
//! what they rebuilt. Threading those as parameters gave one pass eight arguments and another
//! nine, each with a `#[allow(clippy::too_many_arguments)]` on top; this is that list, named
//! once.
//!
//! Deliberately not a *renderer*: it owns no state of its own beyond the frame's output list,
//! so a pass still reads the state it draws from `App` — the split this module has always had.
use crate::ui::cache::TileStore;
use crate::ui::render::{Size, TileId};
use crate::ui::text::{Fonts, TextCache};

pub(crate) struct RenderCtx<'a> {
    /// The rasterized tiles, and the versions they were built at.
    pub tiles: &'a mut TileStore,
    /// Rasterized glyph runs, shared by every tile built this frame.
    pub text: &'a mut TextCache,
    pub fonts: &'a Fonts<'a>,
    pub screen: Size,
    /// The loop's "an event or a background result changed something this tick" flag. It forces
    /// the open modal's tile to re-rasterize, since modal content has no finer dirty tracking
    /// of its own; a pure animation frame passes `false` and rasterizes nothing at all.
    pub content_dirty: bool,
    /// Whether `advance_frame` saw the screen change this tick — the entering modal's tile has
    /// to be rebuilt whatever its key says, and the leaving one's pixels snapshotted.
    pub screen_changed: bool,
    /// Tiles rebuilt this frame, for the caller to re-upload. Drained by
    /// [`App::prepare_tiles`](crate::app::App::prepare_tiles).
    pub updated: Vec<TileId>,
    /// The settings row list, built at most once per frame. Two passes want it — the row
    /// tiles and the focused-row tile — and each used to build its own copy, which on
    /// armv7 is `String` formatting per row twice over. Frame-scoped: nothing here is
    /// carried between frames, so there is no staleness to reason about.
    pub settings_rows: Option<Vec<crate::ui::widgets::FocusRow>>,
}

impl<'a> RenderCtx<'a> {
    pub fn new(
        tiles: &'a mut TileStore,
        text: &'a mut TextCache,
        fonts: &'a Fonts<'a>,
        screen: Size,
        content_dirty: bool,
        screen_changed: bool,
    ) -> Self {
        Self {
            tiles,
            text,
            fonts,
            screen,
            content_dirty,
            screen_changed,
            updated: Vec::new(),
            settings_rows: None,
        }
    }
}
