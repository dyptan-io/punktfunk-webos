//! The generic focusable-row list: `FocusRow`/`RowKind`, the row geometry both the draw and
//! the pointer index by ([`row_layout`]), and the controls a row itself draws — the dropdown
//! pill, the slider and the switch. What sits *around* a row list lives beside it:
//! [`super::dropdown`] (the expanded option overlay), [`super::scroll`] (scrollbar and edge
//! fades) and [`super::confirm`] (the two-button modal's buttons).
use crate::ui::prelude::*;
use anyhow::Result;

/// How a focus row's right-hand control behaves — every row list in the app shares
/// [`FocusRows`]' single implementation, see its docs.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Clone)]
pub struct FocusRow {
    pub icon: &'static str,
    pub label: String,
    pub value: String,
    pub kind: RowKind,
    /// 0.0-1.0 fill fraction, only meaningful for `RowKind::Slider`.
    pub fraction: f32,
    /// Destructive action (Forget host) — drawn in `palette().error` rather than the
    /// normal muted/white pair, so it reads as dangerous before it's confirmed.
    pub danger: bool,
    /// The row is shown but its value cannot be changed here — dictated by another setting or
    /// by the hardware (e.g. HDR under an H.264 codec pick). Only the *control* greys out
    /// (`palette().disabled`); icon and label keep their normal focus colors, so the row still
    /// reads as a live list entry whose value happens to be fixed. Still focusable, so its
    /// [`subtext`](Self::subtext) — where the reason belongs — can be read. Rejecting the
    /// input is the caller's business, not this widget's.
    pub locked: bool,
    /// Icon buttons at the row's right end, in left-to-right order — drawn and focused
    /// exactly like a sidebar host row's ⋯ ([`Canvas::sidebar_menu_button`]). Each is a
    /// further thing Confirm can mean on this row, reached with Right, so the row's own
    /// action stays a single press. Per-row, so one row in a list can offer fewer than its
    /// neighbours (Library has no Remove). Empty (the default) draws nothing.
    pub trailing: &'static [&'static str],
    /// Room to keep clear at the row's right end, in trailing buttons, when placing the
    /// value label — over and above the buttons this row actually has. A list whose rows
    /// carry different counts (Library has no Remove) would otherwise hang its values at
    /// different x; [`align_values`] sets this so the column lines up. 0 (the default)
    /// reserves exactly the row's own buttons.
    pub value_reserve: usize,
    /// The row's own [`icon`](Self::icon) is a button too, in the slot it already draws in —
    /// reached with Left, where [`trailing`](Self::trailing) is reached with Right. For the
    /// action a row wants *before* its label rather than after it (a collection's drag
    /// handle), so the grip sits where the eye starts the row.
    pub leading_button: bool,
    /// That leading button has focus. Mutually exclusive with
    /// [`trailing_focused`](Self::trailing_focused) — the two sides are one cursor.
    pub leading_focused: bool,
    /// The leading button is held *open* — the drag handle of a row being moved. Drawn lit
    /// whether or not it also has focus, rather than only while focus is on it.
    pub leading_active: bool,
    /// Which trailing button has focus, if any — `None` means focus is on the row body.
    pub trailing_focused: Option<usize>,
    /// A small dot in the row's right gutter, in this colour — what marks a row as differing
    /// from whatever it inherits (a per-game settings override, say). Purely an indicator:
    /// not focusable, not clickable, carrying no action; what it means, and how it goes away,
    /// is the caller's business. `None` (the default) draws nothing and leaves the row's
    /// control the width the dot would have taken.
    pub mark: Option<Color>,
    /// Keep the [`mark`](Self::mark) gutter clear even though this row wears no dot — what
    /// [`align_values`] sets on a list where some *other* row does, so one marked row does
    /// not pull its own value 32px left of its neighbours'.
    pub mark_reserve: bool,
    /// A small secondary line drawn under the row's label *only while the row is focused*
    /// (e.g. the high-bitrate "may be unstable on Wi-Fi" caution on the Bitrate row).
    /// Unfocused, the label stays vertically centred and nothing is drawn; on focus the
    /// label + caption centre as a block. `None` never draws anything.
    pub subtext: Option<RowSubtext>,
}

/// A small caption drawn under a row's label. The color is carried with the text so the
/// drawing code stays generic — a neutral hint and a caution differ only by which
/// constructor built them, not by any special-casing in [`Canvas::focus_row`].
#[derive(Clone)]
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
            color: palette().muted,
        }
    }

    /// Dull-orange caution line — a soft warning that isn't a hard error.
    pub fn caution(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: palette().caution,
        }
    }
}

/// The one rule every row control's color follows: a locked row is `palette().disabled`
/// regardless of `active` (focus, or an open dropdown) — locked overrides everything else,
/// so every call site expresses that as data instead of repeating the same `if` three times.
fn locked_fg(locked: bool, active: bool, active_color: Color, inactive_color: Color) -> Color {
    if locked {
        palette().disabled
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
            trailing: &[],
            value_reserve: 0,
            trailing_focused: None,
            leading_button: false,
            leading_focused: false,
            leading_active: false,
            mark: None,
            mark_reserve: false,
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

    /// Puts a [`FocusRow::mark`] dot in this row's right gutter.
    pub fn marked(mut self, color: Color) -> Self {
        self.mark = Some(color);
        self
    }

    /// Gives this row [`trailing`](Self::trailing) buttons. Always built unfocused: which
    /// one is lit belongs to the focused-row tile alone (see `App::list_focus_rows`), so a
    /// shell underneath cannot bake in a highlight that outlives the focus that put it there.
    pub fn with_trailing(mut self, icons: &'static [&'static str]) -> Self {
        self.trailing = icons;
        self
    }

    /// Makes this row's icon a [`leading_button`](Self::leading_button). Always built
    /// unfocused, for the same reason [`with_trailing`](Self::with_trailing) is.
    pub fn with_leading_button(mut self) -> Self {
        self.leading_button = true;
        self
    }
}

// Generous, TV-scale rows — each is its own focusable card (icon + label left,
// control right), consistent with the sidebar/grid's card+focus-ring language
// rather than the bare flat rows the upstream reference uses.
pub const FOCUS_ROW_H: u32 = 92;
pub const FOCUS_ROW_GAP: i32 = 8;
pub const FOCUS_ROW_ICON_SIZE: u32 = 30;
/// Gap between a row's left edge and its icon.
const ICON_PAD: i32 = 24;

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

/// Radius of a [`FocusRow::mark`] dot. A card's title strip wears the same mark.
pub const MARK_DOT_R: i32 = 5;
/// Gap between a row's right edge and whatever sits closest to it — the control on an
/// unmarked row, the mark on a marked one, so both end on the same line.
const CONTROL_PAD: i32 = 28;
/// Gap between the mark and the control it displaces. Tighter than [`CONTROL_PAD`]: the dot
/// belongs to the row's value, so it reads as part of that group rather than as a third
/// column of its own.
const MARK_GAP: i32 = 22;

/// Where a focus row's pieces go: the right edge its control is laid out from, the slider
/// track, and the mark's dot.
///
/// One function for all three, so [`Canvas::focus_row`]'s draw and the pointer path's
/// click/drag hit-testing can't disagree — a dragged thumb lands under the cursor exactly
/// where the track was drawn, marked row or not. A marked row gives the dot's width up off
/// its right edge and every control shifts left by it, so the dot never lands on a dropdown
/// pill, a slider's value slot or a toggle switch.
pub struct RowGeom {
    pub control_right: i32,
    pub track: Rect,
    pub mark: Rect,
}

/// The room a row keeps clear at its right end for the [`FocusRow::mark`] dot, 0 on a list
/// where none is drawn. Every right-anchored piece — the control, the value label, the
/// trailing buttons — starts inside it, so nothing lands under the dot.
pub fn mark_gutter(marked: bool) -> i32 {
    if marked {
        2 * MARK_DOT_R + MARK_GAP
    } else {
        0
    }
}

/// Where a row's label starts — just past its icon. Shared with [`row_layout_for`] so a
/// label-less row's control can begin exactly where the text would have.
pub fn row_label_x(row_rect: Rect) -> i32 {
    row_rect.x() + ICON_PAD + FOCUS_ROW_ICON_SIZE as i32 + 20
}

/// The geometry the renderer will use for `row` — the one source of truth for where its control
/// sits, so a hit test reads exactly the rect that was drawn rather than deriving its own.
pub fn row_geom(row_rect: Rect, row: &FocusRow) -> RowGeom {
    row_layout_for(
        row_rect,
        row.mark.is_some() || row.mark_reserve,
        // A label-less row hands the width its label would have taken to the track — the label
        // lives somewhere else (a card title, say) rather than being blank.
        row.label.is_empty(),
        row.value_reserve.max(row.trailing.len()),
    )
}

/// [`row_layout`], with `wide` spanning the slider track from the label's own x to the value
/// slot rather than giving it a fixed width, and `buttons`
/// trailing buttons' worth of room kept clear at the right end — so a row's control and value
/// stop short of its buttons rather than running under them.
fn row_layout_for(row_rect: Rect, marked: bool, wide: bool, buttons: usize) -> RowGeom {
    let control_right = row_rect.right() - CONTROL_PAD - mark_gutter(marked) - trailing_width(buttons);
    let track_left = control_right - SLIDER_VALUE_SLOT_W - SLIDER_TRACK_GAP;
    let track_w = if wide {
        (track_left - row_label_x(row_rect)).max(1) as u32
    } else {
        220u32.min(row_rect.width() / 3)
    };
    let cy = row_rect.y() + row_rect.height() as i32 / 2;
    RowGeom {
        control_right,
        track: Rect::new(track_left - track_w as i32, cy - 5, track_w, 10),
        mark: Rect::new(
            row_rect.right() - CONTROL_PAD - 2 * MARK_DOT_R,
            cy - MARK_DOT_R,
            2 * MARK_DOT_R as u32,
            2 * MARK_DOT_R as u32,
        ),
    }
}

/// The leading button's rect: the row's icon slot, grown to a button's footprint so the two
/// ends of a row wear the same chrome. Shared by the painter and the pointer hit test.
pub fn leading_button_rect(row_rect: Rect) -> Rect {
    let cy = row_rect.y() + row_rect.height() as i32 / 2;
    Rect::new(
        row_rect.x() + ICON_PAD + (FOCUS_ROW_ICON_SIZE as i32 - SIDEBAR_MENU_BTN as i32) / 2,
        cy - SIDEBAR_MENU_BTN as i32 / 2,
        SIDEBAR_MENU_BTN,
        SIDEBAR_MENU_BTN,
    )
}

/// How much room `count` trailing buttons take off a row's right end — what the row's own
/// value label is pushed left by, so text never runs under a button.
pub fn trailing_width(count: usize) -> i32 {
    count as i32 * (SIDEBAR_MENU_BTN as i32 + TRAILING_GAP)
}

/// Puts every row's value label in one column: each reserves the widest row's
/// trailing-button room, and the mark gutter if any row is marked
/// ([`FocusRow::value_reserve`], [`FocusRow::mark_reserve`]). A right-aligned value otherwise
/// stops wherever its own row's buttons and dot leave off, so one odd row out looks ragged.
pub fn align_values(rows: &mut [FocusRow]) {
    let widest = rows.iter().map(|r| r.trailing.len()).max().unwrap_or(0);
    let any_marked = rows.iter().any(|r| r.mark.is_some());
    for row in rows {
        row.value_reserve = widest;
        row.mark_reserve = any_marked;
    }
}

/// Gap between two trailing buttons, and between the last one and the row's edge.
const TRAILING_GAP: i32 = 10;

/// Trailing button `i` of `count`, packed from the row's right edge in the order they are
/// drawn — inside the [`mark_gutter`], so the last button clears the dot rather than sitting
/// under it. The one-button case is exactly a sidebar row's ⋯, which is what the host menu's
/// Wake row draws — one geometry, so the pointer's per-icon hit test and the painter cannot
/// disagree about where a button is.
pub fn trailing_button_rect(row_rect: Rect, count: usize, i: usize, marked: bool) -> Rect {
    let from_right = count.saturating_sub(i + 1) as i32;
    let stride = SIDEBAR_MENU_BTN as i32 + TRAILING_GAP;
    Rect::new(
        row_rect.right() - mark_gutter(marked) - SIDEBAR_MENU_BTN as i32 - TRAILING_GAP - from_right * stride,
        row_rect.y() + (row_rect.height() as i32 - SIDEBAR_MENU_BTN as i32) / 2,
        SIDEBAR_MENU_BTN,
        SIDEBAR_MENU_BTN,
    )
}

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
            c.focus_row(
                row,
                i == state.focused,
                state.open_dropdown == Some(i),
                switch_frac(row),
                focus_row_rect(area, i),
            )?;
        }
        Ok(())
    }
}

/// Renders one focused row as a tile, composited over the shell. Moving focus
/// recomposites this tile instead of re-rasterizing the whole modal.
/// `switch_frac` animates a `Toggle` row's knob independently.
pub struct FocusRowTile<'a> {
    pub rows: &'a [FocusRow],
    pub content_width: u32,
    pub index: usize,
    pub dropdown_open: bool,
    pub switch_frac: f32,
    /// Which of the row's trailing buttons is lit. Applied here rather than baked into the
    /// row list, because the shell underneath draws the same rows and a highlight baked
    /// there would outlive the focus that put it on.
    pub trailing_focused: Option<usize>,
    /// Same, plus held-open, for the row's leading button — the drag handle of a row being moved.
    pub leading_focused: bool,
    pub leading_active: bool,
}

impl Widget for FocusRowTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let inner = area.inflate(-ROW_TILE_PAD);
        match self.rows.get(self.index) {
            // Cloned to carry this tile's button state: one row, at raster time, next to a
            // full re-render of it.
            Some(row) => {
                let row = FocusRow {
                    trailing_focused: self.trailing_focused,
                    leading_focused: self.leading_focused,
                    leading_active: self.leading_active,
                    ..row.clone()
                };
                c.focus_row(&row, true, self.dropdown_open, self.switch_frac, inner)
            }
            None => Ok(()),
        }
    }
}

impl TileWidget for FocusRowTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        padded_size(self.content_width, FOCUS_ROW_H, ROW_TILE_PAD)
    }
}

/// One unfocused row as its own tile.
///
/// A scrolling list that changes is a stack of these rather than the single baked strip
/// [`FocusRowsTile`] bakes: Settings keyed its whole strip on the whole `Settings` struct, so
/// moving one slider re-rasterized every row — 25-60ms on armv7 per keypress, measured. The
/// value that changed lives in exactly one row, and [`FocusRow::key`] is what lets the cache
/// see that.
pub struct RowTile<'a> {
    pub row: &'a FocusRow,
    pub width: u32,
    pub dropdown_open: bool,
}

impl Widget for RowTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        c.focus_row(self.row, false, self.dropdown_open, switch_frac(self.row), area)
    }
}

impl TileWidget for RowTile<'_> {
    fn size(&self, _fonts: &Fonts) -> (u32, u32) {
        (self.width.max(1), FOCUS_ROW_H)
    }
}

/// A [`RowKind::Toggle`]'s knob position at rest. The switch animates from its own tile
/// while a toggle is mid-flip; a row drawn as part of a list is always at one end or the
/// other.
fn switch_frac(row: &FocusRow) -> f32 {
    if row.value == "On" {
        1.0
    } else {
        0.0
    }
}

/// A [`Color`] flattened to something hashable — [`Color`] itself is only `Eq`.
type Rgba8 = (u8, u8, u8, u8);

/// A row's pixels as something hashable — the version its [`RowTile`] is valid at.
///
/// Borrowed from the row rather than cloned: `cache::version` keeps only the hash, so this
/// never outlives the `Vec<FocusRow>` the caller built for this frame.
///
/// Hand-written rather than `#[derive(Hash)]` on [`FocusRow`], because `fraction` is an
/// `f32` and `cache::version` refuses floats on purpose — a tile keyed on a clock rebuilds
/// every frame. A slider's fill is not a clock: it moves only when the setting behind it
/// does. So it is hashed by bit pattern here, deliberately and in one place, rather than by
/// making `FocusRow` blanket-hashable.
#[derive(PartialEq, Eq, Hash)]
pub struct FocusRowKey<'a> {
    icon: &'a str,
    label: &'a str,
    value: &'a str,
    kind: RowKind,
    fraction_bits: u32,
    danger: bool,
    locked: bool,
    trailing: &'a [&'static str],
    value_reserve: usize,
    trailing_focused: Option<usize>,
    leading_button: bool,
    leading_focused: bool,
    leading_active: bool,
    mark: Option<Rgba8>,
    mark_reserve: bool,
    subtext: Option<(&'a str, Rgba8)>,
}

fn color_bytes(c: Color) -> Rgba8 {
    (c.r, c.g, c.b, c.a)
}

impl FocusRow {
    /// Everything [`Canvas::focus_row`] reads off this row, hashable. See [`FocusRowKey`].
    pub fn key(&self) -> FocusRowKey<'_> {
        FocusRowKey {
            icon: self.icon,
            label: &self.label,
            value: &self.value,
            kind: self.kind,
            fraction_bits: self.fraction.to_bits(),
            danger: self.danger,
            locked: self.locked,
            trailing: self.trailing,
            value_reserve: self.value_reserve,
            trailing_focused: self.trailing_focused,
            leading_button: self.leading_button,
            leading_focused: self.leading_focused,
            leading_active: self.leading_active,
            mark: self.mark.map(color_bytes),
            mark_reserve: self.mark_reserve,
            subtext: self.subtext.as_ref().map(|s| (s.text.as_str(), color_bytes(s.color))),
        }
    }
}

impl Canvas<'_, '_> {
    /// A [`FocusRow::mark`]-sized dot, from its left edge and vertical centre — for callers
    /// placing one outside a row (the card title strip), so the two marks can't drift apart.
    pub fn mark_dot(&mut self, left: i32, cy: i32, color: Color) {
        self.painter.fill_rounded_rect(
            Rect::new(left, cy - MARK_DOT_R, 2 * MARK_DOT_R as u32, 2 * MARK_DOT_R as u32),
            MARK_DOT_R,
            color,
        );
    }

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

        // Past the row's control, on the right edge — the width `row_layout` took off it.
        let marked = row.mark.is_some() || row.mark_reserve;
        let geom = row_geom(row_rect, row);
        if let Some(color) = row.mark {
            self.painter.fill_rounded_rect(geom.mark, MARK_DOT_R, color);
        }

        let icon_rect = Rect::new(
            row_rect.x() + ICON_PAD,
            row_rect.y() + (row_rect.height() as i32 - FOCUS_ROW_ICON_SIZE as i32) / 2,
            FOCUS_ROW_ICON_SIZE,
            FOCUS_ROW_ICON_SIZE,
        );
        // WHY: destructive rows stay red even unfocused — to signal danger before confirm.
        let fg = if row.danger {
            palette().error
        } else if focused {
            palette().text
        } else {
            palette().muted
        };
        if row.leading_button {
            self.row_button(
                leading_button_rect(row_rect),
                row.icon,
                focused,
                row.leading_focused,
                row.leading_active,
            )?;
        } else {
            self.icon(icon_rect, row.icon, fg)?;
        }
        let label_x = row_label_x(row_rect);
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

        let value_y = row_rect.y() + (row_rect.height() as i32 - self.fonts.raster.height(value_font)) / 2;
        match row.kind {
            RowKind::Dropdown => {
                let value_fg = locked_fg(row.locked, focused || dropdown_open, palette().text, palette().muted);
                self.dropdown_value(row_rect, geom.control_right, &row.value, value_fg)?;
            }
            RowKind::Slider => {
                let value_w = self.fonts.raster.measure(value_font, &row.value).0;
                self.text(
                    value_font,
                    &row.value,
                    geom.control_right - value_w as i32,
                    value_y,
                    locked_fg(row.locked, focused, palette().text, palette().muted),
                )?;
                self.painter
                    .slider_with_thumb(geom.track, row.fraction, focused, !row.locked);
            }
            RowKind::Toggle => {
                let switch = Rect::new(
                    geom.control_right - 64,
                    row_rect.y() + (row_rect.height() as i32 - 34) / 2,
                    64,
                    34,
                );
                self.painter.switch(switch, switch_frac, !row.locked);
            }
            // Action rows have no control; `value` is a muted hint only, never interactive.
            RowKind::Action => {
                if !row.value.is_empty() {
                    let value_w = self.fonts.raster.measure(value_font, &row.value).0;
                    self.text(
                        value_font,
                        &row.value,
                        geom.control_right - value_w as i32,
                        value_y,
                        palette().muted,
                    )?;
                }
            }
        }
        for (i, &icon) in row.trailing.iter().enumerate() {
            self.row_button(
                trailing_button_rect(row_rect, row.trailing.len(), i, marked),
                icon,
                focused,
                row.trailing_focused == Some(i),
                false,
            )?;
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
                if enabled { palette().accent } else { palette().disabled },
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
            locked_fg(!enabled, focused, palette().text, palette().muted),
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
        let on_track = if enabled { palette().accent } else { palette().disabled };
        self.fill_rounded_rect(rect, radius, lerp_color(SWITCH_OFF_TRACK, on_track, frac));
        let knob_r = radius as f32 - 4.0;
        let cy = rect.y() as f32 + rect.height() as f32 / 2.0;
        let left = rect.x() as f32 + radius as f32;
        let right = rect.x() as f32 + rect.width() as f32 - radius as f32;
        let cx = left + (right - left) * frac;
        self.fill_circle(cx + 1.0, cy + 2.0, knob_r, Color::RGBA(0x00, 0x00, 0x00, 0x40));
        self.fill_circle(
            cx,
            cy,
            knob_r,
            if enabled { palette().text } else { palette().disabled },
        );
    }
}

/// A settings row's on-screen rect at a pixel scroll offset — the smooth-scroll counterpart
/// of [`focus_row_rect`], which indexes rows within the viewport instead. Can land partly (or
/// wholly) outside `content_rect` while the viewport is gliding; callers clip.
pub fn focus_row_rect_at_px(content_rect: Rect, index: usize, scroll_px: i32) -> Rect {
    let y = content_rect.y() + index as i32 * focus_row_stride() as i32 - scroll_px;
    Rect::new(content_rect.x(), y, content_rect.width(), FOCUS_ROW_H)
}
