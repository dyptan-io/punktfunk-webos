//! The two-button confirm modal's buttons — a primary action plus Cancel, shared by every
//! confirm screen (forget host, send logs, stop streaming, quit app) so their geometry and
//! their focus treatment can't drift apart.
use crate::ui::prelude::*;
use anyhow::Result;

/// Confirm button with identity color (full when focused, dimmed when not).
pub struct ConfirmButton<'a> {
    pub icon: Option<&'a str>,
    pub label: &'a str,
    pub color: Color,
}

/// A primary action button plus a Cancel — the pair every confirm modal shares
/// (forget host, send logs, stop streaming, quit app), so their `ConfirmButton`
/// data can't drift apart. Index 0 is the action, index 1 is Cancel (the safe
/// default focus).
pub fn confirm_buttons(icon: Option<&'static str>, label: &'static str, color: Color) -> [ConfirmButton<'static>; 2] {
    [
        ConfirmButton { icon, label, color },
        ConfirmButton {
            icon: None,
            label: "Cancel",
            color: palette().text,
        },
    ]
}

/// Gap between the two buttons in a [`ConfirmButtons`] row.
const CONFIRM_BUTTON_GAP: i32 = 20;

/// Confirm button metrics derived from label font height — keeps sizing consistent
/// between drawing and measurement.
fn confirm_button_metrics(raster: &dyn TextRaster, font: FontId) -> (u32, i32, i32) {
    let line_h = raster.height(font).max(1);
    ((line_h * 2 / 3).max(1) as u32, (line_h / 3).max(1), (line_h / 2).max(1))
}

/// Button `index`'s rect within a confirm button row: two equal halves, one gap between.
pub fn confirm_button_rect(content: Rect, index: usize) -> Rect {
    Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
        .gap(CONFIRM_BUTTON_GAP)
        .split(content)[index.min(1)]
}

/// The pair of buttons every confirm modal ends with — see [`confirm_buttons`] for the
/// pair itself. Renders both unfocused: the focused one composites over this from
/// [`render_confirm_button_tile`], zoom-animated in `app::App`.
pub struct ConfirmButtons<'a> {
    buttons: &'a [ConfirmButton<'a>; 2],
}

impl<'a> ConfirmButtons<'a> {
    pub fn new(buttons: &'a [ConfirmButton<'a>; 2]) -> Self {
        Self { buttons }
    }
}

impl Widget for ConfirmButtons<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        for (i, button) in self.buttons.iter().enumerate() {
            c.confirm_button(button, false, confirm_button_rect(area, i))?;
        }
        Ok(())
    }
}

/// One focused button as a tile, composited over the shell.
pub struct ConfirmButtonTile<'a> {
    pub button: &'a ConfirmButton<'a>,
    pub w: u32,
    pub h: u32,
}

impl Widget for ConfirmButtonTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.confirm_button(self.button, true, area.inflate(-ROW_TILE_PAD))
    }
}

impl TileWidget for ConfirmButtonTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.w, self.h, ROW_TILE_PAD)
    }
}

impl Canvas<'_, '_> {
    /// Draws one confirm button at normal size, focused or not.
    pub fn confirm_button(&mut self, button: &ConfirmButton<'_>, focused: bool, rect: Rect) -> Result<()> {
        self.painter.selectable_fixed(rect, focused);
        let color = if focused { button.color } else { palette().muted };

        // Every inset here is derived from the label font's own line height, which
        // `load_font` already scales by the panel's height — the button's width scales with
        // the screen too, so a hardcoded icon inset does not stay in proportion to either.
        // It used to be a fixed `20 + 26 + 12`, which left "Stop streaming" more label than
        // button below 4K (~117px of room for ~154px of text at 720p) and ran it past the
        // right edge, because nothing clamped the label either.
        let font = self.fonts.label;
        let line_h = self.fonts.raster.height(font).max(1);
        let (icon_size, icon_gap, side_pad) = confirm_button_metrics(self.fonts.raster, font);

        // Icon and label are centred as one group, the same way a label without an icon
        // was already centred on its own — and the label is held to whatever the icon
        // leaves, so no label can overflow the button regardless of resolution.
        let leading = match button.icon {
            Some(_) => icon_size + icon_gap as u32,
            None => 0,
        };
        let budget = rect.width().saturating_sub(2 * side_pad as u32).saturating_sub(leading);
        let label_w = self.fonts.raster.measure(font, button.label).0.min(budget);
        let start_x = rect.x() + (rect.width() as i32 - (leading + label_w) as i32) / 2;

        if let Some(icon) = button.icon {
            let icon_rect = Rect::new(
                start_x,
                rect.y() + (rect.height() as i32 - icon_size as i32) / 2,
                icon_size,
                icon_size,
            );
            self.icon(icon_rect, icon, color)?;
        }
        self.text_faded(
            font,
            button.label,
            start_x + leading as i32,
            rect.y() + (rect.height() as i32 - line_h) / 2,
            budget,
            color,
        )?;
        Ok(())
    }
}
