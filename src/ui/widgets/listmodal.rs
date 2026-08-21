use anyhow::Result;

use crate::ui::prelude::*;

/// Wider than the confirm-style modals ([`SIMPLE_MODAL_WIDTH_FRAC`]) — these
/// hold full rows with icons and hint text, not a sentence and two buttons.
pub const LIST_MODAL_WIDTH_FRAC: f32 = 0.46;
/// Gap between the header's last line and the first row.
const HEADER_GAP: i32 = 24;
/// Space left below the last row, inside the card.
const BOTTOM_PAD: i32 = 24;
/// Left/right inset of the row list within the card.
const SIDE_PAD: i32 = 32;

/// The card's vertical stack — header, gap, row list, bottom pad — and the side insets the
/// list sits in. The one place a list modal's geometry lives: [`list_modal_card_rect`] reads
/// its total length for the card height, [`list_modal_content_rect`] reads the row slot out
/// of the same split, so the two cannot drift.
fn card_layout(fonts: &Fonts, card: Rect, subtitle: &str, row_count: usize) -> Layout {
    let header_h = (modal_header_end_y(fonts, card, subtitle) - card.y()).max(0) as u32;
    Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Length(HEADER_GAP as u32),
        Constraint::Length(row_count as u32 * focus_row_stride()),
        Constraint::Length(BOTTOM_PAD as u32),
    ])
}

/// The card rect for a list modal with `row_count` rows and this `subtitle` (whose
/// wrapped height moves everything below it). Mirrors `simple_modal_card`'s
/// probe trick: measure against a zero-height card at the final width, then place it.
pub fn list_modal_card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str, row_count: usize) -> Rect {
    let w = (screen_w as f32 * LIST_MODAL_WIDTH_FRAC).round() as u32;
    let probe = Rect::new(0, 0, w, 0);
    let height = card_layout(fonts, probe, subtitle, row_count).total_length();
    modal_card_rect(screen_w, screen_h, LIST_MODAL_WIDTH_FRAC, height)
}

/// Where the row list starts inside `card` — the rect `focus_row_rect` indexes into,
/// so `draw_list` can position the focused-row tile without re-rendering the header.
pub fn list_modal_content_rect(card: Rect, fonts: &Fonts, subtitle: &str, row_count: usize) -> Rect {
    card_layout(fonts, card, subtitle, row_count).split(card)[2].inset_x(SIDE_PAD as u32)
}

/// A whole list-modal screen: header plus its [`FocusRows`], inside the card `area`.
///
/// `area` is the card, not the row list — the content rect is derived from it and the
/// wrapped subtitle's height, the one place that geometry lives ([`list_modal_content_rect`]).
pub struct ListModal<'a> {
    title: &'a str,
    subtitle: &'a str,
    rows: &'a [FocusRow],
}

impl<'a> ListModal<'a> {
    pub fn new(title: &'a str, subtitle: &'a str, rows: &'a [FocusRow]) -> Self {
        Self { title, subtitle, rows }
    }
}

impl StatefulWidget for ListModal<'_> {
    type State = FocusRowsState;

    fn render(self, area: Rect, c: &mut Canvas, state: &mut Self::State) -> Result<()> {
        c.modal_header(area, self.title, palette().text, self.subtitle, palette().muted)?;
        let content = list_modal_content_rect(area, c.fonts, self.subtitle, self.rows.len());
        c.render_stateful(FocusRows::new(self.rows), content, state)
    }
}

impl Canvas<'_, '_> {
    /// A whole list-modal screen with nothing focused: card chrome, then the header and
    /// rows. Every list-modal screen's `render` is this one call — see `app::view::hostmenu`;
    /// the focused row composites on top as its own tile.
    pub fn list_modal_screen(
        &mut self,
        card: Rect,
        title: &str,
        subtitle: &str,
        rows: &[FocusRow],
        hover_close: bool,
    ) -> Result<()> {
        self.modal_shell(card, hover_close)?;
        self.render_stateful(
            ListModal::new(title, subtitle, rows),
            card,
            &mut FocusRowsState::unfocused(),
        )
    }
}

/// Navigate within a list, wrapping. Returns true if focus moved.
///
/// Takes a [`Dir`] rather than an app input event: `ui` draws, it does not know what a
/// button press means. The caller maps its own event vocabulary once, at its boundary.
pub fn list_nav(focused: &mut usize, len: usize, dir: Option<Dir>) -> bool {
    if len == 0 {
        return false;
    }
    match dir {
        Some(Dir::Up) => {
            *focused = if *focused == 0 { len - 1 } else { *focused - 1 };
            true
        }
        Some(Dir::Down) => {
            *focused = (*focused + 1) % len;
            true
        }
        _ => false,
    }
}
