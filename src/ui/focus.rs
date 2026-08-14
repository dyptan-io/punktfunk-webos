//! Spatial (d-pad) focus navigation over a flat list of on-screen rects.
//!
//! Screens describe *where their focusables are*; this module answers "what is to the
//! left of here". That keeps grid/column arithmetic out of `app::state` — the geometry
//! already lives in `app::view`, and holes in a layout (a partial pinned row) are
//! expressed by simply not emitting an item, rather than by a guard at every direction.

use crate::ui::render::Rect;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    fn vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// Which axis a group wraps on. Axis-scoped rather than a flag, because a vertical list
/// that wraps top-to-bottom must *not* also send a Right press back to its left edge.
/// Only the cases Home needs exist; add a horizontal one when a screen has a row list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrap {
    None,
    Vertical,
}

impl Wrap {
    fn allows(self, dir: Dir) -> bool {
        match self {
            Self::None => false,
            Self::Vertical => dir.vertical(),
        }
    }
}

/// How much a candidate's cross-axis offset counts against it relative to its distance
/// along the direction of travel. Only ever applies between candidates that share no
/// cross-axis overlap at all — anything overlapping wins outright (see [`nearest`]).
const CROSS_PENALTY: i32 = 2;

/// A container of focusables that navigation treats as a unit: moves are resolved
/// inside the group before they are allowed to leave it, and entering the group from
/// outside lands on the remembered position rather than on whatever is geometrically
/// nearest.
struct Group<K> {
    id: u8,
    /// A move that finds nothing inside the group falls through to the far side of the
    /// group instead of leaving it. `None` for grids, where running off the bottom edge
    /// should stop rather than teleport to the top.
    wrap: Wrap,
    /// Where to land when entering from another group, falling back to `default` and
    /// then to the geometrically nearest item. Both are ignored while they name an item
    /// the map doesn't hold, so a stale remembered index can never strand focus.
    remembered: K,
    default: K,
}

struct FocusItem<K> {
    key: K,
    rect: Rect,
    group: u8,
}

/// One screen's focusables, rebuilt per navigation from the same geometry helpers that
/// paint them. Cheap enough to discard each keypress: it is arithmetic over rects, and
/// nothing calls it per frame.
pub struct FocusMap<K> {
    items: Vec<FocusItem<K>>,
    groups: Vec<Group<K>>,
}

impl<K> Default for FocusMap<K> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            groups: Vec::new(),
        }
    }
}

impl<K: Copy + PartialEq> FocusMap<K> {
    /// Declares a group. Call before adding its items.
    pub fn group(&mut self, id: u8, wrap: Wrap, remembered: K, default: K) {
        self.groups.push(Group {
            id,
            wrap,
            remembered,
            default,
        });
    }

    pub fn item(&mut self, key: K, rect: Rect, group: u8) {
        self.items.push(FocusItem { key, rect, group });
    }

    fn spec(&self, id: u8) -> Option<&Group<K>> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The key a group should be entered on: its remembered position, else its declared
    /// default, else `fallback` (the geometric winner).
    fn entry_point(&self, id: u8, fallback: K) -> K {
        let Some(group) = self.spec(id) else { return fallback };
        let holds = |k: K| self.items.iter().any(|i| i.group == id && i.key == k);
        [group.remembered, group.default]
            .into_iter()
            .find(|&k| holds(k))
            .unwrap_or(fallback)
    }

    /// The focus `dir` moves to from `from`, or `None` when nothing lies that way.
    ///
    /// Resolution order: nearest inside the origin's own group, then that group's wrap,
    /// then nearest anywhere — with a cross-group winner redirected to its group's
    /// entry point so containers keep their place.
    pub fn navigate(&self, from: K, dir: Dir) -> Option<K> {
        let origin = self.items.iter().find(|i| i.key == from)?;
        let (rect, home) = (origin.rect, origin.group);
        let siblings = || self.items.iter().filter(|i| i.group == home && i.key != from);

        if let Some(hit) = nearest(siblings(), rect, dir) {
            return Some(hit);
        }
        if self.spec(home).is_some_and(|g| g.wrap.allows(dir)) {
            if let Some(hit) = opposite_extreme(siblings(), rect, dir) {
                return Some(hit);
            }
        }
        let hit = nearest(self.items.iter().filter(|i| i.group != home), rect, dir)?;
        let group = self.items.iter().find(|i| i.key == hit)?.group;
        Some(self.entry_point(group, hit))
    }
}

/// Gap between two 1-D intervals — `0` when they overlap at all.
fn interval_gap((a1, a2): (i32, i32), (b1, b2): (i32, i32)) -> i32 {
    (a1.max(b1) - a2.min(b2)).max(0)
}

fn interval_center_delta((a1, a2): (i32, i32), (b1, b2): (i32, i32)) -> i32 {
    ((a1 + a2) - (b1 + b2)).abs() / 2
}

fn cross_span(dir: Dir, r: Rect) -> (i32, i32) {
    if dir.vertical() {
        (r.x(), r.right())
    } else {
        (r.y(), r.bottom())
    }
}

/// Distance from `origin` to `cand` along `dir`, or `None` when `cand` is not strictly
/// beyond `origin`'s leading edge. Rects that merely overlap the origin are never
/// candidates, so callers must keep a screen's focusables disjoint along each axis they
/// expect to navigate (see `app::view::sidebar::row_body_rect`).
fn primary_gap(dir: Dir, origin: Rect, cand: Rect) -> Option<i32> {
    let gap = match dir {
        Dir::Up => origin.y() - cand.bottom(),
        Dir::Down => cand.y() - origin.bottom(),
        Dir::Left => origin.x() - cand.right(),
        Dir::Right => cand.x() - origin.right(),
    };
    (gap >= 0).then_some(gap)
}

/// Nearest candidate in `dir`: anything sharing cross-axis overlap with the origin beats
/// anything that doesn't, and within each bucket the lowest weighted distance wins.
/// Ties break toward the better-centred candidate, then toward insertion order.
fn nearest<'a, K: Copy + 'a>(items: impl Iterator<Item = &'a FocusItem<K>>, origin: Rect, dir: Dir) -> Option<K> {
    let origin_cross = cross_span(dir, origin);
    items
        .filter_map(|i| {
            let primary = primary_gap(dir, origin, i.rect)?;
            let cross = cross_span(dir, i.rect);
            let gap = interval_gap(origin_cross, cross);
            // `gap > 0` leads: an aligned candidate always outranks an unaligned one,
            // however much further along the axis it sits.
            let rank = (
                gap > 0,
                primary + CROSS_PENALTY * gap,
                interval_center_delta(origin_cross, cross),
            );
            Some((rank, i.key))
        })
        .min_by_key(|&(rank, _)| rank)
        .map(|(_, key)| key)
}

/// The item furthest *against* `dir` — where a wrapping group sends a move that ran off
/// its edge. Prefers staying in the origin's row/column so wrapping a two-column group
/// doesn't also change column.
fn opposite_extreme<'a, K: Copy + 'a>(
    items: impl Iterator<Item = &'a FocusItem<K>>,
    origin: Rect,
    dir: Dir,
) -> Option<K> {
    let origin_cross = cross_span(dir, origin);
    items
        .min_by_key(|i| {
            let depth = match dir {
                Dir::Up => -i.rect.y(),
                Dir::Down => i.rect.y(),
                Dir::Left => -i.rect.x(),
                Dir::Right => i.rect.x(),
            };
            let cross = cross_span(dir, i.rect);
            (
                interval_gap(origin_cross, cross),
                depth,
                interval_center_delta(origin_cross, cross),
            )
        })
        .map(|i| i.key)
}
