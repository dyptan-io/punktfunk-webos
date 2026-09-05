//! Sidebar panel metrics and the generic nav-row widgets built on them: an icon + label
//! row with optional selection tint, and the ⋯ actions button a row can carry.
//!
//! This app's own host list is `app::view::sidebar`, which composes these.
use crate::ui::prelude::*;
use anyhow::Result;

// Sized for a 10-foot TV viewing distance, not a desktop/phone screen.
pub const SIDEBAR_W: u32 = 460;
pub const SIDEBAR_PAD: i32 = 24;
pub const SIDEBAR_ROW_H: u32 = 76;
pub const SIDEBAR_ROW_GAP: i32 = 10;

/// Size of the "more actions" (⋯) hit target at the right end of a host row. Square,
/// and generous — this is a 10-foot UI driven by a wobbly pointer, so the touch target
/// is deliberately much larger than the glyph drawn inside it.
pub const SIDEBAR_MENU_BTN: u32 = 52;
/// A row's ⋯ button. The single-button case of a focus row's trailing buttons — one
/// geometry, so the sidebar's ⋯ and a list row's cannot drift apart.
pub fn sidebar_menu_button_rect(row_rect: Rect) -> Rect {
    super::trailing_button_rect(row_rect, 1, 0, false)
}

/// The glyph itself, inset within a row button's target.
const ROW_BUTTON_GLYPH: u32 = 26;

impl Canvas<'_, '_> {
    /// A row's trailing button: a plate when focused or held open (`active`), else just the
    /// glyph. `active` reads brighter still — a mode has to look different from a focus.
    pub fn row_button(&mut self, btn: Rect, icon: &str, row_focused: bool, focused: bool, active: bool) -> Result<()> {
        if focused || active {
            let plate = if active {
                palette().accent_bright
            } else {
                palette().accent
            };
            self.painter.fill_rounded_rect(btn, (btn.height() / 2) as i32, plate);
        }
        let glyph = ROW_BUTTON_GLYPH;
        let glyph_rect = Rect::new(
            btn.x() + (btn.width() as i32 - glyph as i32) / 2,
            btn.y() + (btn.height() as i32 - glyph as i32) / 2,
            glyph,
            glyph,
        );
        let color = if focused || active || row_focused {
            palette().text
        } else {
            palette().muted
        };
        self.icon(glyph_rect, icon, color)
    }
}
