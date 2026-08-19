//! Home screen geometry: the game grid (columns, card rects, scroll extent) and its
//! pinned/library section split. Navigation/selection logic lives in `app::state::home`;
//! `App` supplies `grid_sections`/`grid_scroll` from there.
use crate::ui::render::Rect;

/// The grid's vertical section shape: how many rows the pinned block takes, and which of the
/// two headings ("Pinned" / "Library") are drawn — each pushes every row below it down by its
/// own height, so nothing that positions a card can ignore them.
///
/// A heading is drawn iff its section has cards (`GridLayout::sections`), so neither can end up
/// naming an empty block or hanging under the last row. "Library" therefore shows even with
/// nothing pinned, where it is the only heading and there is no gap to bridge.
#[derive(Clone, Copy, Default)]
pub(crate) struct GridSections {
    pub pinned_rows: usize,
    pub pinned_heading: bool,
    pub library_heading: bool,
}

/// Height reserved for one section heading: one line of the title font plus air under it. A
/// constant rather than a measured line — the grid's geometry is used from the pointer path
/// and the focus map, neither of which has a rasterizer to hand.
pub const SECTION_HEADING_H: i32 = 60;
/// Of that height, the air between the heading and the cards under it.
pub const SECTION_HEADING_PAD: i32 = 14;
impl GridSections {
    /// Extra vertical offset carried by grid row `row`: whatever headings and gaps stack above
    /// it. Non-decreasing in `row`, which is what makes a card's y monotone in its index — see
    /// [`visible_cards`].
    fn row_offset_at(&self, row: usize) -> i32 {
        let heading = |shown: bool| if shown { SECTION_HEADING_H } else { 0 };
        if row >= self.pinned_rows {
            let pinned_block = if self.pinned_heading {
                SECTION_HEADING_H + PINNED_SECTION_GAP
            } else {
                0
            };
            pinned_block + heading(self.library_heading)
        } else {
            heading(self.pinned_heading)
        }
    }

    /// [`row_offset_at`](Self::row_offset_at) for the row grid index `idx` sits in.
    fn row_offset(&self, idx: usize, columns: usize) -> i32 {
        self.row_offset_at(idx / columns.max(1))
    }

    /// The grid's rows split into the two bands that share a vertical offset, each with its
    /// own. Every row inside a band is uniformly spaced, so a band's visible rows are a
    /// closed-form range.
    fn row_bands(&self, rows: usize) -> [(std::ops::Range<usize>, i32); 2] {
        let split = self.pinned_rows.min(rows);
        [
            (0..split, self.row_offset_at(0)),
            (split..rows, self.row_offset_at(self.pinned_rows)),
        ]
    }

    /// What the sections add to the grid's total height — the offset its last row carries.
    pub fn total_extra(&self) -> i32 {
        // `library_heading` is exactly "the library section has rows", so it also says which
        // section the last row is in. With everything pinned that row carries the pinned
        // heading alone; counting the gap too would let the grid scroll past its content.
        self.row_offset(if self.library_heading { self.pinned_rows } else { 0 }, 1)
    }
}

/// [`grid_card_rect`] translated by the sections above it — everything except the current
/// scroll offset, which [`scrolled_card_rect`] applies on top.
pub(crate) fn unscrolled_card_rect(
    idx: usize,
    columns: usize,
    grid_x: i32,
    available_w: u32,
    sections: GridSections,
) -> Rect {
    grid_card_rect(idx, columns, grid_x, available_w).offset(0, sections.row_offset(idx, columns))
}

/// [`unscrolled_card_rect`] translated by the current scroll offset — every draw-list card
/// position starts from this.
pub(crate) fn scrolled_card_rect(
    idx: usize,
    columns: usize,
    grid_x: i32,
    available_w: u32,
    sections: GridSections,
    grid_scroll: i32,
) -> Rect {
    unscrolled_card_rect(idx, columns, grid_x, available_w, sections).offset(0, -grid_scroll)
}

/// The headings' text. Here rather than at the draw site: the tile that rasterizes them and
/// the tile cache's key both want the same strings.
pub const SECTION_PINNED_LABEL: &str = "Pinned";
pub const SECTION_LIBRARY_LABEL: &str = "Library";

/// The band a section heading is drawn in, scrolled like any other grid content: directly
/// above `first_idx`, the first card of the section it names, so heading and block move
/// together. Positioned off [`scrolled_card_rect`], which is what keeps them aligned.
pub(crate) fn section_heading_rect(
    first_idx: usize,
    columns: usize,
    grid_x: i32,
    available_w: u32,
    sections: GridSections,
    grid_scroll: i32,
) -> Rect {
    let row = scrolled_card_rect(first_idx, columns, grid_x, available_w, sections, grid_scroll);
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
    count: usize,
    columns: usize,
    available_w: u32,
    sections: GridSections,
    grid_scroll: i32,
    screen_h: i32,
    pad: i32,
) -> std::ops::Range<usize> {
    let cols = columns.max(1);
    let (_, card_h) = grid_card_size(available_w, columns);
    let row_h = card_h as i32 + GRID_GAP;
    let rows = count.div_ceil(cols);
    let mut first = rows;
    let mut last = 0;
    for (band, offset) in sections.row_bands(rows) {
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

/// Gap between the pinned block's last row and the "Library" heading under it — what makes
/// the two sections read as separate blocks (there is no dividing line; the headings say it).
pub const PINNED_SECTION_GAP: i32 = 32;

/// `clamp(2, available_w / (min_card_w + gap), 5)` — moonlight-tv's own formula.
pub fn grid_columns(available_w: u32) -> usize {
    let cols = (available_w / (CARD_MIN_W + GRID_GAP as u32)).max(1);
    cols.clamp(2, 5) as usize
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
    count: usize,
    columns: usize,
    available_w: u32,
    sections: GridSections,
    grid_scroll: i32,
    screen_h: i32,
    focus: Option<usize>,
) -> std::ops::Range<usize> {
    let cols = columns.max(1);
    let visible = visible_cards(count, columns, available_w, sections, grid_scroll, screen_h, 0);
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
pub(crate) fn card_at_point(
    count: usize,
    columns: usize,
    grid_x: i32,
    available_w: u32,
    sections: GridSections,
    (x, y): (i32, i32),
) -> Option<usize> {
    let cols = columns.max(1);
    let (_, card_h) = grid_card_size(available_w, columns);
    let row_h = card_h as i32 + GRID_GAP;
    let rows = count.div_ceil(cols);
    sections.row_bands(rows).into_iter().find_map(|(band, offset)| {
        let row = div_floor(y - GRID_TOP_Y - offset, row_h);
        let row = usize::try_from(row).ok().filter(|r| band.contains(r))?;
        (row * cols..((row + 1) * cols).min(count))
            .find(|&idx| unscrolled_card_rect(idx, columns, grid_x, available_w, sections).contains_point((x, y)))
    })
}
