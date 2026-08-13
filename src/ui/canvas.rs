//! `Canvas` — the four things every screen needs to paint, as one value.
use crate::ui::{Fonts, Painter, TextCache};

/// The target painter, the glyph cache it rasterizes through, the fonts, and the panel size.
///
/// Every `app::view::*::render` took these five as separate parameters, which put all of them
/// at or past clippy's `too_many_arguments` threshold before a single screen-specific argument
/// was added. `painter` and `text_cache` stay separate fields rather than being hidden behind
/// methods: the `ui` drawing primitives take both, and disjoint field borrows let a caller
/// pass `c.painter` and `c.text_cache` to one call.
pub struct Canvas<'a, 'f> {
    pub painter: &'a mut Painter,
    pub text_cache: &'a mut TextCache,
    pub fonts: &'a Fonts<'f>,
    pub screen_w: u32,
    pub screen_h: u32,
}
