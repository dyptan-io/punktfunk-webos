//! The Home screen's host-list sidebar: brand lockup, host rows, "+ Add host", and the
//! bottom-pinned Settings row, plus the hit tests that go with them. Built from the
//! nav-row widgets in `ui::sidebar`.
use crate::app::hosts::HostEntry;
use crate::ui::render::Rect;
use crate::ui::{self, Canvas};
use anyhow::Result;

pub const TOP_Y: i32 = 216;

pub fn row_rect(index: usize) -> Rect {
    let y = TOP_Y + index as i32 * (ui::SIDEBAR_ROW_H as i32 + ui::SIDEBAR_ROW_GAP);
    Rect::new(
        ui::SIDEBAR_PAD,
        y,
        ui::SIDEBAR_W - 2 * ui::SIDEBAR_PAD as u32,
        ui::SIDEBAR_ROW_H,
    )
}

/// The "Settings" row's rect — pinned to the bottom of the sidebar panel instead
/// of following the host list/"+ Add host" row sequentially (`row_rect`),
/// so it stays in the same place regardless of how many hosts are known.
pub fn settings_row_rect(screen_h: u32) -> Rect {
    let y = screen_h as i32 - ui::SIDEBAR_PAD - ui::SIDEBAR_ROW_H as i32;
    Rect::new(
        ui::SIDEBAR_PAD,
        y,
        ui::SIDEBAR_W - 2 * ui::SIDEBAR_PAD as u32,
        ui::SIDEBAR_ROW_H,
    )
}

/// Diameter of the presence dot badged onto a host row's icon.
const PRESENCE_DOT: f32 = 9.0;

/// Whether `(x, y)` is on host row `index`'s ⋯ button. Checked *before*
/// [`hit_test_row`] by the click handler, since the button sits inside the row
/// it belongs to and would otherwise just read as a click on the row.
pub fn hit_test_menu_button(x: i32, y: i32, host_count: usize) -> Option<usize> {
    (0..host_count).find(|&i| ui::sidebar_menu_button_rect(row_rect(i)).contains_point((x, y)))
}

/// `None` when `(x, y)` falls outside the sidebar's horizontal band at all — lets
/// mouse-motion handling distinguish "not hovering the sidebar" from "hovering the
/// sidebar but between rows." The last nav position (`row_count - 1`, "Settings")
/// is pinned to the bottom of the panel (see [`settings_row_rect`]) rather than
/// following on from the sequential rows above it.
pub fn hit_test_row(x: i32, y: i32, row_count: usize, screen_h: u32) -> Option<usize> {
    if x < 0 || x as u32 > ui::SIDEBAR_W || row_count == 0 {
        return None;
    }
    let settings_index = row_count - 1;
    if settings_row_rect(screen_h).contains_point((x, y)) {
        return Some(settings_index);
    }
    (0..settings_index).find(|&i| row_rect(i).contains_point((x, y)))
}

/// One host row's live state: how it should draw, and what its presence dot should say.
pub struct HostRowState {
    pub paired: bool,
    pub focused: bool,
    pub selected: bool,
    /// The ⋯ button has focus rather than the row body.
    pub menu_focused: bool,
    /// `None` until reachability has been probed at all (see `app::state::reach`) — an
    /// unknown state draws no dot rather than a confident "offline".
    pub online: Option<bool>,
}

/// Draws sidebar: flat panel + logo + host rows + "Add host" + Settings (bottom-pinned).
/// `selected_index` highlights the active/connected host; `online` is index-aligned with
/// `entries`.
pub fn draw(
    c: &mut Canvas,
    entries: &[HostEntry],
    focused_index: Option<usize>,
    selected_index: Option<usize>,
    online: &[Option<bool>],
) -> Result<()> {
    let screen_h = c.screen_h;
    c.painter
        .fill_rect(Rect::new(0, 0, ui::SIDEBAR_W, screen_h), ui::SIDEBAR_BG);
    // Logo is 1:1 (no runtime scaling); bundled at exact display size.
    if let Some(logo) = ui::logo_pixmap() {
        let logo_x = (ui::SIDEBAR_W as i32 - logo.width() as i32) / 2;
        c.painter.draw_pixmap(logo_x, 32, logo);
    }

    let add_row = entries.len();
    let settings_row = entries.len() + 1;
    for (i, entry) in entries.iter().enumerate() {
        draw_host_row(
            c,
            row_rect(i),
            entry.name(),
            &HostRowState {
                paired: entry.is_paired(),
                focused: focused_index == Some(i),
                selected: selected_index == Some(i),
                menu_focused: false,
                online: online.get(i).copied().flatten(),
            },
        )?;
    }
    draw_utility_row(c, row_rect(add_row), "+ Add host", focused_index == Some(add_row))?;

    let settings_rect = settings_row_rect(screen_h);
    // The version lives on the About screen, not in nav chrome — see `app::view::about::VERSION`.
    c.rule(settings_rect.x(), settings_rect.y() - 14, settings_rect.width());
    draw_utility_row(c, settings_rect, "Settings", focused_index == Some(settings_row))?;

    Ok(())
}

/// Host row with ⋯ actions button (drawn on every row to advertise actions exist).
pub fn draw_host_row(c: &mut Canvas, rect: Rect, name: &str, state: &HostRowState) -> Result<()> {
    let &HostRowState { focused, online, .. } = state;
    let glyph = if state.paired { ui::ICON_TV } else { ui::ICON_LOCK };
    c.sidebar_row(rect, glyph, name, focused, state.selected, ui::SIDEBAR_MENU_BTN + 10)?;
    // Badged onto the icon's corner rather than given its own column: it needs no layout
    // of its own, and a presence dot on the thing it describes is a well-worn idiom.
    if let Some(online) = online {
        let icon = ui::sidebar_icon_rect(ui::inflate(rect, focused));
        let cx = icon.right() as f32 - 1.0;
        let cy = icon.bottom() as f32 - 2.0;
        // A ring of panel background first, so the dot reads as separate from the glyph
        // it overlaps rather than merging into it.
        c.painter.fill_circle(cx, cy, PRESENCE_DOT / 2.0 + 2.0, ui::SIDEBAR_BG);
        let color = if online { ui::ONLINE_GREEN } else { ui::MUTED };
        c.painter.fill_circle(cx, cy, PRESENCE_DOT / 2.0, color);
    }
    c.sidebar_menu_button(rect, focused, state.menu_focused)
}

pub fn draw_utility_row(c: &mut Canvas, rect: Rect, label: &str, focused: bool) -> Result<()> {
    let glyph = if label.starts_with('+') {
        ui::ICON_ADD
    } else {
        ui::ICON_SETTINGS
    };
    let label = label.trim_start_matches('+').trim();
    c.sidebar_row(rect, glyph, label, focused, false, 0)
}

/// Focused sidebar row as padded tile. `menu_focused` flags the actions button.
/// Both button states reuse one tile; moving between them costs one re-rasterize.
pub fn render_focused_row_tile(
    text_cache: &mut ui::TextCache,
    fonts: &ui::Fonts,
    entries: &[HostEntry],
    index: usize,
    menu_focused: bool,
    online: Option<bool>,
) -> Result<ui::Painter> {
    let pad = ui::ROW_TILE_PAD;
    let base = row_rect(0);
    let rect = Rect::new(pad, pad, base.width(), base.height());
    let mut p = ui::Painter::new(base.width() + 2 * pad as u32, base.height() + 2 * pad as u32);
    let c = &mut Canvas::tile(&mut p, text_cache, fonts);
    if let Some(entry) = entries.get(index) {
        draw_host_row(
            c,
            rect,
            entry.name(),
            &HostRowState {
                paired: entry.is_paired(),
                focused: true,
                selected: false,
                menu_focused,
                online,
            },
        )?;
    } else if index == entries.len() {
        draw_utility_row(c, rect, "+ Add host", true)?;
    } else {
        draw_utility_row(c, rect, "Settings", true)?;
    }
    Ok(p)
}
