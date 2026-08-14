//! Rasterized-once tile sources for the GPU compositor.
//!
//! Split out of the former single-file `ui.rs`; see `super`'s module docs.
use crate::core::model::{GamepadType, LogLevelOverride, Settings};
use crate::core::screen::Screen;
use crate::ui::prelude::*;
use anyhow::Result;
use tiny_skia::Pixmap;

// ---------------------------------GPU tiles-----------------------------------
// The compositor path (see `compositor.rs` + `App::prepare_tiles`): widgets are
// rasterized by tiny-skia into standalone padded tiles ONCE (keeping the AA/soft
// shadow look), then composed per frame by the GPU — position, scroll, the focus
// pop's scale, and fades are all texture-copy parameters, not re-rasterization.

/// Transparent padding around a card tile so its drop shadow (dx 3 / dy 5 /
/// blur 14) fits inside the tile instead of clipping at its edge.
pub const CARD_TILE_PAD: i32 = 20;

/// Grid card as padded tile (unfocused). GPU scales + composites focus ring.
pub fn render_card_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    card_w: u32,
    card_h: u32,
    title: &str,
    art: Option<&Pixmap>,
) -> Result<Painter> {
    let pad = CARD_TILE_PAD;
    let mut p = Painter::new(card_w + 2 * pad as u32, card_h + 2 * pad as u32);
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    c.poster_card(Rect::new(pad, pad, card_w, card_h), title, art, false)?;
    Ok(p)
}

/// Transparent padding around the focus-ring tile — must clear
/// `FOCUS_GLOW_BLUR`'s blur radius or the glow clips against the canvas edge.
pub const FOCUS_RING_PAD: i32 = 20;

/// Focus-ring glow as shared tile (all cards same size). GPU scales + fades.
pub fn render_focus_ring_tile(w: u32, h: u32) -> Painter {
    let pad = FOCUS_RING_PAD;
    let mut p = Painter::new(w + 2 * pad as u32, h + 2 * pad as u32);
    p.focus_ring(Rect::new(pad, pad, w, h), CARD_RADIUS);
    p
}

/// Transparent padding around the card-outline tile — just enough for the
/// stroke's own width/AA, not a blur radius like `FOCUS_RING_PAD`.
pub const CARD_OUTLINE_PAD: i32 = 4;

/// The focused card's crisp edge outline as a shared tile (all cards same
/// size), composited on top of the card art — see `draw_card_outline`.
pub fn render_card_outline_tile(w: u32, h: u32) -> Painter {
    let pad = CARD_OUTLINE_PAD;
    let mut p = Painter::new(w + 2 * pad as u32, h + 2 * pad as u32);
    p.card_outline(Rect::new(pad, pad, w, h));
    p
}

/// Diameter of the pinned badge composited over the focused grid/pinned
/// card's top-right corner (see `Tile::PinBadge`).
pub const PIN_BADGE_SIZE: u32 = 28;

/// Pinned badge: dark disc with PIN icon. Single shared tile.
pub fn render_pin_badge_tile(text_cache: &mut TextCache, fonts: &Fonts) -> Result<Painter> {
    let d = PIN_BADGE_SIZE;
    let mut p = Painter::new(d, d);
    let mid = d as f32 / 2.0;
    p.fill_circle(mid, mid, mid, Color::RGBA(0x00, 0x00, 0x00, 0x70));
    let icon = (d as f32 * 0.6) as u32;
    let icon_rect = Rect::new(((d - icon) / 2) as i32, ((d - icon) / 2) as i32, icon, icon);
    Canvas::tile(&mut p, text_cache, fonts).icon(icon_rect, icons().pin, theme().muted)?;
    Ok(p)
}

/// Padding for row tile shadow + sidebar inflate. Settings rows use GPU scale.
pub const ROW_TILE_PAD: i32 = 28;

/// A single line of text as its own tight transparent tile.
pub fn render_text_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    font: FontId,
    text: &str,
    color: Color,
) -> Result<Painter> {
    let (w, h) = fonts.raster.measure(font, text);
    let mut p = Painter::new(w.max(1), h.max(1));
    Canvas::tile(&mut p, text_cache, fonts).text(font, text, 0, 0, color)?;
    Ok(p)
}

/// A wrapped text block as its own transparent tile (`max_w` wide, as tall as
/// its wrapped line count).
#[allow(clippy::too_many_arguments)]
pub fn render_wrapped_text_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    font: FontId,
    text: &str,
    max_w: u32,
    color: Color,
    line_gap: i32,
) -> Result<Painter> {
    let line_h = fonts.raster.height(font) + line_gap;
    let lines = wrap_text(fonts.raster, font, text, max_w).len().max(1) as u32;
    let mut p = Painter::new(max_w.max(1), lines * line_h.max(1) as u32);
    Canvas::tile(&mut p, text_cache, fonts).text_wrapped(font, text, 0, 0, max_w, color, line_gap)?;
    Ok(p)
}

/// Worst-case stat line used to lock overlay width.
pub const STATS_OVERLAY_REF_LINE: &str = "3840x2160@120 HEVC HDR";

/// In-stream stats overlay with fixed width and centered hint.
/// `lines[0]` is highlighted; remaining lines are muted.
pub fn render_stats_overlay_tile(fonts: &Fonts, lines: &[String], hint: &str) -> Result<Painter> {
    let (raster, font, caption_font) = (fonts.raster, fonts.value, fonts.caption);
    let pad = 18i32;
    let content_safety = 16u32;
    let line_h = raster.height(font) + 6;
    let caption_h = raster.height(caption_font);
    let hint_h = caption_h + 8;
    let line_count = lines.len() as i32;

    let inner_w = raster.measure(font, STATS_OVERLAY_REF_LINE).0 + content_safety;
    let w = inner_w + 2 * pad as u32;
    let h = (line_count * line_h + hint_h + 2 * pad) as u32;
    let content_w_i32 = w as i32 - 2 * pad;

    let mut p = Painter::new(w.max(1), h.max(1));
    let mut tc = TextCache::new();
    p.fill_rounded_rect(Rect::new(0, 0, w, h), 14, Color::RGBA(0x14, 0x10, 0x1f, 0x70));
    let mut c = Canvas::tile(&mut p, &mut tc, fonts);

    for (i, line) in lines.iter().enumerate() {
        let color = if i == 0 { theme().text } else { theme().muted };
        let y = pad + i as i32 * line_h;
        c.text(font, line, pad, y, color)?;
    }

    let hint_y = pad + line_count * line_h + (hint_h - caption_h);
    let hint_w = raster.measure(caption_font, hint).0 as i32;
    let hint_x = pad + (content_w_i32 - hint_w) / 2;
    c.text(caption_font, hint, hint_x, hint_y, theme().muted)?;
    Ok(p)
}

/// Number of lines shown in the log-tail overlay.
pub const LOG_OVERLAY_LINES: usize = 9;

/// Color for log line by level prefix; errors/warnings highlighted to stand out.
fn log_line_color(line: &str) -> Color {
    match line.split_whitespace().next() {
        Some("ERROR") => theme().error,
        Some("WARN") => theme().warning,
        Some("INFO") => theme().text,
        _ => theme().muted,
    }
}

/// Left indent for a wrapped log line's 2nd+ row, so it reads as a continuation.
const LOG_OVERLAY_WRAP_INDENT: i32 = 20;

/// Full-width log-tail at screen bottom (all screens, unlike stats overlay) — a
/// constant left-to-right size regardless of content. Long lines word-wrap
/// instead of clipping, only once they'd actually reach the screen edge.
pub fn render_log_overlay_tile(fonts: &Fonts, screen_w: u32, lines: &[String]) -> Result<Painter> {
    let (raster, font) = (fonts.raster, fonts.caption);
    let pad = 14i32;
    let line_h = raster.height(font) + 4;
    let inner_w = screen_w.saturating_sub(2 * pad as u32);
    // Narrowed by the indent so continuation rows fit too; first row just has slack.
    let wrap_w = inner_w.saturating_sub(LOG_OVERLAY_WRAP_INDENT as u32).max(1);
    let wrapped: Vec<(Vec<String>, Color)> = lines
        .iter()
        .map(|line| (wrap_text(raster, font, line, wrap_w), log_line_color(line)))
        .collect();
    let total_rows: usize = wrapped.iter().map(|(rows, _)| rows.len().max(1)).sum();
    let h = (total_rows.max(1) as i32 * line_h + 2 * pad).max(1) as u32;
    let mut p = Painter::new(screen_w.max(1), h);
    let mut tc = TextCache::new();
    p.fill_rounded_rect(
        Rect::new(0, 0, screen_w.max(1), h),
        14,
        Color::RGBA(0x14, 0x10, 0x1f, 0xb8),
    );
    let mut c = Canvas::tile(&mut p, &mut tc, fonts);
    let mut row = 0i32;
    for (wrapped_rows, color) in &wrapped {
        // Empty line wraps to zero rows — still reserve one so later lines don't creep up.
        if wrapped_rows.is_empty() {
            row += 1;
            continue;
        }
        for (i, text) in wrapped_rows.iter().enumerate() {
            let x = if i == 0 { pad } else { pad + LOG_OVERLAY_WRAP_INDENT };
            c.text(font, text, x, pad + row * line_h, *color)?;
            row += 1;
        }
    }
    Ok(p)
}

/// Confirm dialog card rect: [`SIMPLE_MODAL_WIDTH_FRAC`] wide, height driven by the
/// wrapped subtitle plus room for the button row. Split from [`confirm_dialog_layout`]
/// so callers that only need the card (hit-testing the close button, positioning the
/// shell) skip the second subtitle wrap the button-row rect would cost.
pub fn confirm_dialog_card(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str) -> Rect {
    simple_modal_card(screen_w, screen_h, |probe| {
        confirm_dialog_stack(fonts, probe, subtitle).total_length()
    })
}

/// A confirm dialog's vertical stack: header, gap, button row, bottom pad. Read for the
/// card's height and, once placed, for the button row's rect — the same split both times.
fn confirm_dialog_stack(fonts: &Fonts, card: Rect, subtitle: &str) -> Layout {
    let header_h = (modal_header_end_y(fonts, card, subtitle) - card.y()).max(0) as u32;
    Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Length(CONFIRM_DIALOG_GAP),
        Constraint::Length(CONFIRM_BUTTON_ROW_H),
        Constraint::Length(CONFIRM_DIALOG_GAP),
    ])
}

/// Gap above and below a confirm dialog's button row.
const CONFIRM_DIALOG_GAP: u32 = 32;
/// Height of that row.
const CONFIRM_BUTTON_ROW_H: u32 = 72;
/// Left/right inset of the dialog's content column.
const CONFIRM_DIALOG_SIDE_PAD: u32 = 32;

/// Confirm dialog card + button-row rects, sized exactly like the `App`'s simple modals
/// (forget host, send logs): [`SIMPLE_MODAL_WIDTH_FRAC`] wide, height driven by the wrapped
/// subtitle. Shared with `main.rs`'s in-stream/quit dialog so all four modals match.
pub fn confirm_dialog_layout(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str) -> (Rect, Rect) {
    let card = confirm_dialog_card(screen_w, screen_h, fonts, subtitle);
    let buttons = confirm_dialog_stack(fonts, card, subtitle).split(card)[2];
    let content = Layout::horizontal([
        Constraint::Length(CONFIRM_DIALOG_SIDE_PAD),
        Constraint::Fill(1),
        Constraint::Length(CONFIRM_DIALOG_SIDE_PAD),
    ])
    .split(buttons)[1];
    (card, content)
}

/// Button index under `(x, y)` within a confirm dialog's button-row `content` rect, or
/// `None` off both buttons — the shared hit-test the App confirm modals' hover arms and
/// the in-stream Disconnect dialog use so pointer focus behaves identically.
pub fn confirm_button_at(content: Rect, x: i32, y: i32) -> Option<usize> {
    (0..2).find(|&i| confirm_button_rect(content, i).contains_point((x, y)))
}

/// Confirm dialog shell (full-screen tile): the same card + close (X) + header + unfocused
/// buttons that `Canvas::modal_shell`/`Canvas::modal_header` paint for the forget-host and
/// send-logs modals — replicated here because the streaming/quit loops have no `App`. The
/// focused button composites on top as its own small tile (shell/focus-tile split).
pub fn render_confirm_dialog_shell(
    screen_w: u32,
    screen_h: u32,
    fonts: &Fonts,
    title: &str,
    subtitle: &str,
    buttons: &[ConfirmButton; 2],
) -> Result<Painter> {
    let mut p = Painter::new(screen_w, screen_h);
    let mut tc = TextCache::new();
    let (card, content) = confirm_dialog_layout(screen_w, screen_h, fonts, subtitle);
    p.modal_card(card);
    let mut c = Canvas::tile(&mut p, &mut tc, fonts);
    // These dialogs are remote/controller-driven (no local pointer over the overlay), so the
    // X is a visual affordance only — always in the unhovered color.
    c.icon(modal_close_rect(card), icons().close, theme().muted)?;
    c.modal_header(card, title, theme().text, subtitle, theme().muted)?;
    c.render(ConfirmButtons::new(buttons), content)?;
    Ok(p)
}

// -------------------------- tile cache + its render keys ----------------------
// Moved out of `app` (was `app::tiles` + two key enums in `app/mod.rs`) so the
// render cache and its staleness keys are `ui`-owned — no `App` reference — which
// is what lets a `ui`-only harness render without `app`. The keys still name this
// app's screens, which is the next thing to make opaque.

/// Focused widget in the open modal. Each variant carries its content,
/// so value changes (not just focus moves) invalidate the tile.
#[derive(PartialEq)]
pub enum ModalFocusKey {
    /// The detected pad type rides along because the Controller row's "Automatic (...)" value
    /// depends on it, not just on `Settings` — a hotplug alone doesn't touch `Settings` at all.
    SettingsRow(usize, Settings, Option<GamepadType>),
    WakeToggle(bool),
    WakeButton(usize),
    PairingDigit(usize, u8),
    PairingButton,
    ForgetButton(usize),
    /// Carries label to prevent stale tiles across screen changes.
    SpeedTestButton(usize, String),
    /// Carries label+menu flag for row list shape changes and ⋯ state.
    MenuRow(usize, String, bool),
    /// (focused row, log level, stats-overlay on, show-logs on) — any change invalidates the tile.
    DiagnosticsRow(usize, LogLevelOverride, bool, bool),
    /// (focused row, frame-pacing on, game-mode on) — any change invalidates the tile.
    ExperimentalRow(usize, bool, bool),
    /// (focused row, cursor-capture on, cursor-gestures on) — any change invalidates the tile.
    CursorSettingsRow(usize, bool, bool),
    /// Which `Screen::SendLogs` button is focused (0 = Cancel, 1 = Send).
    SendLogsButton(usize),
}

/// Scrollable modal content keys. Paired with Screen for staleness checks.
#[derive(Clone, PartialEq)]
pub enum ScrollContentKey {
    /// Settings row list + open dropdown row + detected pad type (see `ModalFocusKey::SettingsRow`).
    Settings(Settings, Option<usize>, Option<GamepadType>),
    /// About window's start line.
    About(usize),
}

/// The 17 rasterized-once tile sources for the GPU compositor (`compositor.rs`), keyed as
/// each render path needs. `prepare_tiles` rebuilds whichever are stale and reports them for
/// upload; `draw_list` composes each frame from their textures. Focus movement, scrolling,
/// and animations never re-rasterize anything.
pub struct TileCache {
    /// Focus-free sidebar strip (`SIDEBAR_W` × screen height): panel, brand mark +
    /// wordmark, every row unfocused. Stale when row content changes (`sidebar_dirty`),
    /// never on focus movement.
    pub(crate) sidebar_layer: Option<Painter>,
    /// Per-card tiles (shadow baked in, transparent padding), keyed by pin id
    /// (a `GameEntry::id`, or `store::DESKTOP_PIN_ID`) rather than grid index —
    /// a pin/unpin reorder only shuffles which index a game sits at, so keying
    /// by identity means the reorder never has to rebuild anything. Absent = not
    /// yet rasterized (or evicted).
    pub(crate) card_tiles: std::collections::HashMap<String, Painter>,
    /// The shared focus-ring glow tile (one per card size).
    pub(crate) ring_tile: Option<Painter>,
    /// The shared card-outline tile (one per card size) — composited on top of the
    /// focused card's art, unlike `ring_tile` which sits behind it.
    pub(crate) outline_tile: Option<Painter>,
    /// The shared pinned badge tile — built once (it doesn't depend on card size),
    /// composited over the focused card when that card is pinned.
    pub(crate) pin_badge_tile: Option<Painter>,
    /// The focused sidebar row's tile, keyed by row index.
    pub(crate) focused_row_tile: Option<((usize, bool), Painter)>,
    /// The active modal rasterized full-screen (transparent surroundings). Always the
    /// *shell* — every selectable widget drawn unfocused — with the focused one composited
    /// on top from `modal_focus_tile` (see `ModalFocusKey`'s docs).
    pub(crate) modal_tile: Option<Painter>,
    /// The single focused, zoom-animated widget of whichever modal is open —
    /// see `ModalFocusKey`'s docs on why one tile/key suffices for all of them.
    pub(crate) modal_focus_tile: Option<(ModalFocusKey, Painter)>,
    /// Dropdown overlay panel, keyed by (Screen, row) to disambiguate row 0 across
    /// Settings vs Diagnostics. Composited after `ScrollContent`.
    pub(crate) dropdown_overlay_tile: Option<((Screen, usize), Painter)>,
    /// Dropdown's focused option tile, keyed by (Screen, row, focused index).
    /// Composited over `DropdownOverlay`; focus movement rebuilds only this.
    pub(crate) dropdown_focus_tile: Option<((Screen, usize, usize), Painter)>,
    /// Whichever scrollable modal's indicator is baked, keyed by `(total units,
    /// visible units, scroll offset)`. One slot for all of them.
    pub(crate) scroll_indicator_tile: Option<((usize, usize, usize), Painter)>,
    /// Whichever scrollable modal's content is baked, at full (unscrolled) height —
    /// keyed by `(Screen, ScrollContentKey)`. Scrolling within the baked window never
    /// invalidates this.
    pub(crate) scroll_content_tile: Option<((Screen, ScrollContentKey), Painter)>,
    /// The bottom scroll fade. Unkeyed and built at most once per run: a fixed-size alpha
    /// ramp the GPU stretches to each list's width.
    pub(crate) scroll_fade_tile: Option<Painter>,
    /// The mirrored fade for the top edge.
    pub(crate) scroll_fade_top_tile: Option<Painter>,
    /// Home's status line block, keyed by its text.
    pub(crate) status_tile: Option<(String, Painter)>,
    /// The static "No host selected" hint line.
    pub(crate) nohost_tile: Option<Painter>,
}

impl TileCache {
    pub fn new() -> Self {
        Self {
            sidebar_layer: None,
            card_tiles: std::collections::HashMap::new(),
            ring_tile: None,
            outline_tile: None,
            pin_badge_tile: None,
            focused_row_tile: None,
            modal_tile: None,
            modal_focus_tile: None,
            dropdown_overlay_tile: None,
            dropdown_focus_tile: None,
            scroll_indicator_tile: None,
            scroll_content_tile: None,
            scroll_fade_tile: None,
            scroll_fade_top_tile: None,
            status_tile: None,
            nohost_tile: None,
        }
    }
}
