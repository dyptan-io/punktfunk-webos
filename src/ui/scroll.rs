//! Shared scroll bookkeeping for modal content lists (uniform-stride rows or
//! wrapped text lines). Offset clamping, scroll-into-view, and fade logic
//! extracted so any modal can reuse it. Caller-agnostic to rendering/pixels.
//!
//! [`ScrollWindow`] is the row-index form, for lists whose scroll position picks *which
//! rows render at all*; [`scroll_to_reveal`] is the pixel form, for content that scrolls
//! continuously behind a viewport.
use crate::ui::render::Rect;
use std::time::Instant;

/// The pixel scroll offset that brings `target` (in unscrolled content space) fully
/// inside the vertical band `viewport`, starting from `current` and moving as little as
/// possible. `margin` is slack for whatever the focus treatment draws outside the rect
/// itself. Unclamped — the caller knows its own content extent.
pub fn scroll_to_reveal(target: Rect, viewport: (i32, i32), current: i32, margin: i32) -> i32 {
    let (top, bottom) = viewport;
    let above = target.y() - margin;
    let below = target.bottom() + margin;
    if above - current < top {
        above - top
    } else if below - current > bottom {
        below - bottom
    } else {
        current
    }
}

/// Scroll offset bookkeeping. `total`/`visible` passed per-call (not stored)
/// to avoid stale copies disagreeing with caller's geometry.
#[derive(Clone, Copy)]
pub struct ScrollWindow {
    pub offset: usize,
    /// When `offset` last changed (scrollbar shows then fades).
    pub shown_at: Option<Instant>,
}

impl ScrollWindow {
    pub fn new() -> Self {
        Self {
            offset: 0,
            shown_at: None,
        }
    }

    /// Clamped offset (use where it feeds layout formulas, not raw field).
    pub fn clamped(&self, total: usize, visible: usize) -> usize {
        self.offset.min(total.saturating_sub(visible))
    }

    /// Scroll to keep `focused` visible (no wraparound). Returns whether moved.
    pub fn scroll_into_view(&mut self, focused: usize, total: usize, visible: usize) -> bool {
        let mut offset = self.clamped(total, visible);
        if focused < offset {
            offset = focused;
        } else if focused >= offset + visible {
            offset = focused + 1 - visible;
        }
        self.set(offset)
    }

    fn set(&mut self, offset: usize) -> bool {
        let moved = offset != self.offset;
        self.offset = offset;
        if moved {
            self.shown_at = Some(Instant::now());
        }
        moved
    }
}
