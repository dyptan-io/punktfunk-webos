//! The shared confirm-dialog shape: its geometry, its hit test, and its shell tile.
//!
//! Menu confirmations and the runtime's disconnect/quit prompts both use it. The runtime has
//! no `App`, so the geometry lives here where both paths can match by construction.
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

impl Canvas<'_, '_> {
    /// Paints the shared confirmation card and its unfocused buttons.
    pub fn confirm_dialog(
        &mut self,
        title: &str,
        subtitle: &str,
        subtitle_color: Color,
        buttons: &[ConfirmButton<'_>; 2],
        hover_close: bool,
        surface: ConfirmSurface,
    ) -> Result<()> {
        let (card, content) = confirm_dialog_layout(self.screen_w, self.screen_h, self.fonts, subtitle);
        match surface {
            ConfirmSurface::Glass => self.modal_shell(card, hover_close)?,
            ConfirmSurface::Opaque => {
                self.painter.modal_card(card);
                let color = if hover_close { palette().text } else { palette().muted };
                self.icon(modal_close_rect(card), icons().close, color)?;
            }
        }
        self.modal_header(card, title, palette().text, subtitle, subtitle_color)?;
        self.render(ConfirmButtons::new(buttons), content)
    }
}

#[derive(Clone, Copy)]
pub enum ConfirmSurface {
    Glass,
    Opaque,
}

/// Confirm dialog shell (full-screen tile), using the same card, header, and unfocused-button
/// painter as the app's confirmation modals. The focused button composites on top separately.
pub struct ConfirmDialogShellTile<'a> {
    pub screen_w: u32,
    pub screen_h: u32,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub buttons: &'a [ConfirmButton<'a>; 2],
    /// Draw the card as glass when the caller pushes a matching `DrawCmd::Frost` under it.
    /// In-stream prompts use [`ConfirmSurface::Opaque`]: NDL video lives on a
    /// hardware plane *below* the SDL surface, so it is not in the framebuffer and there is
    /// nothing there to blur.
    pub surface: ConfirmSurface,
}

impl Widget for ConfirmDialogShellTile<'_> {
    fn render(self, _area: Rect, c: &mut Canvas) -> Result<()> {
        // These dialogs are remote/controller-driven, so the X is always unhovered.
        c.confirm_dialog(
            self.title,
            self.subtitle,
            palette().muted,
            self.buttons,
            false,
            self.surface,
        )
    }
}

impl TileWidget for ConfirmDialogShellTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.screen_w, self.screen_h)
    }
}
