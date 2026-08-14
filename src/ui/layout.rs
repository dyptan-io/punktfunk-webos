//! Constraint layout: split one rect into a stack of rects, once, and let paint,
//! hit-testing and focus all read the same result.
//!
//! The design is Ratatui's `Layout`/`Constraint`, not its implementation: layouts here are
//! 1-D stacks of pixels, so there is no cassowary solver and no `u16` cell arithmetic —
//! resolve the fixed constraints, hand the remainder to `Fill` by weight, clamp. What
//! it replaces is the pair of `*_rect` helpers every screen used to keep in agreement
//! with its own painter, each recomputing the same offsets from scratch.

use crate::ui::render::Rect;
/// How much of a [`Layout`]'s length one slot takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Constraint {
    /// Exactly this many pixels.
    Length(u32),
    /// At least this many pixels, and flexible above that: shares the leftover space like
    /// a `Fill(1)` but never drops below its floor while anything more elastic remains.
    /// A centring spacer with a minimum inset is the usual case.
    Min(u32),
    /// This percentage of the length available to the whole stack (gaps already removed).
    Percentage(u16),
    /// A weighted share of whatever is left after the fixed slots — `Fill(2)` takes twice
    /// what `Fill(1)` does. The usual "and the rest goes here" slot.
    Fill(u16),
}

impl Constraint {
    /// The slot's starting size, before any leftover space is handed out.
    fn baseline(self, total: u32) -> u32 {
        match self {
            Self::Length(n) | Self::Min(n) => n,
            Self::Percentage(p) => total * u32::from(p.min(100)) / 100,
            Self::Fill(_) => 0,
        }
    }

    /// How big a share of the leftover space this slot takes. `Min` is flexible too — its
    /// baseline is a floor, not a size.
    fn fill_weight(self) -> u32 {
        match self {
            Self::Fill(w) => u32::from(w),
            Self::Min(_) => 1,
            _ => 0,
        }
    }
}

/// Which way a [`Layout`] stacks its slots.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A stack of [`Constraint`]s to divide a rect by. Build it, [`split`](Layout::split) it,
/// and index the result — that `Vec<Rect>` is the one geometry every consumer reads.
pub struct Layout {
    axis: Axis,
    constraints: Vec<Constraint>,
    gap: i32,
}

impl Layout {
    pub fn vertical(constraints: impl IntoIterator<Item = Constraint>) -> Self {
        Self::new(Axis::Vertical, constraints)
    }

    pub fn horizontal(constraints: impl IntoIterator<Item = Constraint>) -> Self {
        Self::new(Axis::Horizontal, constraints)
    }

    fn new(axis: Axis, constraints: impl IntoIterator<Item = Constraint>) -> Self {
        Self {
            axis,
            constraints: constraints.into_iter().collect(),
            gap: 0,
        }
    }

    /// Pixels left between consecutive slots. Gaps come off the length before the
    /// constraints are resolved, so `Fill` and `Percentage` never eat into them.
    pub fn gap(mut self, gap: i32) -> Self {
        self.gap = gap;
        self
    }

    /// The length this stack occupies along its axis — what a card's own height is derived
    /// from when the content decides the size rather than the other way round (the probe
    /// pattern: build the stack, ask how long it is, then place the card).
    pub fn total_length(&self) -> u32 {
        let gaps = self.gap * (self.constraints.len() as i32 - 1).max(0);
        self.sizes(u32::MAX).iter().sum::<u32>() + gaps.max(0) as u32
    }

    /// Divides `area` into one rect per constraint, in order.
    pub fn split(&self, area: Rect) -> Vec<Rect> {
        let n = self.constraints.len();
        let gaps = (self.gap * (n as i32 - 1).max(0)).max(0) as u32;
        let along = match self.axis {
            Axis::Vertical => area.height(),
            Axis::Horizontal => area.width(),
        };
        let sizes = self.sizes(along.saturating_sub(gaps));

        let mut rects = Vec::with_capacity(n);
        let mut cursor = match self.axis {
            Axis::Vertical => area.y(),
            Axis::Horizontal => area.x(),
        };
        for size in sizes {
            rects.push(match self.axis {
                Axis::Vertical => Rect::new(area.x(), cursor, area.width(), size),
                Axis::Horizontal => Rect::new(cursor, area.y(), size, area.height()),
            });
            cursor += size as i32 + self.gap;
        }
        rects
    }

    /// The resolved length of every slot, given the space left after gaps.
    ///
    /// Overflow clamps rather than panicking, shrinking from the most elastic slot to the
    /// least: `Fill` down to nothing, then `Min` past its floor, and `Length`/`Percentage`
    /// only if the area is smaller than the fixed content itself. A `u32::MAX` `total`
    /// means "unconstrained" — [`total_length`](Self::total_length) asking what the
    /// baselines add up to.
    fn sizes(&self, total: u32) -> Vec<u32> {
        let mut sizes: Vec<u32> = self.constraints.iter().map(|c| c.baseline(total)).collect();
        if total == u32::MAX {
            return sizes;
        }
        let used: u32 = sizes.iter().sum();

        if let Some(mut spare) = total.checked_sub(used).filter(|&s| s > 0) {
            let weights: u32 = self.constraints.iter().map(|c| c.fill_weight()).sum();
            if let Some(weights) = std::num::NonZeroU32::new(weights) {
                for (size, c) in sizes.iter_mut().zip(&self.constraints) {
                    let share = spare * c.fill_weight() / weights.get();
                    *size += share;
                    spare -= share;
                }
                // Integer division leaves a pixel or two over; the first flexible slot
                // absorbs it so the stack always fills `area` exactly.
                if let Some((size, _)) = sizes
                    .iter_mut()
                    .zip(&self.constraints)
                    .find(|(_, c)| c.fill_weight() > 0)
                {
                    *size += spare;
                }
            }
            return sizes;
        }

        let mut over = used.saturating_sub(total);
        for elasticity in [1u8, 2, 3] {
            for (size, c) in sizes.iter_mut().zip(&self.constraints) {
                if over == 0 {
                    return sizes;
                }
                let elastic = match c {
                    Constraint::Fill(_) => 1,
                    Constraint::Min(_) => 2,
                    Constraint::Length(_) | Constraint::Percentage(_) => 3,
                };
                if elastic == elasticity {
                    let cut = (*size).min(over);
                    *size -= cut;
                    over -= cut;
                }
            }
        }
        sizes
    }
}
