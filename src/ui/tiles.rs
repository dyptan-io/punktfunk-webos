//! Rasterized-once tile sources for the GPU compositor.
//!
//! Split out of the former single-file `ui.rs`; see `super`'s module docs.
use crate::ui::prelude::*;
use anyhow::Result;
use tiny_skia::Pixmap;

// ---------------------------------GPU tiles-----------------------------------
// The compositor path (see `compositor.rs` + `App::prepare_tiles`): widgets are
// rasterized by tiny-skia into standalone padded tiles ONCE (keeping the AA/soft
// shadow look), then composed per frame by the GPU — position, scroll, the focus
// pop's scale, and fades are all texture-copy parameters, not re-rasterization.

/// Transparent margin the card's drop shadow (dx 3 / dy 5 / blur 14) needs around the
/// card itself — the padding of [`render_card_shadow_tile`]'s canvas, and how far past the
/// viewport a card can still be visible.
pub const CARD_SHADOW_PAD: i32 = 20;

/// A card-sized shape on a canvas with `pad` of transparent margin all round, handed the
/// card's own rect within it. The shared shell of the three card decoration tiles below,
/// which differ only in their pad and the one call they make.
fn padded_card_tile(w: u32, h: u32, pad: i32, draw: impl FnOnce(&mut Painter, Rect)) -> Painter {
    let mut p = Painter::new(w + 2 * pad as u32, h + 2 * pad as u32);
    draw(&mut p, Rect::new(pad, pad, w, h));
    p
}

/// The card drop shadow as a shared tile (all cards are one size), composited *behind*
/// each card rather than baked into it. Every card's shadow is identical, so baking it in
/// bought nothing and cost every card tile a 20px margin a side — ~35% more pixels
/// rasterized, uploaded and blended per card.
pub fn render_card_shadow_tile(w: u32, h: u32) -> Painter {
    padded_card_tile(w, h, CARD_SHADOW_PAD, |p, r| p.card_shadow(r, CARD_RADIUS))
}

/// Grid card (unfocused), exactly card-sized. GPU scales it and composites the shadow,
/// focus ring, title strip and outline around it.
pub fn render_card_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    card_w: u32,
    card_h: u32,
    title: &str,
    art: Option<&Pixmap>,
) -> Painter {
    let mut p = Painter::new(card_w, card_h);
    Canvas::tile(&mut p, text_cache, fonts).poster_art(Rect::new(0, 0, card_w, card_h), title, art);
    p
}

/// The focused card's title strip as its own tile, exactly card-wide.
///
/// Frost needs something to blur, so the card's own art is re-drawn here translated up by
/// everything above the strip: the strip's slice of the cover lands at y 0 and the rest
/// falls off the canvas, where tiny-skia clips it. One small blur per focus move — a
/// fraction of the card build happening at that same rate — and nothing per frame, since
/// the wipe is a crop of this tile (see `app::render::compose`).
pub fn render_card_title_tile(
    text_cache: &mut TextCache,
    fonts: &Fonts,
    card_w: u32,
    card_h: u32,
    title: &str,
    art: Option<&Pixmap>,
) -> Result<Painter> {
    let strip_h = title_strip_h(fonts.raster, fonts.value, card_h);
    let mut p = Painter::new(card_w.max(1), strip_h);
    let mut c = Canvas::tile(&mut p, text_cache, fonts);
    let strip = Rect::new(0, 0, card_w, strip_h);
    c.poster_art(Rect::new(0, -((card_h - strip_h) as i32), card_w, card_h), title, art);
    c.poster_title_strip(strip, title)?;
    Ok(p)
}

/// Transparent padding around the focus-ring tile — must clear
/// `FOCUS_GLOW_BLUR`'s blur radius or the glow clips against the canvas edge.
pub const FOCUS_RING_PAD: i32 = 24;

/// Focus-ring glow as shared tile (all cards same size). GPU scales + fades.
pub fn render_focus_ring_tile(w: u32, h: u32) -> Painter {
    padded_card_tile(w, h, FOCUS_RING_PAD, Painter::focus_ring)
}

/// Transparent padding around the card-outline tile — just enough for the stroke's own
/// width/AA, not a blur radius like [`FOCUS_RING_PAD`].
pub const CARD_OUTLINE_PAD: i32 = 4;

/// The focused card's crisp lit edge as a shared tile (all cards are one size),
/// composited on top of the card art — see [`Painter::card_outline`].
pub fn render_card_outline_tile(w: u32, h: u32) -> Painter {
    padded_card_tile(w, h, CARD_OUTLINE_PAD, Painter::card_outline)
}

/// Diameter of the pinned badge composited over the focused grid/pinned
/// card's top-right corner (see `tile::PIN_BADGE`).
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
    let content = confirm_dialog_stack(fonts, card, subtitle).split(card)[2].inset_x(CONFIRM_DIALOG_SIDE_PAD);
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
