//! The scrolling row-list modal: a card whose shell is one tile and whose rows are one tile
//! each, cropped to a viewport that scrolls under edge fades.
//!
//! Settings is the pattern's first client and Collections its second. The split exists because
//! a plain `ui::widgets::ListModal` bakes its header *and every row* into `tile::MODAL`, and a
//! list long enough to scroll makes that card taller than the screen — re-baking it per scroll
//! step is the 25-60ms armv7 raster this avoids.
//!
//! Geometry only; which rows a screen has is its own module's business.
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::Canvas;
use anyhow::Result;

/// Card space above the row list: title, divider, and their padding.
pub(crate) const CHROME_TOP: u32 = 120;

/// Card space below the row list. Just enough to clear the card's rounded corner, so the list
/// runs to the card's edge and the bottom fade dissolves into it.
///
/// Anything more shows as a band of flat card background under the fade — the fade already
/// *is* the bottom edge, so padding beneath it reads as dead space rather than breathing room.
pub(crate) const CHROME_BOTTOM: u32 = 16;

/// Minimum gap between the card and the screen edges, top and bottom combined.
///
/// Trimmed from 160 when the second peek strip arrived: two 44px peeks cost a whole visible
/// row out of a 1080p budget, and the card had more inset to spare than the list had rows.
pub(crate) const EDGE_MARGIN: u32 = 120;

/// How much of the adjacent row stays visible past each edge of the viewport while the list
/// overflows — the strip an edge fade dissolves. Applied to the top and bottom alike.
///
/// Load-bearing, not decoration: a viewport edge landing exactly on a row boundary has
/// nothing but card background in its outermost pixels (unfocused rows draw no fill of their
/// own), so a fade there blends the card colour into the card colour and is *mathematically
/// invisible*. Both cuts have to land mid-row for either fade to read at all — which is also
/// why the rendered offset is biased by one peek (see `App::sync_modal_scroll`) instead of
/// sitting on the row grid.
///
/// Independent of `ui::widgets::SCROLL_FADE_H`, which is taller: this is how much of the next
/// row is *exposed*, while that is how far the fade reaches back over what is already visible.
/// Deep enough to expose a row's icon and label, which sit in the middle third of its height —
/// a shallower peek shows only the row's internal padding, i.e. nothing to dissolve.
pub(crate) const PEEK: u32 = 44;

/// Width the title line keeps clear on the right for the close button, which overhangs the
/// content column (see `ui::widgets::modal_close_rect`).
const CLOSE_RESERVE: u32 = 48;

/// Left/right inset of the card's own content column — the title, the rule and the row list
/// all start here.
pub(crate) const SIDE_PAD: u32 = 40;

/// Radius of the dot separating the heading from its suffix, and the gap it keeps on each side.
const SEP_DOT_R: i32 = 3;
const SEP_DOT_GAP: i32 = 12;

/// Pixel stride between two consecutive rows — the divisor all the scroll arithmetic runs on.
pub(crate) fn stride() -> i32 {
    ui::widgets::FOCUS_ROW_H as i32 + ui::widgets::FOCUS_ROW_GAP
}

/// How many of `total` rows are *fully* visible. Capped at the live row count, so a hidden row
/// leaves no empty slot.
///
/// When the list overflows, one row's worth of budget is spent on [`PEEK`] instead — the
/// partially-visible sliver the bottom fade dissolves. Computed without the peek first, because
/// a list that fits entirely has nothing below to peek at and should not give up the space.
pub(crate) fn visible_rows(total: usize, screen_h: u32) -> usize {
    let stride = ui::widgets::focus_row_stride();
    let budget = screen_h.saturating_sub(CHROME_TOP + CHROME_BOTTOM + EDGE_MARGIN);
    if (budget / stride) as usize >= total {
        return total.max(1);
    }
    // Both peeks come out of the budget, not just the bottom one — see [`PEEK`].
    ((budget.saturating_sub(2 * PEEK) / stride) as usize).clamp(1, total)
}

/// Height of the scrolling viewport: the fully-visible rows plus a peek strip past each edge
/// while the list overflows. Deliberately *not* a whole multiple of the row stride when
/// scrolling — see [`PEEK`].
pub(crate) fn content_h(total: usize, screen_h: u32) -> u32 {
    let visible = visible_rows(total, screen_h);
    let peeks = if visible < total { 2 * PEEK } else { 0 };
    visible as u32 * ui::widgets::focus_row_stride() + peeks
}

/// Settings' card width. Wider than a plain list modal: its rows carry a label, a value and
/// an override hint on one line, and the scroll indicator rides the right edge.
pub(crate) const SETTINGS_WIDTH_FRAC: f32 = 0.62;

/// Collections' card width — the host menu's, since its rows hold the same thing a list
/// modal's do (an icon, a name, a short count) and nothing that wants Settings' extra column.
pub(crate) const COLLECTIONS_WIDTH_FRAC: f32 = ui::widgets::LIST_MODAL_WIDTH_FRAC;

/// Card and content rects, shared by render and hit-test. One split, read twice: the card's
/// height is what its own stack adds up to, and the viewport is the middle slot of it.
pub(crate) fn layout(total: usize, screen_w: u32, screen_h: u32, width_frac: f32) -> (Rect, Rect) {
    let stack = ui::layout::Layout::vertical([
        ui::layout::Constraint::Length(CHROME_TOP),
        ui::layout::Constraint::Length(content_h(total, screen_h)),
        ui::layout::Constraint::Length(CHROME_BOTTOM),
    ]);
    let card = ui::widgets::modal_card_rect(screen_w, screen_h, width_frac, stack.total_length());
    (card, content_column(stack.split(card)[1]))
}

/// The horizontal inset every element of the card shares.
pub(crate) fn content_column(row: Rect) -> Rect {
    row.inset_x(SIDE_PAD)
}

/// Where a dropdown opened from row `row` anchors its option overlay — one row below it.
///
/// Positioned from a pixel scroll offset rather than a viewport-local row index, since a
/// gliding list puts its rows at continuous offsets. `scroll_px` of 0 is the unscrolled case.
pub(crate) fn dropdown_overlay_rect_at_px(content: Rect, row: usize, scroll_px: i32) -> Rect {
    let y = ui::widgets::focus_row_rect_at_px(content, row + 1, scroll_px).y();
    Rect::new(content.x(), y, content.width(), 0)
}

/// The shell only: card chrome, title and rule. The row list is its own scroll-content tile
/// and any open dropdown its own overlay tile, so neither scrolling nor navigating options
/// re-rasterizes this.
///
/// `suffix` is appended after the title in the muted text colour — the per-game settings
/// screen names its game there, dimmer so the heading still reads as the heading.
pub(crate) fn render(
    c: &mut Canvas,
    total: usize,
    width_frac: f32,
    title: &str,
    suffix: Option<&str>,
    hover_close: bool,
) -> Result<()> {
    let (card, _content) = layout(total, c.screen_w, c.screen_h, width_frac);
    let column = content_column(card);
    let baseline = card.y() + 36;
    c.modal_shell(card, hover_close)?;
    c.text(c.fonts.label, title, column.x(), baseline, ui::theme::palette().text)?;
    if let Some(suffix) = suffix {
        let font = c.fonts.label;
        let title_w = c.fonts.raster.measure(font, title).0;
        // A dot between the heading and the name: at this size a space reads as a line break
        // waiting to happen, and the two words are not one phrase.
        let dot_x = column.x() + title_w as i32 + SEP_DOT_GAP;
        c.painter.fill_rounded_rect(
            Rect::new(
                dot_x,
                baseline + c.fonts.raster.height(font) / 2 - SEP_DOT_R,
                2 * SEP_DOT_R as u32,
                2 * SEP_DOT_R as u32,
            ),
            SEP_DOT_R,
            ui::theme::palette().muted,
        );
        let used = title_w + (2 * SEP_DOT_GAP + 2 * SEP_DOT_R) as u32;
        // Faded rather than clipped: a long name must not run under the close button.
        let avail = column.width().saturating_sub(used + CLOSE_RESERVE);
        c.text_faded(
            font,
            suffix,
            column.x() + used as i32,
            baseline,
            avail,
            ui::theme::palette().muted,
        )?;
    }
    c.painter.rule(column.x(), card.y() + 88, column.width());
    Ok(())
}
