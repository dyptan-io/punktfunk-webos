//! The shared confirm-dialog shape: its geometry, its hit test, and its shell tile.
//!
//! Four modals wear it — forget host, send logs, wake, the in-stream disconnect/quit prompt —
//! and the last of those runs in `runtime`, which has no `App`. So the geometry lives here,
//! where both sides can reach it, and all four match by construction.
use crate::ui::prelude::*;
use anyhow::Result;

/// Gap above and below a confirm dialog's button row.
const CONFIRM_DIALOG_GAP: u32 = 32;
/// Height of that row.
const CONFIRM_BUTTON_ROW_H: u32 = 72;
/// Left/right inset of the dialog's content column.
const CONFIRM_DIALOG_SIDE_PAD: u32 = 32;

/// A confirm dialog's vertical stack: header, gap, button row, bottom pad. Read for the card's
/// height and, once placed, for the button row's rect — the same split both times.
fn confirm_dialog_stack(fonts: &Fonts, card: Rect, subtitle: &str) -> Layout {
    let header_h = (modal_header_end_y(fonts, card, subtitle) - card.y()).max(0) as u32;
    Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Length(CONFIRM_DIALOG_GAP),
        Constraint::Length(CONFIRM_BUTTON_ROW_H),
        Constraint::Length(CONFIRM_DIALOG_GAP),
    ])
}

/// Confirm dialog card rect: [`SIMPLE_MODAL_WIDTH_FRAC`] wide, height driven by the wrapped
/// subtitle plus room for the button row. Split from [`confirm_dialog_layout`] so callers that
/// only need the card (hit-testing the close button, positioning the shell) skip the second
/// subtitle wrap the button-row rect would cost.
pub fn confirm_dialog_card(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str) -> Rect {
    simple_modal_card(screen_w, screen_h, |probe| {
        confirm_dialog_stack(fonts, probe, subtitle).total_length()
    })
}

/// Confirm dialog card + button-row rects.
pub fn confirm_dialog_layout(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str) -> (Rect, Rect) {
    let card = confirm_dialog_card(screen_w, screen_h, fonts, subtitle);
    let content = confirm_dialog_stack(fonts, card, subtitle).split(card)[2].inset_x(CONFIRM_DIALOG_SIDE_PAD);
    (card, content)
}

/// Button index under `(x, y)` within a confirm dialog's button-row `content` rect, or `None` off
/// both buttons — the shared hit test the `App` confirm modals' hover arms and the in-stream
/// Disconnect dialog use, so pointer focus behaves identically.
pub fn confirm_button_at(content: Rect, x: i32, y: i32) -> Option<usize> {
    (0..2).find(|&i| confirm_button_rect(content, i).contains_point((x, y)))
}

/// Confirm dialog shell (full-screen tile): the same card + close (X) + header + unfocused
/// buttons that `Canvas::modal_shell`/`Canvas::modal_header` paint for the forget-host and
/// send-logs modals — replicated here because the streaming/quit loops have no `App`. The focused
/// button composites on top as its own small tile (shell/focus-tile split).
pub struct ConfirmDialogShellTile<'a> {
    pub screen_w: u32,
    pub screen_h: u32,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub buttons: &'a [ConfirmButton<'a>; 2],
}

impl Widget for ConfirmDialogShellTile<'_> {
    fn render(self, _area: Rect, c: &mut Canvas) -> Result<()> {
        let (card, content) = confirm_dialog_layout(self.screen_w, self.screen_h, c.fonts, self.subtitle);
        c.painter.modal_card(card);
        // These dialogs are remote/controller-driven (no local pointer over the overlay), so the
        // X is a visual affordance only — always in the unhovered color.
        c.icon(modal_close_rect(card), icons().close, theme().muted)?;
        c.modal_header(card, self.title, theme().text, self.subtitle, theme().muted)?;
        c.render(ConfirmButtons::new(self.buttons), content)
    }
}

impl TileWidget for ConfirmDialogShellTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.screen_w, self.screen_h)
    }
}
