use crate::ui::prelude::*;
use anyhow::Result;

/// A centered glass card of `(width_frac * screen_w, height)` — a centring spacer either
/// side of the card on both axes, which is all "centred" is.
pub fn modal_card_rect(screen_w: u32, screen_h: u32, width_frac: f32, height: u32) -> Rect {
    center(Rect::new(0, 0, screen_w, screen_h), width_frac, height, 0)
}

/// The card `width_frac` wide and `height` tall, centred in `within`, with the top spacer
/// never shorter than `min_top`.
fn center(within: Rect, width_frac: f32, height: u32, min_top: u32) -> Rect {
    let column = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Percentage((width_frac * 100.0).round() as u16),
        Constraint::Fill(1),
    ])
    .split(within)[1];
    Layout::vertical([Constraint::Min(min_top), Constraint::Length(height), Constraint::Min(0)]).split(column)[1]
}

impl Painter {
    /// The modal card surface, opaque — for the in-stream dialogs, which have no frost
    /// beneath them to show through (the video is on a hardware plane below the SDL
    /// surface, so it is not in the framebuffer this composites into).
    pub fn modal_card(&mut self, rect: Rect) {
        self.panel_in(rect, MODAL_RADIUS, palette().panel);
    }

    /// The same card in [`Glass::panel`](crate::ui::theme::Glass::panel) — for a
    /// menu modal, whose compositor draws a `DrawCmd::Frost` of the same rect and radius
    /// underneath it.
    /// No drop shadow: the modal card's shadow is nine GPU draws from
    /// [`tile::MODAL_SHADOW`](crate::app::render::tile::MODAL_SHADOW), not a card-sized blit
    /// baked in here. On a 1190x924 card that blit measured 205ms of a 260ms shell raster —
    /// `draw_pixmap` runs a pattern shader per pixel, at a measured 5.6 megapixels a second, so the cost was
    /// the card's whole area, paid on every open. See [`shadow_atlas`](crate::ui::painter::shadow_atlas).
    pub fn modal_card_glass(&mut self, rect: Rect) {
        self.glass_face(rect, MODAL_RADIUS, crate::ui::theme::glass_fill());
    }

    /// [`Self::glass_face`] with a drop shadow baked in beside it — for a surface drawn
    /// into a painter big enough to hold that shadow, which in practice means the in-stream
    /// dialogs, whose whole frame is one painter. Anything rendering into a tile sized to
    /// the surface itself wants `glass_face` plus a composited shadow instead (see
    /// [`shadow_atlas`](crate::ui::painter::shadow_atlas)) — a baked one would only be
    /// clipped away.
    pub fn panel_in(&mut self, rect: Rect, radius: i32, fill: Color) {
        self.card_shadow(rect, radius);
        self.glass_face(rect, radius, fill);
    }

    /// [`Self::panel_in`] without the drop shadow — for a surface drawn into a tile sized to
    /// the panel exactly, where every shadow pixel would fall outside the canvas or be
    /// overpainted by the fill, and the blur that produced it is pure waste.
    pub fn glass_face(&mut self, rect: Rect, radius: i32, fill: Color) {
        self.fill_rounded_rect(rect, radius, fill);
        self.stroke_rounded_rect(rect, radius, palette().glass_edge, 1.5);
    }
}

impl Canvas<'_, '_> {
    /// Shared modal chrome — the rounded card and its close (X) button — that every
    /// screen's renderer draws before its own content inside `card`.
    ///
    /// No backdrop: the scrim behind the modal is a GPU fill in the composed frame (it fades
    /// in with the modal), and this painter is the modal's own transparent tile. The card is
    /// glass — `compose_modal_card` pushes the matching `DrawCmd::Frost` under this tile.
    pub fn modal_shell(&mut self, card: Rect, hover_close: bool) -> Result<()> {
        self.painter.modal_card_glass(card);
        let color = if hover_close { palette().text } else { palette().muted };
        self.icon(modal_close_rect(card), icons().close, color)
    }
}

/// Width fraction shared by the confirm-style modals (forget host, send logs, stop
/// streaming, quit app) — narrower than the scrollable `ListModal` screens.
pub const SIMPLE_MODAL_WIDTH_FRAC: f32 = 0.40;

/// A centered [`SIMPLE_MODAL_WIDTH_FRAC`]-wide card whose *height* is derived from its
/// own content: `content_height` receives a zero-y/height probe card at the final width
/// and returns the card's total height. Shared by every confirm modal so they size
/// identically whether they render through `App` (forget/send-logs) or `main.rs`'s
/// in-stream/quit dialog.
pub fn simple_modal_card(screen_w: u32, screen_h: u32, content_height: impl FnOnce(Rect) -> u32) -> Rect {
    let w = (screen_w as f32 * SIMPLE_MODAL_WIDTH_FRAC).round() as u32;
    let height = content_height(Rect::new(0, 0, w, 0));
    modal_card_rect(screen_w, screen_h, SIMPLE_MODAL_WIDTH_FRAC, height)
}

pub fn modal_close_rect(card_rect: Rect) -> Rect {
    const SIZE: u32 = 44;
    const MARGIN: i32 = 20;
    Rect::new(
        card_rect.right() - MARGIN - SIZE as i32,
        card_rect.y() + MARGIN,
        SIZE,
        SIZE,
    )
}

impl Canvas<'_, '_> {}
