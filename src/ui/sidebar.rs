//! Sidebar panel metrics and the generic nav-row widgets built on them: an icon + label
//! row with optional selection tint, and the ⋯ actions button a row can carry.
//!
//! This app's own host list is `app::view::sidebar`, which composes these.
use super::*;
use crate::ui::render::Color;
use crate::ui::render::Rect;
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

/// Draw a selectable row with optional selection highlighting. When focused, shows
/// the full card with shadow and zoom. When selected (but not focused), shows a
/// subtle background. When neither, shows no background.
fn draw_selectable_with_selection(painter: &mut Painter, rect: Rect, focused: bool, selected: bool) -> Rect {
    let r = draw_selectable(painter, rect, focused);
    if !focused && selected {
        let selected_bg = Color::RGBA(0x2b, 0x21, 0x48, 0x40);
        painter.fill_rounded_rect(r, CARD_RADIUS, selected_bg);
    }
    r
}

/// Sidebar row layout: left-aligned icon + label, focus-colored.
/// `selected` adds subtle background when unfocused.
#[allow(clippy::too_many_arguments)]
pub fn draw_sidebar_row(
    painter: &mut Painter,
    text_cache: &mut TextCache,
    fonts: &Fonts,
    rect: Rect,
    glyph: &str,
    label: &str,
    focused: bool,
    selected: bool,
    reserve_right: u32,
) -> Result<()> {
    let drawn = draw_selectable_with_selection(painter, rect, focused, selected);
    let icon_rect = sidebar_icon_rect(drawn);
    let color = if focused { WHITE } else { MUTED };
    draw_icon(painter, text_cache, fonts.raster, fonts.icon, icon_rect, glyph, color)?;
    // Ellipsized to prevent overflow; reserve_right prevents running under ⋯ button.
    let text_x = SIDEBAR_ICON_PAD + SIDEBAR_ICON_SIZE as i32 + 16;
    let max_w = drawn.width().saturating_sub(text_x as u32 + 20 + reserve_right);
    let label = ellipsize(fonts.raster, fonts.label, label, max_w);
    draw_text(
        painter,
        text_cache,
        fonts.raster,
        fonts.label,
        &label,
        drawn.x() + text_x,
        drawn.y() + (drawn.height() as i32 - fonts.raster.height(fonts.label)) / 2,
        color,
    )?;
    Ok(())
}

/// The ⋯ button itself: a rounded highlight plate once it has focus, then the glyph.
pub fn draw_sidebar_menu_button(
    painter: &mut Painter,
    text_cache: &mut TextCache,
    fonts: &Fonts,
    row_rect: Rect,
    row_focused: bool,
    menu_focused: bool,
) -> Result<()> {
    let btn = sidebar_menu_button_rect(row_rect);
    if menu_focused {
        painter.fill_rounded_rect(btn, (SIDEBAR_MENU_BTN / 2) as i32, ACCENT);
    }
    let glyph_rect = Rect::new(
        btn.x() + (btn.width() as i32 - SIDEBAR_MENU_GLYPH as i32) / 2,
        btn.y() + (btn.height() as i32 - SIDEBAR_MENU_GLYPH as i32) / 2,
        SIDEBAR_MENU_GLYPH,
        SIDEBAR_MENU_GLYPH,
    );
    let color = if menu_focused || row_focused { WHITE } else { MUTED };
    draw_icon(
        painter,
        text_cache,
        fonts.raster,
        fonts.icon,
        glyph_rect,
        ICON_MORE,
        color,
    )
}
