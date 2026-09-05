//! Focus rings, selectable cards, game-grid poster card.
use crate::ui::prelude::*;

/// Card corner radius (softened from moonlight-tv's ~2px).
pub const CARD_RADIUS: i32 = 10;
pub const MODAL_RADIUS: i32 = 20;

/// The themed card shapes, as `Painter` methods rather than free functions taking one:
/// they need nothing but the surface, so a tile builder with only a `Painter` (and no
/// fonts — see `render_focus_ring_tile`) can still draw them. Anything that also needs
/// the glyph cache or fonts is a [`Canvas`](crate::ui::Canvas) method instead.
impl Painter {
    /// Soft drop shadow matching moonlight-tv's card look.
    pub fn card_shadow(&mut self, rect: Rect, radius: i32) {
        let (dx, dy) = SHADOW_OFFSET;
        self.fill_shadow(rect, radius, dx as f32, dy as f32, SHADOW_BLUR, SHADOW_OPACITY);
    }

    /// Focus card that never inflates. Rows are rasterized once at their literal size;
    /// `app::App`'s draw-list animates the zoom by GPU-scaling the focused-row tile around its
    /// center (same technique as the grid's card focus-pop) — a CPU-baked inflate
    /// here would fight that, since the rasterized content would then need
    /// re-rendering every animation frame instead of just repositioning.
    pub fn selectable_fixed(&mut self, rect: Rect, focused: bool) {
        if focused {
            self.card_shadow(rect, CARD_RADIUS);
            self.fill_rounded_rect(rect, CARD_RADIUS, palette().surface);
        }
    }
}
