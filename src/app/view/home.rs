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
    /// Extra vertical offset for the row grid index `idx` sits in: whatever headings and gaps
    /// stack above it.
    fn row_offset(&self, idx: usize, columns: usize) -> i32 {
        let pinned_block = if self.pinned_heading {
            SECTION_HEADING_H + PINNED_SECTION_GAP
        } else {
            0
        };
        let heading = |shown: bool| if shown { SECTION_HEADING_H } else { 0 };
        if idx / columns.max(1) >= self.pinned_rows {
            pinned_block + heading(self.library_heading)
        } else {
            heading(self.pinned_heading)
        }
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
