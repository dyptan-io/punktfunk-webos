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
/// The glyph itself, inset within that target.
const SIDEBAR_MENU_GLYPH: u32 = 26;

/// The ⋯ actions button's rect within a host row. Right-aligned inside the row, so it
/// reads as belonging to that host rather than to the panel.
/// Size and left inset of a nav row's leading icon — the presence dot a host row badges
/// onto that icon is placed from the same rect (see `app::view::sidebar::draw_host_row`),
/// so the two can't drift apart.
pub const SIDEBAR_ICON_SIZE: u32 = 30;
const SIDEBAR_ICON_PAD: i32 = 20;

/// The leading icon's rect within an already-inflated row rect.
pub fn sidebar_icon_rect(drawn: Rect) -> Rect {
    Rect::new(
        drawn.x() + SIDEBAR_ICON_PAD,
        drawn.y() + (drawn.height() as i32 - SIDEBAR_ICON_SIZE as i32) / 2,
        SIDEBAR_ICON_SIZE,
        SIDEBAR_ICON_SIZE,
    )
}

pub fn sidebar_menu_button_rect(row_rect: Rect) -> Rect {
    let inset = 10i32;
    Rect::new(
        row_rect.right() - SIDEBAR_MENU_BTN as i32 - inset,
        row_rect.y() + (row_rect.height() as i32 - SIDEBAR_MENU_BTN as i32) / 2,
        SIDEBAR_MENU_BTN,
        SIDEBAR_MENU_BTN,
    )
}

impl Painter {
    /// A selectable row with optional selection highlighting. When focused, shows
    /// the full card with shadow and zoom. When selected (but not focused), shows a
    /// subtle background. When neither, shows no background.
    fn selectable_with_selection(&mut self, rect: Rect, focused: bool, selected: bool) -> Rect {
        let r = self.selectable(rect, focused);
        if !focused && selected {
            let selected_bg = Color::RGBA(0x2b, 0x21, 0x48, 0x40);
            self.fill_rounded_rect(r, CARD_RADIUS, selected_bg);
        }
        r
    }
}

/// A nav row: left-aligned icon + label, focus-coloured, with an optional selection tint
/// and an optional ⋯ actions button.
///
/// Built by value so a caller states only what differs from the plain row —
/// `SidebarRow::new(icon, label).selected(true).with_menu(false)`. The app's own host list
/// composes these; see `app::view::sidebar`.
pub struct SidebarRow<'a> {
    glyph: &'a str,
    label: &'a str,
    focused: bool,
    selected: bool,
    /// Width kept clear at the row's right end, so a long label can't run under
    /// whatever sits there (the ⋯ button, a presence dot).
    reserve_right: u32,
}

impl<'a> SidebarRow<'a> {
    pub fn new(glyph: &'a str, label: &'a str) -> Self {
        Self {
            glyph,
            label,
            focused: false,
            selected: false,
            reserve_right: 0,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Subtle background when this row is the current selection but not focused.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// See [`SidebarRow::reserve_right`].
    pub fn reserve_right(mut self, px: u32) -> Self {
        self.reserve_right = px;
        self
    }
}

impl Widget for SidebarRow<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let drawn = c.painter.selectable_with_selection(area, self.focused, self.selected);
        let color = if self.focused { theme().text } else { theme().muted };
        c.icon(sidebar_icon_rect(drawn), self.glyph, color)?;
        // Ellipsized to prevent overflow; `reserve_right` keeps it clear of the ⋯ button.
        let text_x = SIDEBAR_ICON_PAD + SIDEBAR_ICON_SIZE as i32 + 16;
        let max_w = drawn.width().saturating_sub(text_x as u32 + 20 + self.reserve_right);
        let label = ellipsize(c.fonts.raster, c.fonts.label, self.label, max_w);
        let font = c.fonts.label;
        let y = drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(font)) / 2;
        c.text(font, &label, drawn.x() + text_x, y, color)?;
        Ok(())
    }
}

impl Canvas<'_, '_> {
    /// The ⋯ button itself: a rounded highlight plate once it has focus, then the glyph.
    pub fn sidebar_menu_button(&mut self, row_rect: Rect, row_focused: bool, menu_focused: bool) -> Result<()> {
        let btn = sidebar_menu_button_rect(row_rect);
        if menu_focused {
            self.painter
                .fill_rounded_rect(btn, (SIDEBAR_MENU_BTN / 2) as i32, theme().accent);
        }
        let glyph_rect = Rect::new(
            btn.x() + (btn.width() as i32 - SIDEBAR_MENU_GLYPH as i32) / 2,
            btn.y() + (btn.height() as i32 - SIDEBAR_MENU_GLYPH as i32) / 2,
            SIDEBAR_MENU_GLYPH,
            SIDEBAR_MENU_GLYPH,
        );
        let color = if menu_focused || row_focused {
            theme().text
        } else {
            theme().muted
        };
        self.icon(glyph_rect, icons().overflow, color)
    }
}
