//! Home screen geometry: the game grid (columns, card rects, scroll extent) and the pixel
//! placement of its sections. The section *arithmetic* (which group a row is in, what offset
//! it carries) is `app::grid::GridLayout`; navigation/selection is `app::state::home`.
use crate::app::grid::GridLayout;
use crate::ui::render::Rect;

/// Height reserved for one section heading: one line of the title font plus air under it. A
/// constant rather than a measured line — the grid's geometry is used from the pointer path
/// and the focus map, neither of which has a rasterizer to hand.
pub const SECTION_HEADING_H: i32 = 60;
/// Of that height, the air between the heading and the cards under it.
pub const SECTION_HEADING_PAD: i32 = 14;
/// Gap above every heading after the first — what makes the block above it read as finished
/// (there is no dividing line; the headings say it).
pub const SECTION_GAP: i32 = 32;

/// [`grid_card_rect`] translated by the sections above it — everything except the current
/// scroll offset, which [`scrolled_card_rect`] applies on top.
pub(crate) fn unscrolled_card_rect(idx: usize, grid_x: i32, available_w: u32, layout: GridLayout) -> Rect {
    grid_card_rect(idx, layout.columns(), grid_x, available_w).offset(0, layout.row_offset(idx))
}

/// [`unscrolled_card_rect`] translated by the current scroll offset — every draw-list card
/// position starts from this.
pub(crate) fn scrolled_card_rect(
    idx: usize,
    grid_x: i32,
    available_w: u32,
    layout: GridLayout,
    grid_scroll: i32,
) -> Rect {
    unscrolled_card_rect(idx, grid_x, available_w, layout).offset(0, -grid_scroll)
}

/// The band a section heading is drawn in, scrolled like any other grid content: directly
/// above `first_idx`, the first card of the section it names, so heading and block move
/// together. Positioned off [`scrolled_card_rect`], which is what keeps them aligned.
pub(crate) fn section_heading_rect(
    first_idx: usize,
    grid_x: i32,
    available_w: u32,
    layout: GridLayout,
    grid_scroll: i32,
) -> Rect {
    let row = scrolled_card_rect(first_idx, grid_x, available_w, layout, grid_scroll);
    Rect::new(
        grid_x,
        row.y() - SECTION_HEADING_H,
        available_w,
        SECTION_HEADING_H as u32,
    )
    .inset_x(GRID_PAD as u32)
}

/// The grid indices that can appear in a `screen_h`-tall viewport at `grid_scroll`, with `pad`
/// px of slack at both edges (the card shadow, which draws outside the card's own rect).
///
/// Arithmetic, not a scan: rows are uniformly spaced inside each section band and the bands'
/// offsets only grow with row index, so a card's y is monotone in its index — the visible set
/// is one contiguous range whose ends are a division. The compose path used to ask every card
/// in the library whether it was on screen, once per frame, to learn the same thing.
pub(crate) fn visible_cards(
    available_w: u32,
    layout: GridLayout,
    grid_scroll: i32,
    screen_h: i32,
    pad: i32,
) -> std::ops::Range<usize> {
    let cols = layout.columns();
    let count = layout.len();
    let (_, card_h) = grid_card_size(available_w, cols);
    let row_h = card_h as i32 + GRID_GAP;
    let mut first = layout.rows();
    let mut last = 0;
    for (band, offset) in layout.row_bands() {
        if band.is_empty() {
            continue;
        }
        // `y(row) = GRID_TOP_Y + row * row_h + offset - grid_scroll`, visible while
        // `y + card_h + pad >= 0` and `y - pad <= screen_h`.
        let top = GRID_TOP_Y + offset - grid_scroll;
        let lo = div_ceil(-top - card_h as i32 - pad, row_h).clamp(band.start as i32, band.end as i32);
        let hi = div_floor(screen_h + pad - top, row_h).clamp(band.start as i32 - 1, band.end as i32 - 1);
        if lo > hi {
            continue;
        }
        first = first.min(lo as usize);
        last = last.max(hi as usize);
    }
    if first > last {
        return 0..0;
    }
    (first * cols).min(count)..((last + 1) * cols).min(count)
}

/// `floor(a / b)` for a positive `b` — `/` truncates towards zero, which is the wrong way for
/// a negative scroll offset.
fn div_floor(a: i32, b: i32) -> i32 {
    a.div_euclid(b)
}

/// `ceil(a / b)` for a positive `b`.
fn div_ceil(a: i32, b: i32) -> i32 {
    -(-a).div_euclid(b)
}

pub const GRID_PAD: i32 = 32;
pub const GRID_GAP: i32 = 24;
pub const GRID_TOP_Y: i32 = 160;
pub const CARD_MIN_W: u32 = 220;

/// `clamp(2, available_w / (min_card_w + gap), 5)` — moonlight-tv's own formula.
pub fn grid_columns(available_w: u32) -> usize {
    let cols = (available_w / (CARD_MIN_W + GRID_GAP as u32)).max(1);
    cols.clamp(2, 5) as usize
}

/// [`grid_columns`] from the screen width rather than the grid's: the sidebar is the one
/// thing between the two, and every handler that only has a screen width was subtracting it
/// by hand.
pub fn grid_columns_for_screen(screen_w: u32) -> usize {
    grid_columns(screen_w.saturating_sub(crate::ui::widgets::SIDEBAR_W))
}

/// Card size in 3:4 portrait aspect (moonlight-tv's box-art style).
pub fn grid_card_size(available_w: u32, columns: usize) -> (u32, u32) {
    let usable = available_w.saturating_sub(2 * GRID_PAD as u32);
    let gaps = (columns as u32).saturating_sub(1) * GRID_GAP as u32;
    let w = usable.saturating_sub(gaps) / columns.max(1) as u32;
    let h = w * 4 / 3;
    (w, h)
}

pub fn grid_card_rect(index: usize, columns: usize, grid_x: i32, available_w: u32) -> Rect {
    let (card_w, card_h) = grid_card_size(available_w, columns);
    let col = index % columns.max(1);
    let row = index / columns.max(1);
    let x = grid_x + GRID_PAD + col as i32 * (card_w as i32 + GRID_GAP);
    let y = GRID_TOP_Y + row as i32 * (card_h as i32 + GRID_GAP);
    Rect::new(x, y, card_w, card_h)
}

/// Headroom for card shadows in cached grid layer (prevents clipping).
pub const GRID_LAYER_PAD: i32 = 24;

/// Cached grid layer height (all rows + shadow headroom).
pub fn grid_layer_height(count: usize, columns: usize, available_w: u32) -> u32 {
    let rows = count.div_ceil(columns.max(1));
    let (_, card_h) = grid_card_size(available_w, columns);
    (rows.max(1) as u32 * (card_h + GRID_GAP as u32)) + 2 * GRID_LAYER_PAD as u32
}

/// Rows of slack either side of the on-screen band that [`focus_window`] keeps navigable. One
/// row is all a single d-pad step can reach; two is slack for the heading offsets that the
/// band arithmetic rounds through.
const FOCUS_WINDOW_ROWS: usize = 2;

/// The grid indices a d-pad move can reach right now: the on-screen band widened by
/// [`FOCUS_WINDOW_ROWS`], plus wherever focus currently sits.
///
/// The focus map is rebuilt per keypress, and it used to hold one rect per card in the
/// library — allocating and then scanning the whole library several times over for a move
/// that can only ever land one row away. Focus itself must always be in the window or
/// [`FocusMap::navigate`](crate::ui::focus::FocusMap::navigate) finds no origin to move from.
pub(crate) fn focus_window(
    available_w: u32,
    layout: GridLayout,
    grid_scroll: i32,
    screen_h: i32,
    focus: Option<usize>,
) -> std::ops::Range<usize> {
    let cols = layout.columns();
    let count = layout.len();
    let visible = visible_cards(available_w, layout, grid_scroll, screen_h, 0);
    let focus = focus.filter(|&i| i < count);
    // A band can be empty with the grid scrolled clear of the viewport; the focused card is
    // then the only anchor there is, and with no focus either there is nothing to navigate.
    let (lo, hi) = match (visible.is_empty(), focus) {
        (true, None) => return 0..0,
        (true, Some(i)) => (i, i + 1),
        (false, focus) => {
            let f = focus.unwrap_or(visible.start);
            (visible.start.min(f), visible.end.max(f + 1))
        }
    };
    let pad = FOCUS_WINDOW_ROWS * cols;
    // Row-aligned at both ends: a half row of candidates would make Left/Right along the
    // margin row behave differently from the same move one row in.
    let lo = lo.saturating_sub(pad) / cols * cols;
    let hi = ((hi + pad).div_ceil(cols) * cols).min(count);
    lo..hi
}

/// Inverse of [`unscrolled_card_rect`]: the grid index whose card covers `(x, y)` in
/// unscrolled grid space, or `None` for a gap, a heading band, or past the last row.
///
/// Two divisions and a row's worth of rect tests, one per band — the pointer path asked
/// every card in the library the same question on every motion event. The candidate row is
/// arithmetic but the answer still comes from [`unscrolled_card_rect`], so a point in the
/// grid's gutters matches nothing here exactly as it did before.
pub(crate) fn card_at_point(grid_x: i32, available_w: u32, layout: GridLayout, (x, y): (i32, i32)) -> Option<usize> {
    let cols = layout.columns();
    let count = layout.len();
    let (_, card_h) = grid_card_size(available_w, cols);
    let row_h = card_h as i32 + GRID_GAP;
    layout.row_bands().find_map(|(band, offset)| {
        let row = div_floor(y - GRID_TOP_Y - offset, row_h);
        let row = usize::try_from(row).ok().filter(|r| band.contains(r))?;
        (row * cols..((row + 1) * cols).min(count))
            .find(|&idx| unscrolled_card_rect(idx, grid_x, available_w, layout).contains_point((x, y)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::grid::Group;

    const COLUMNS: usize = 5;
    const AVAILABLE_W: u32 = 1920 - crate::ui::widgets::SIDEBAR_W;
    const SCREEN_H: i32 = 1080;

    /// Two sections, the first exactly one row wide — the pre-collections shape, and the
    /// one every offset here was tuned against.
    fn groups(count: usize) -> Vec<Group> {
        let pinned = COLUMNS.min(count);
        // Empty sections never reach the grid, so the fixture must not build one either.
        [("Pinned", 0, pinned), ("Library", pinned, count - pinned)]
            .into_iter()
            .filter(|&(_, _, len)| len > 0)
            .map(|(name, games_start, len)| Group {
                name: name.into(),
                len,
                games_start,
            })
            .collect()
    }

    fn window(count: usize, scroll: i32, focus: Option<usize>) -> std::ops::Range<usize> {
        let groups = groups(count);
        focus_window(AVAILABLE_W, GridLayout::new(&groups, COLUMNS), scroll, SCREEN_H, focus)
    }

    /// The invariant that silently freezes focus when broken: `FocusMap::navigate` needs the
    /// current focus in the map to have an origin to move from.
    #[test]
    fn the_window_always_contains_the_current_focus() {
        for count in [1usize, 5, 6, 23, 200] {
            for scroll in [-500, 0, 137, 4000, 100_000] {
                for focus in 0..count {
                    let w = window(count, scroll, Some(focus));
                    assert!(
                        w.contains(&focus),
                        "focus {focus} outside {w:?} (count {count}, scroll {scroll})"
                    );
                }
            }
        }
    }

    #[test]
    fn the_window_is_row_aligned_and_inside_the_grid() {
        for count in [1usize, 7, 23, 200] {
            for scroll in [0, 300, 4000] {
                let w = window(count, scroll, Some(count / 2));
                assert_eq!(w.start % COLUMNS, 0, "start {} not row-aligned", w.start);
                assert!(w.end == count || w.end % COLUMNS == 0, "end {} not row-aligned", w.end);
                assert!(w.end <= count);
                assert!(w.start <= w.end);
            }
        }
    }

    /// O(visible), not O(library): the window a keypress builds must not grow with the
    /// library behind it.
    #[test]
    fn the_window_does_not_grow_with_the_library() {
        let small = window(60, 0, Some(0)).len();
        let large = window(5_000, 0, Some(0)).len();
        assert_eq!(small, large);
    }

    #[test]
    fn a_focus_past_the_end_is_ignored_rather_than_widening_the_window() {
        let w = window(20, 0, Some(9_999));
        assert!(w.end <= 20);
    }

    #[test]
    fn an_empty_grid_has_nothing_to_navigate() {
        assert_eq!(
            focus_window(AVAILABLE_W, GridLayout::new(&[], COLUMNS), 0, SCREEN_H, None),
            0..0
        );
    }

    /// Scrolled clear of the viewport there is no visible band, so the focused card is the
    /// only anchor left — and with no focus, nothing at all.
    #[test]
    fn a_grid_scrolled_off_screen_keeps_only_the_focused_card() {
        let far = 1_000_000;
        let w = window(200, far, Some(100));
        assert!(w.contains(&100));
        assert!(window(200, far, None).is_empty());
    }
}
