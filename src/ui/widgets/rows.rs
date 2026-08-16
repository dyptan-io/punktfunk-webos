//! The generic focusable-row list: `FocusRow`/`RowKind` plus every control
//! (dropdown pill, slider, switch, confirm button) a row can carry.
//!
use crate::ui::prelude::*;
use anyhow::Result;

/// How a focus row's right-hand control behaves — every row list in the app shares
/// [`FocusRows`]' single implementation, see its docs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Dropdown,
    Slider,
    Toggle,
    /// A plain actionable row — icon + label, with `value` (if any) as a muted hint on
    /// the right and no control at all. This is what makes a screen out of nothing but
    /// a list: see [`ListModal`], which the host-actions menu and
    /// the Settings sub-page links are both built from. Confirm on the row *is* the
    /// action; there is nothing to adjust in place.
    Action,
}

/// One focusable icon + label (+ dropdown pill / slider / switch) row, drawn by
/// [`FocusRows`]/[`Canvas::focus_row`]. The lists themselves are built per screen in
/// `app::view`.
pub struct FocusRow {
    pub icon: &'static str,
    pub label: String,
    pub value: String,
    pub kind: RowKind,
    /// 0.0-1.0 fill fraction, only meaningful for `RowKind::Slider`.
    pub fraction: f32,
    /// Destructive action (Forget host) — drawn in `theme().error` rather than the
    /// normal muted/white pair, so it reads as dangerous before it's confirmed.
    pub danger: bool,
    /// The row is shown but its value cannot be changed here — dictated by another setting or
    /// by the hardware (e.g. HDR under an H.264 codec pick). Only the *control* greys out
    /// (`theme().disabled`); icon and label keep their normal focus colors, so the row still
    /// reads as a live list entry whose value happens to be fixed. Still focusable, so its
    /// [`subtext`](Self::subtext) — where the reason belongs — can be read. Rejecting the
    /// input is the caller's business, not this widget's.
    pub locked: bool,
    /// `Some` gives this row its own ⋯ actions button, drawn and focused exactly like
    /// a sidebar host row's ([`Canvas::sidebar_menu_button`]) — the bool is whether the
    /// *button* has focus rather than the row body. A row with one has a second thing
    /// Confirm can mean, reached with Right, so the row's own action stays a single
    /// press. `None` (the default) draws no button at all.
    pub menu: Option<bool>,
    /// A small secondary line drawn under the row's label *only while the row is focused*
    /// (e.g. the high-bitrate "may be unstable on Wi-Fi" caution on the Bitrate row).
    /// Unfocused, the label stays vertically centred and nothing is drawn; on focus the
    /// label + caption centre as a block. `None` never draws anything.
    pub subtext: Option<RowSubtext>,
}

/// A small caption drawn under a row's label. The color is carried with the text so the
/// drawing code stays generic — a neutral hint and a caution differ only by which
/// constructor built them, not by any special-casing in [`Canvas::focus_row`].
pub struct RowSubtext {
    pub text: String,
    pub color: Color,
}

impl RowSubtext {
    /// Neutral secondary line (muted grey) — the default for extra context (e.g. the
    /// rooted-TV note on the experimental Game mode row).
    pub fn hint(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: theme().muted,
        }
    }

    /// Dull-orange caution line — a soft warning that isn't a hard error.
    pub fn caution(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: theme().caution,
        }
    }
}

/// The one rule every row control's color follows: a locked row is `theme().disabled`
/// regardless of `active` (focus, or an open dropdown) — locked overrides everything else,
/// so every call site expresses that as data instead of repeating the same `if` three times.
fn locked_fg(locked: bool, active: bool, active_color: Color, inactive_color: Color) -> Color {
    if locked {
        theme().disabled
    } else if active {
        active_color
    } else {
        inactive_color
    }
}

impl FocusRow {
    /// A plain [`RowKind::Action`] row — the common case for list-modal screens.
    pub fn action(icon: &'static str, label: impl Into<String>) -> Self {
        Self {
            icon,
            label: label.into(),
            value: String::new(),
            kind: RowKind::Action,
            fraction: 0.0,
            danger: false,
            locked: false,
            menu: None,
            subtext: None,
        }
    }

    /// Same, with a muted right-hand hint (e.g. a host's address under its name).
    pub fn action_with_value(icon: &'static str, label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            ..Self::action(icon, label)
        }
    }

    /// A [`RowKind::Toggle`] row; `on` picks the On/Off value the switch animates from
    /// ([`Canvas::focus_row`] reads it back out of `value`).
    pub fn toggle(icon: &'static str, label: impl Into<String>, on: bool) -> Self {
        Self {
            kind: RowKind::Toggle,
            value: if on { "On".into() } else { "Off".into() },
            ..Self::action(icon, label)
        }
    }

    /// A [`RowKind::Dropdown`] row showing `value` as its current pick.
    pub fn dropdown(icon: &'static str, label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            kind: RowKind::Dropdown,
            ..Self::action_with_value(icon, label, value)
        }
    }

    /// A [`RowKind::Slider`] row; `fraction` (0.0-1.0) fills the track.
    pub fn slider(icon: &'static str, label: impl Into<String>, value: impl Into<String>, fraction: f32) -> Self {
        Self {
            kind: RowKind::Slider,
            fraction,
            ..Self::action_with_value(icon, label, value)
        }
    }

    /// Marks this row destructive (see [`FocusRow::danger`]).
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Greys this row out and marks its value fixed (see [`FocusRow::locked`]).
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Adds a muted caption line under the label.
    pub fn with_subtext(mut self, subtext: RowSubtext) -> Self {
        self.subtext = Some(subtext);
        self
    }

    /// [`with_subtext`](Self::with_subtext) for a caption a condition may not produce.
    pub fn with_subtext_opt(mut self, subtext: Option<RowSubtext>) -> Self {
        self.subtext = subtext;
        self
    }

    /// Adds a ⋯ actions button; `focused` indicates button vs row focus.
    pub fn with_menu(mut self, focused: bool) -> Self {
        self.menu = Some(focused);
        self
    }
}

// Generous, TV-scale rows — each is its own focusable card (icon + label left,
// control right), consistent with the sidebar/grid's card+focus-ring language
// rather than the bare flat rows the upstream reference uses.
pub const FOCUS_ROW_H: u32 = 92;
pub const FOCUS_ROW_GAP: i32 = 8;
pub const FOCUS_ROW_ICON_SIZE: u32 = 30;

/// Pixels between the tops of consecutive focus rows.
pub const fn focus_row_stride() -> u32 {
    FOCUS_ROW_H + FOCUS_ROW_GAP as u32
}

/// Row `index`'s rect within a modal's content area — used by [`FocusRows`]
/// and `app::App`'s draw-list building to position the focused-row tile.
pub fn focus_row_rect(content_rect: Rect, index: usize) -> Rect {
    focus_row_rect_at_px(content_rect, index, 0)
}

/// Fixed reserved width for a slider row's value label (e.g. "150 Mbps",
/// "Automatic") — the track's position is anchored to this fixed slot rather
/// than to the label's actual (variable) text width, so the track never
/// shifts or appears to resize as the label's digit count changes.
pub const SLIDER_VALUE_SLOT_W: i32 = 150;
/// Extra gap between the track's right edge and the value slot.
const SLIDER_TRACK_GAP: i32 = 16;

/// A modal's focus-row list: icon + label left, control right, one row per
/// [`FocusRow`] stacked at [`focus_row_stride`]. Only the focused row gets a background
/// card; others draw bare. Rows render at normal size — the focused row's zoom is a GPU
/// animation in `app::App`, not a CPU inflate.
///
/// The focus index (and which row's dropdown is expanded) is state the caller owns, so
/// this is a [`StatefulWidget`]: the same row list renders as a whole-list shell with
/// nothing focused ([`FocusRowsState::unfocused`]) and as the one focused row on its own
/// tile, without the list itself having to know which.
pub struct FocusRows<'a> {
    rows: &'a [FocusRow],
}

/// Which row of a [`FocusRows`] has focus, and whether its dropdown is expanded.
pub struct FocusRowsState {
    /// The focused row, or [`UNFOCUSED`].
    pub focused: usize,
    /// The row whose dropdown overlay is currently expanded, if any — independent of
    /// focus, since the pill brightens only while the overlay is actually open.
    pub open_dropdown: Option<usize>,
}

/// Focus index meaning "nothing in this widget is focused" — what a shell tile renders
/// with, since the focused row/button is always composited on top from its own tile. No real
/// index can collide with it.
pub const UNFOCUSED: usize = usize::MAX;

impl FocusRowsState {
    /// Nothing focused — what a shell tile renders with.
    pub fn unfocused() -> Self {
        Self {
            focused: UNFOCUSED,
            open_dropdown: None,
        }
    }

    /// The row whose dropdown is expanded (see [`FocusRowsState::open_dropdown`]).
    pub fn open_dropdown(mut self, row: Option<usize>) -> Self {
        self.open_dropdown = row;
        self
    }
}

impl<'a> FocusRows<'a> {
    pub fn new(rows: &'a [FocusRow]) -> Self {
        Self { rows }
    }
}

impl StatefulWidget for FocusRows<'_> {
    type State = FocusRowsState;

    fn render(self, area: Rect, c: &mut Canvas, state: &mut Self::State) -> Result<()> {
        for (i, row) in self.rows.iter().enumerate() {
            let switch_frac = if row.value == "On" { 1.0 } else { 0.0 };
            c.focus_row(
                row,
                i == state.focused,
                state.open_dropdown == Some(i),
                switch_frac,
                focus_row_rect(area, i),
            )?;
        }
        Ok(())
    }
}

/// Renders one focused row as a tile, composited over the shell. Moving focus
/// recomposites this tile instead of re-rasterizing the whole modal.
/// `switch_frac` animates a `Toggle` row's knob independently.
pub fn render_focus_row_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    rows: &[FocusRow],
    content_width: u32,
    index: usize,
    dropdown_open: bool,
    switch_frac: f32,
) -> Result<Painter> {
    let pad = ROW_TILE_PAD;
    let rect = Rect::new(pad, pad, content_width, FOCUS_ROW_H);
    let mut p = Painter::new(content_width + 2 * pad as u32, FOCUS_ROW_H + 2 * pad as u32);
    if let Some(row) = rows.get(index) {
        let mut c = Canvas::tile(&mut p, text_cache, fonts);
        c.focus_row(row, true, dropdown_open, switch_frac, rect)?;
    }
    Ok(p)
}

/// All rows unfocused as one tile. GPU-side `DrawCmd::TexCropped` handles
/// scrolling without re-rasterizing on each scroll event.
pub fn render_focus_rows_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    rows: &[FocusRow],
    width: u32,
    open_dropdown_row: Option<usize>,
) -> Result<Painter> {
    let height = rows.len() as u32 * focus_row_stride();
    let mut p = Painter::new(width, height.max(1));
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    let mut state = FocusRowsState::unfocused().open_dropdown(open_dropdown_row);
    c.render_stateful(FocusRows::new(rows), Rect::new(0, 0, width, height), &mut state)?;
    Ok(p)
}

impl Canvas<'_, '_> {
    /// Draws one focus row (icon + label + control per `RowKind`) at normal size.
    /// `dropdown_open` is independent of `focused` — a dropdown row's pill brightens while
    /// its overlay is expanded too, not only on row focus.
    pub fn focus_row(
        &mut self,
        row: &FocusRow,
        focused: bool,
        dropdown_open: bool,
        switch_frac: f32,
        row_rect: Rect,
    ) -> Result<()> {
        self.painter.selectable_fixed(row_rect, focused);

        let icon_pad = 24;
        let icon_rect = Rect::new(
            row_rect.x() + icon_pad,
            row_rect.y() + (row_rect.height() as i32 - FOCUS_ROW_ICON_SIZE as i32) / 2,
            FOCUS_ROW_ICON_SIZE,
            FOCUS_ROW_ICON_SIZE,
        );
        // WHY: destructive rows stay red even unfocused — to signal danger before confirm.
        let fg = if row.danger {
            theme().error
        } else if focused {
            theme().text
        } else {
            theme().muted
        };
        self.icon(icon_rect, row.icon, fg)?;
        let label_x = icon_rect.x() + FOCUS_ROW_ICON_SIZE as i32 + 20;
        // A caption belongs to the focused row only: unfocused rows keep the label centred
        // (the common case), while a focused row with one centres label + caption as a block.
        let (label_font, value_font, caption_font) = (self.fonts.label, self.fonts.value, self.fonts.caption);
        let label_h = self.fonts.raster.height(label_font);
        let label_y = match &row.subtext {
            Some(subtext) if focused => {
                let caption_h = self.fonts.raster.height(caption_font);
                let gap = 4;
                let block_h = label_h + gap + caption_h;
                let top = row_rect.y() + (row_rect.height() as i32 - block_h) / 2;
                self.text(caption_font, &subtext.text, label_x, top + label_h + gap, subtext.color)?;
                top
            }
            _ => row_rect.y() + (row_rect.height() as i32 - label_h) / 2,
        };
        self.text(label_font, &row.label, label_x, label_y, fg)?;

        let control_pad = 28;
        let value_y = row_rect.y() + (row_rect.height() as i32 - self.fonts.raster.height(value_font)) / 2;
        match row.kind {
            RowKind::Dropdown => {
                let right_edge = row_rect.right() - control_pad;
                let value_fg = locked_fg(row.locked, focused || dropdown_open, theme().text, theme().muted);
                self.dropdown_value(row_rect, right_edge, &row.value, value_fg)?;
            }
            RowKind::Slider => {
                let value_w = self.fonts.raster.measure(value_font, &row.value).0;
                let slot_right = row_rect.right() - control_pad;
                self.text(
                    value_font,
                    &row.value,
                    slot_right - value_w as i32,
                    value_y,
                    locked_fg(row.locked, focused, theme().text, theme().muted),
                )?;
                let track_w = 220u32.min(row_rect.width() / 3);
                let track = Rect::new(
                    slot_right - SLIDER_VALUE_SLOT_W - SLIDER_TRACK_GAP - track_w as i32,
                    row_rect.y() + (row_rect.height() as i32 - 10) / 2,
                    track_w,
                    10,
                );
                self.painter
                    .slider_with_thumb(track, row.fraction, focused, !row.locked);
            }
            RowKind::Toggle => {
                let switch = Rect::new(
                    row_rect.right() - control_pad - 64,
                    row_rect.y() + (row_rect.height() as i32 - 34) / 2,
                    64,
                    34,
                );
                self.painter.switch(switch, switch_frac, !row.locked);
            }
            // Action rows have no control; `value` is a muted hint only, never interactive.
            RowKind::Action => {
                if !row.value.is_empty() {
                    let menu_w = row.menu.map_or(0, |_| SIDEBAR_MENU_BTN as i32 + 10);
                    let value_w = self.fonts.raster.measure(value_font, &row.value).0;
                    self.text(
                        value_font,
                        &row.value,
                        row_rect.right() - control_pad - menu_w - value_w as i32,
                        value_y,
                        theme().muted,
                    )?;
                }
            }
        }
        if let Some(menu_focused) = row.menu {
            self.sidebar_menu_button(row_rect, focused, menu_focused)?;
        }
        Ok(())
    }

    /// The dropdown value + chevron, right-aligned to `right_edge` and vertically
    /// centered on `row_rect` — no box, the row's own focus state already provides one.
    /// Text and chevron share `fg`, so the caller decides what the pill's state means
    /// (brighter while focused or the overlay is expanded, grey while the row is locked).
    pub fn dropdown_value(&mut self, row_rect: Rect, right_edge: i32, label: &str, fg: Color) -> Result<()> {
        let chevron_size = 20u32;
        let chevron_rect = Rect::new(
            right_edge - chevron_size as i32,
            row_rect.y() + (row_rect.height() as i32 - chevron_size as i32) / 2,
            chevron_size,
            chevron_size,
        );
        self.icon(chevron_rect, icons().chevron_down, fg)?;
        let font = self.fonts.value;
        let text_w = self.fonts.raster.measure(font, label).0;
        let y = row_rect.y() + (row_rect.height() as i32 - self.fonts.raster.height(font)) / 2;
        self.text(
            font,
            label,
            right_edge - chevron_size as i32 - 10 - text_w as i32,
            y,
            fg,
        )?;
        Ok(())
    }
}

impl Painter {
    /// Slider track with round-thumbed, shadowed knob. `enabled` false greys the fill and
    /// knob — the value still reads, but not as something this screen will change.
    pub fn slider_with_thumb(&mut self, rect: Rect, fraction: f32, focused: bool, enabled: bool) {
        let track_h = rect.height();
        self.fill_rounded_rect(rect, track_h as i32 / 2, Color::RGBA(0xff, 0xff, 0xff, 0x22));
        let filled_w = (rect.width() as f32 * fraction.clamp(0.0, 1.0)) as u32;
        if filled_w > 0 {
            let filled = Rect::new(rect.x(), rect.y(), filled_w.max(track_h), track_h);
            self.fill_rounded_rect(
                filled,
                track_h as i32 / 2,
                if enabled { theme().accent } else { theme().disabled },
            );
        }
        let thumb_r = 14.0;
        let cx = rect.x() as f32 + filled_w as f32;
        let cy = rect.y() as f32 + rect.height() as f32 / 2.0;
        self.fill_circle(cx + 2.0, cy + 3.0, thumb_r, Color::RGBA(0x00, 0x00, 0x00, 0x50));
        self.fill_circle(
            cx,
            cy,
            thumb_r,
            locked_fg(!enabled, focused, theme().text, theme().muted),
        );
    }
}

/// Lerp between two colors; used for switch track cross-fade.
pub fn lerp_color(from: Color, to: Color, frac: f32) -> Color {
    let f = frac.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * f) as u8;
    Color::RGBA(
        lerp(from.r, to.r),
        lerp(from.g, to.g),
        lerp(from.b, to.b),
        lerp(from.a, to.a),
    )
}

pub const SWITCH_OFF_TRACK: Color = Color::RGBA(0xff, 0xff, 0xff, 0x22);

impl Painter {
    /// Modern sliding pill switch. `frac` (0.0=off, 1.0=on) lerps position & color
    /// for smooth animation; pass static 0.0/1.0 for immediate toggle. `enabled` false
    /// greys the "on" track, so a locked toggle still shows its state without inviting a press.
    pub fn switch(&mut self, rect: Rect, frac: f32, enabled: bool) {
        let frac = frac.clamp(0.0, 1.0);
        let radius = rect.height() as i32 / 2;
        let on_track = if enabled { theme().accent } else { theme().disabled };
        self.fill_rounded_rect(rect, radius, lerp_color(SWITCH_OFF_TRACK, on_track, frac));
        let knob_r = radius as f32 - 4.0;
        let cy = rect.y() as f32 + rect.height() as f32 / 2.0;
        let left = rect.x() as f32 + radius as f32;
        let right = rect.x() as f32 + rect.width() as f32 - radius as f32;
        let cx = left + (right - left) * frac;
        self.fill_circle(cx + 1.0, cy + 2.0, knob_r, Color::RGBA(0x00, 0x00, 0x00, 0x40));
        self.fill_circle(cx, cy, knob_r, if enabled { theme().text } else { theme().disabled });
    }
}

/// Row height of one dropdown option — also `render_dropdown_option_tile`'s tile size.
pub const DROPDOWN_OPTION_H: u32 = 56;

/// Scrollbar track+thumb. Rendered as own tile so fade-in/out is alpha
/// composite, not re-rasterization.
const SCROLLBAR_TRACK_W: u32 = 6;

pub fn render_list_scrollbar_tile(tile_w: u32, tile_h: u32, total: usize, visible: usize, scroll: usize) -> Painter {
    let mut painter = Painter::new(tile_w, tile_h.max(1));
    if total <= visible {
        return painter;
    }
    let track_w = SCROLLBAR_TRACK_W.min(tile_w);
    let track = Rect::new(tile_w as i32 - track_w as i32, 0, track_w, tile_h);
    painter.fill_rounded_rect(track, track_w as i32 / 2, Color::RGBA(0xff, 0xff, 0xff, 0x14));

    let thumb_h = ((visible as f32 / total as f32) * track.height() as f32).round() as u32;
    let thumb_h = thumb_h.clamp(24, track.height());
    let max_thumb_y = track.height().saturating_sub(thumb_h) as f32;
    let max_scroll = (total - visible).max(1) as f32;
    let thumb_y = track.y() + ((scroll as f32 / max_scroll) * max_thumb_y).round() as i32;
    let thumb = Rect::new(track.x(), thumb_y, track_w, thumb_h);
    painter.fill_rounded_rect(thumb, track_w as i32 / 2, Color::RGBA(0xff, 0xff, 0xff, 0x50));
    painter
}

/// A settings row's on-screen rect at a pixel scroll offset — the smooth-scroll counterpart
/// of [`focus_row_rect`], which indexes rows within the viewport instead. Can land partly (or
/// wholly) outside `content_rect` while the viewport is gliding; callers clip.
pub fn focus_row_rect_at_px(content_rect: Rect, index: usize, scroll_px: i32) -> Rect {
    let y = content_rect.y() + index as i32 * focus_row_stride() as i32 - scroll_px;
    Rect::new(content_rect.x(), y, content_rect.width(), FOCUS_ROW_H)
}

/// How tall an edge fade is: exactly one row.
///
/// Deliberately taller than the peek strip it dissolves (`view::settings::PEEK`), so the band
/// reaches past the partial row and into the full row beyond it. Sized to the peek instead,
/// the ramp only reached ~35% alpha by the time it crossed the partial row's text — enough to
/// render, not enough to read as a fade. Being taller also means the dense end lands on the
/// partial row while the row above it takes only the ramp's first, near-clear pixels.
pub const SCROLL_FADE_H: u32 = FOCUS_ROW_H;

/// Tile width for the scroll fade. The ramp is uniform horizontally, so the GPU stretches
/// this to whatever the list's width is — a fixed narrow tile means one static texture for
/// every modal instead of one per content width. Not 1px: under linear filtering a
/// single-column texture has no interior samples to stretch from.
const SCROLL_FADE_TILE_W: u32 = 8;

/// Which edge of the viewport a fade tile dissolves into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FadeEdge {
    /// Dense at the top, clear at the bottom — shown while content is scrolled off above.
    Top,
    /// Clear at the top, dense at the bottom — shown while content remains below.
    Bottom,
}

/// An edge fade that signals "the list continues this way".
///
/// Exists because the scrollbar alone doesn't answer the question on arrival: it's
/// hold-then-fade (see `SCROLL_INDICATOR_HOLD`), so a list that opens already overflowing
/// shows nothing at all once the hold lapses, and the last row looks like the final row.
///
/// Fades to the modal card's own background (`theme().panel`), not to black: the band has to
/// look like the card surface swallowing the row, and any other colour reads as a shadow
/// sitting on top of the list.
pub fn render_scroll_fade_tile(edge: FadeEdge) -> Painter {
    let mut painter = Painter::new(SCROLL_FADE_TILE_W, SCROLL_FADE_H);
    match edge {
        FadeEdge::Top => painter.fill_vertical_fade(theme().panel, 0xff, 0x00),
        FadeEdge::Bottom => painter.fill_vertical_fade(theme().panel, 0x00, 0xff),
    }
    painter
}

/// The expanded dropdown: its options as an overlay list anchored below the opener row.
/// One panel background+shadow instead of per-row cards, to avoid shadow smearing.
/// Renders every option unfocused, like the row lists: the focused one composites over it
/// from [`render_dropdown_option_tile`].
pub struct DropdownOverlay<'a> {
    options: &'a [String],
}

impl<'a> DropdownOverlay<'a> {
    pub fn new(options: &'a [String]) -> Self {
        Self { options }
    }
}

impl Widget for DropdownOverlay<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let bg_rect = Rect::new(
            area.x(),
            area.y(),
            area.width(),
            self.options.len() as u32 * DROPDOWN_OPTION_H,
        );
        c.painter.popup_panel(bg_rect, Color::RGBA(0xff, 0xff, 0xff, 0x20));
        for (i, opt) in self.options.iter().enumerate() {
            c.dropdown_option(opt, false, dropdown_option_rect(area, i))?;
        }
        Ok(())
    }
}

/// Option `index`'s rect within a dropdown overlay.
pub fn dropdown_option_rect(rect: Rect, index: usize) -> Rect {
    Rect::new(
        rect.x(),
        rect.y() + index as i32 * DROPDOWN_OPTION_H as i32,
        rect.width(),
        DROPDOWN_OPTION_H,
    )
}

/// Renders one focused dropdown option as a tile, composited over the overlay.
/// Moving focus recomposites just this tile instead of re-rasterizing.
pub fn render_dropdown_option_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    option: &str,
    width: u32,
) -> Result<Painter> {
    let mut p = Painter::new(width, DROPDOWN_OPTION_H);
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    c.dropdown_option(option, true, Rect::new(0, 0, width, DROPDOWN_OPTION_H))?;
    Ok(p)
}

impl Canvas<'_, '_> {
    /// Draws one dropdown option (highlighted if focused) at normal size.
    pub fn dropdown_option(&mut self, option: &str, focused: bool, row_rect: Rect) -> Result<()> {
        if focused {
            let highlight = Rect::new(
                row_rect.x() + 6,
                row_rect.y() + 4,
                row_rect.width().saturating_sub(12),
                row_rect.height().saturating_sub(8),
            );
            self.painter.fill_rounded_rect(
                highlight,
                8,
                Color::RGBA(theme().accent.r, theme().accent.g, theme().accent.b, 0x50),
            );
        }
        let font = self.fonts.value;
        let y = row_rect.y() + (row_rect.height() as i32 - self.fonts.raster.height(font)) / 2;
        self.text(
            font,
            option,
            row_rect.x() + 20,
            y,
            if focused { theme().text } else { theme().muted },
        )?;
        Ok(())
    }
}

impl Painter {
    /// Common popup panel chrome: shadowed dark background with colored border.
    pub fn popup_panel(&mut self, rect: Rect, border_color: Color) {
        self.card_shadow(rect, CARD_RADIUS);
        self.fill_rounded_rect(rect, CARD_RADIUS, Color::RGBA(0x17, 0x11, 0x28, 0xf6));
        self.stroke_rounded_rect(rect, CARD_RADIUS, border_color, 1.5);
    }
}

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
            color: theme().text,
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

/// Renders one focused button as a tile, composited over the shell.
pub fn render_confirm_button_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    button: &ConfirmButton<'_>,
    w: u32,
    h: u32,
) -> Result<Painter> {
    let pad = ROW_TILE_PAD;
    let mut p = Painter::new(w + 2 * pad as u32, h + 2 * pad as u32);
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    c.confirm_button(button, true, Rect::new(pad, pad, w, h))?;
    Ok(p)
}

impl Canvas<'_, '_> {
    /// Draws one confirm button at normal size, focused or not.
    pub fn confirm_button(&mut self, button: &ConfirmButton<'_>, focused: bool, rect: Rect) -> Result<()> {
        self.painter.selectable_fixed(rect, focused);
        let color = if focused { button.color } else { theme().muted };

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
        // was already centred on its own — and the label is ellipsized to whatever the icon
        // leaves, so no label can overflow the button regardless of resolution.
        let leading = match button.icon {
            Some(_) => icon_size + icon_gap as u32,
            None => 0,
        };
        let budget = rect.width().saturating_sub(2 * side_pad as u32).saturating_sub(leading);
        let label = ellipsize(self.fonts.raster, font, button.label, budget);
        let label_w = self.fonts.raster.measure(font, &label).0;
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
        self.text(
            font,
            &label,
            start_x + leading as i32,
            rect.y() + (rect.height() as i32 - line_h) / 2,
            color,
        )?;
        Ok(())
    }
}
