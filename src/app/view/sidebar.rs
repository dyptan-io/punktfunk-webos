//! The Home screen's host-list sidebar: brand lockup, host rows, "+ Add host", and the
//! bottom-pinned Settings row, plus the hit tests that go with them. Built from the
//! nav-row widgets in `ui::widgets`.
use crate::app::hosts::HostEntry;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::Canvas;
use anyhow::Result;

pub const TOP_Y: i32 = 216;

/// Every nav position's rect, in order: the host rows and "+ Add host" stacked from
/// [`TOP_Y`], then "Settings" pinned to the bottom of the panel — a spacer slot between
/// them is what "pinned" means, so it stays put regardless of how many hosts are known.
///
/// One split, read by the painter, both hit tests and `home_focus_map` alike; nothing
/// recomputes a row's position from an index formula of its own.
pub fn nav_rows(row_count: usize, screen_h: u32) -> Vec<Rect> {
    let column = Rect::new(
        ui::widgets::SIDEBAR_PAD,
        TOP_Y,
        ui::widgets::SIDEBAR_W - 2 * ui::widgets::SIDEBAR_PAD as u32,
        (screen_h as i32 - ui::widgets::SIDEBAR_PAD - TOP_Y).max(0) as u32,
    );
    let above = row_count.saturating_sub(1);
    let slots = std::iter::repeat_n(ui::layout::Constraint::Length(ui::widgets::SIDEBAR_ROW_H), above).chain([
        ui::layout::Constraint::Fill(1),
        ui::layout::Constraint::Length(ui::widgets::SIDEBAR_ROW_H),
    ]);
    let mut rects = ui::layout::Layout::vertical(slots)
        .gap(ui::widgets::SIDEBAR_ROW_GAP)
        .split(column);
    // Drop the spacer, leaving one rect per nav position.
    rects.remove(above);
    rects.truncate(row_count);
    rects
}

/// Nav position `index`'s rect. [`nav_rows`] where a caller needs more than one.
pub fn nav_row_rect(index: usize, row_count: usize, screen_h: u32) -> Rect {
    nav_rows(row_count, screen_h)
        .get(index)
        .copied()
        .unwrap_or(Rect::new(0, 0, 0, 0))
}

/// The part of a nav row that is the row *itself* rather than its ⋯ button. Focus
/// navigation needs the two as disjoint targets (`ui::focus` never treats overlapping
/// rects as candidates for each other), so where the body ends is layout's business, not
/// navigation's.
pub fn row_body_rect(row: Rect, has_menu: bool) -> Rect {
    if !has_menu {
        return row;
    }
    let btn = ui::widgets::sidebar_menu_button_rect(row);
    Rect::new(row.x(), row.y(), (btn.x() - row.x()).max(0) as u32, row.height())
}

/// Diameter of the presence dot badged onto a host row's icon.
const PRESENCE_DOT: f32 = 9.0;

/// Whether `(x, y)` is on host row `index`'s ⋯ button. Checked *before*
/// [`hit_test_row`] by the click handler, since the button sits inside the row
/// it belongs to and would otherwise just read as a click on the row.
pub fn hit_test_menu_button(x: i32, y: i32, host_count: usize, row_count: usize, screen_h: u32) -> Option<usize> {
    nav_rows(row_count, screen_h)
        .into_iter()
        .take(host_count)
        .position(|row| ui::widgets::sidebar_menu_button_rect(row).contains_point((x, y)))
}

/// `None` when `(x, y)` falls outside the sidebar's horizontal band at all — lets
/// mouse-motion handling distinguish "not hovering the sidebar" from "hovering the
/// sidebar but between rows." The last nav position (`row_count - 1`, "Settings")
/// is pinned to the bottom of the panel (see [`settings_row_rect`]) rather than
/// following on from the sequential rows above it.
pub fn hit_test_row(x: i32, y: i32, row_count: usize, screen_h: u32) -> Option<usize> {
    if x < 0 || x as u32 > ui::widgets::SIDEBAR_W || row_count == 0 {
        return None;
    }
    nav_rows(row_count, screen_h)
        .into_iter()
        .position(|row| row.contains_point((x, y)))
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
    c.painter.fill_rect(
        Rect::new(0, 0, ui::widgets::SIDEBAR_W, screen_h),
        ui::style::theme().panel,
    );
    // Logo is 1:1 (no runtime scaling); bundled at exact display size.
    if let Some(logo) = crate::assets::logo_pixmap() {
        let logo_x = (ui::widgets::SIDEBAR_W as i32 - logo.width() as i32) / 2;
        c.painter.draw_pixmap(logo_x, 32, logo);
    }

    let add_row = entries.len();
    let settings_row = entries.len() + 1;
    let rows = nav_rows(settings_row + 1, screen_h);
    for (i, entry) in entries.iter().enumerate() {
        draw_host_row(
            c,
            rows[i],
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
    draw_utility_row(c, rows[add_row], "+ Add host", focused_index == Some(add_row))?;

    let settings_rect = rows[settings_row];
    // The version lives on the About screen, not in nav chrome — see `app::view::about::VERSION`.
    c.painter
        .rule(settings_rect.x(), settings_rect.y() - 14, settings_rect.width());
    draw_utility_row(c, settings_rect, "Settings", focused_index == Some(settings_row))?;

    Ok(())
}

/// Host row with ⋯ actions button (drawn on every row to advertise actions exist).
pub fn draw_host_row(c: &mut Canvas, rect: Rect, name: &str, state: &HostRowState) -> Result<()> {
    let &HostRowState { focused, online, .. } = state;
    let glyph = if state.paired {
        crate::app::view::icons::ICON_TV
    } else {
        crate::app::view::icons::ICON_LOCK
    };
    c.render(
        ui::widgets::SidebarRow::new(glyph, name)
            .focused(focused)
            .selected(state.selected)
            .reserve_right(ui::widgets::SIDEBAR_MENU_BTN + 10),
        rect,
    )?;
    // Badged onto the icon's corner rather than given its own column: it needs no layout
    // of its own, and a presence dot on the thing it describes is a well-worn idiom.
    if let Some(online) = online {
        let icon = ui::widgets::sidebar_icon_rect(ui::widgets::focus_zoom(rect, focused));
        let cx = icon.right() as f32 - 1.0;
        let cy = icon.bottom() as f32 - 2.0;
        // A ring of panel background first, so the dot reads as separate from the glyph
        // it overlaps rather than merging into it.
        c.painter
            .fill_circle(cx, cy, PRESENCE_DOT / 2.0 + 2.0, ui::style::theme().panel);
        let color = if online {
            ui::style::theme().ok
        } else {
            ui::style::theme().muted
        };
        c.painter.fill_circle(cx, cy, PRESENCE_DOT / 2.0, color);
    }
    c.sidebar_menu_button(rect, focused, state.menu_focused)
}

pub fn draw_utility_row(c: &mut Canvas, rect: Rect, label: &str, focused: bool) -> Result<()> {
    let glyph = if label.starts_with('+') {
        crate::app::view::icons::ICON_ADD
    } else {
        crate::app::view::icons::ICON_SETTINGS
    };
    let label = label.trim_start_matches('+').trim();
    c.render(ui::widgets::SidebarRow::new(glyph, label).focused(focused), rect)
}

/// The focused sidebar row, padded so the compositor can pop it without clipping its shadow.
/// `menu_focused` flags the actions button; both button states reuse one tile, so moving
/// between them costs one re-rasterize.
pub struct FocusedRowTile<'a> {
    pub entries: &'a [HostEntry],
    pub index: usize,
    pub menu_focused: bool,
    pub online: Option<bool>,
}

impl FocusedRowTile<'_> {
    /// The row's own size, before the padding [`TileWidget::size`] adds.
    fn row_size() -> (u32, u32) {
        (
            ui::widgets::SIDEBAR_W - 2 * ui::widgets::SIDEBAR_PAD as u32,
            ui::widgets::SIDEBAR_ROW_H,
        )
    }
}

impl ui::Widget for FocusedRowTile<'_> {
    fn render(self, area: ui::render::Rect, c: &mut ui::Canvas) -> Result<()> {
        let rect = area.inflate(-ui::tiles::ROW_TILE_PAD);
        if let Some(entry) = self.entries.get(self.index) {
            draw_host_row(
                c,
                rect,
                entry.name(),
                &HostRowState {
                    paired: entry.is_paired(),
                    focused: true,
                    selected: false,
                    menu_focused: self.menu_focused,
                    online: self.online,
                },
            )
        } else if self.index == self.entries.len() {
            draw_utility_row(c, rect, "+ Add host", true)
        } else {
            draw_utility_row(c, rect, "Settings", true)
        }
    }
}

impl ui::TileWidget for FocusedRowTile<'_> {
    fn size(&self, _fonts: &ui::text::Fonts) -> (u32, u32) {
        let (w, h) = Self::row_size();
        ui::tiles::padded_size(w, h, ui::tiles::ROW_TILE_PAD)
    }
}
