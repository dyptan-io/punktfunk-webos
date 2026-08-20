//! `ModalScreen` — what a modal screen is, from the library's side.
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::Canvas;
use anyhow::Result;

/// Where a modal screen sits, without building what it paints.
///
/// Split out of [`ModalScreen`] because the hit tests run on every Magic Remote
/// `MouseMotion`: a hover only asks where the card and its rows are, and building the
/// screen's `FocusRow` list — one owned `String` per label — to answer that was the most
/// expensive thing on the pointer path. A screen whose rows cost anything to build supplies
/// a separate metrics value that carries their count instead (see `app::view::hostmenu`).
pub trait ModalMetrics {
    /// The card rect this screen paints into, in panel coordinates.
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect;

    /// The row-list viewport inside `card`, for the screens that have one — the geometry
    /// hover, click and the focused-row tile all measure against. `None` for a modal with
    /// no row list (a confirm dialog, a text field, a document).
    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        let _ = (card, fonts);
        None
    }
}

/// One modal screen: where its card sits, and how it paints.
///
/// The pair belongs together — hit-testing, tile sizing and the fade all measure the card
/// the renderer is about to draw, and a per-screen `match` for each of them was how the two
/// drifted apart. An implementor is a plain value built from whatever state the screen
/// shows (see `app::view::*::Modal`), so the app's job is one `match` that picks it.
pub trait ModalScreen: ModalMetrics {
    /// The whole shell — chrome, header, and every widget drawn unfocused. The focused
    /// widget is composited on top as its own tile, so it is not this method's business.
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()>;
}
