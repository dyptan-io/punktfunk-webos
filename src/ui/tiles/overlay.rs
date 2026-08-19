//! The two debug overlays: the in-stream stats panel and the log tail.
//!
//! Both are rebuilt at ~2Hz from text that changes every time, so unlike every other tile here
//! their cost is in the rasterization, not in the compositing. They draw through a long-lived
//! `TextCache` of their own (see `runtime`), separate from the menu's — a wall of log lines would
//! otherwise evict every glyph run the UI depends on.
use crate::ui::prelude::*;
use anyhow::Result;

/// Worst-case stat line the overlay locks its width to.
const STATS_OVERLAY_REF_LINE: &str = "3840x2160@120 HEVC HDR";

/// Extra width past [`STATS_OVERLAY_REF_LINE`], as digits of the stat font. The reference line is
/// no longer the widest one — the audio line carries a layout name and two figures — and a line
/// that overruns the tile is clipped, not wrapped.
const STATS_OVERLAY_SLACK: &str = "00000";

/// Padding inside both overlays' rounded panel.
const STATS_PAD: i32 = 18;
/// Slack past the measured reference line, absorbing per-font rounding.
const STATS_CONTENT_SAFETY: u32 = 16;

/// In-stream stats overlay with fixed width and centered hint. `lines[0]` is highlighted;
/// remaining lines are muted.
pub struct StatsOverlayTile<'a> {
    pub lines: &'a [String],
    pub hint: &'a str,
}

impl StatsOverlayTile<'_> {
    /// `(line height, hint band height)` — the two strides both `size` and `render` step by.
    fn metrics(fonts: &Fonts) -> (i32, i32) {
        let caption_h = fonts.raster.height(fonts.caption);
        (fonts.raster.height(fonts.value) + 6, caption_h + 8)
    }
}

impl Widget for StatsOverlayTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let (font, caption_font) = (c.fonts.value, c.fonts.caption);
        let (line_h, hint_h) = Self::metrics(c.fonts);
        let caption_h = c.fonts.raster.height(caption_font);
        let (w, h) = (area.width(), area.height());
        c.painter
            .fill_rounded_rect(Rect::new(0, 0, w, h), 14, Color::RGBA(0x14, 0x10, 0x1f, 0x70));
        for (i, line) in self.lines.iter().enumerate() {
            let color = if i == 0 { theme().text } else { theme().muted };
            c.text(font, line, STATS_PAD, STATS_PAD + i as i32 * line_h, color)?;
        }
        let hint_y = STATS_PAD + self.lines.len() as i32 * line_h + (hint_h - caption_h);
        let hint_w = c.fonts.raster.measure(caption_font, self.hint).0 as i32;
        let hint_x = STATS_PAD + (w as i32 - 2 * STATS_PAD - hint_w) / 2;
        c.text(caption_font, self.hint, hint_x, hint_y, theme().muted)?;
        Ok(())
    }
}

impl TileWidget for StatsOverlayTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        let (raster, font) = (fonts.raster, fonts.value);
        let (line_h, hint_h) = Self::metrics(fonts);
        let inner_w = raster.measure(font, STATS_OVERLAY_REF_LINE).0
            + raster.measure(font, STATS_OVERLAY_SLACK).0
            + STATS_CONTENT_SAFETY;
        (
            inner_w + 2 * STATS_PAD as u32,
            (self.lines.len() as i32 * line_h + hint_h + 2 * STATS_PAD) as u32,
        )
    }
}

/// Number of lines shown in the log-tail overlay.
pub const LOG_OVERLAY_LINES: usize = 9;

/// Padding inside the log overlay's panel.
const LOG_PAD: i32 = 14;

/// Left indent for a wrapped log line's 2nd+ row, so it reads as a continuation.
const LOG_OVERLAY_WRAP_INDENT: i32 = 20;

/// Color for a log line by level prefix; errors/warnings highlighted to stand out.
fn log_line_color(line: &str) -> Color {
    match line.split_whitespace().next() {
        Some("ERROR") => theme().error,
        Some("WARN") => theme().warning,
        Some("INFO") => theme().text,
        _ => theme().muted,
    }
}

/// Full-width log tail at the screen bottom (all screens, unlike the stats overlay) — a constant
/// left-to-right size regardless of content. Long lines word-wrap instead of clipping, only once
/// they would actually reach the screen edge.
pub struct LogOverlayTile<'a> {
    pub screen_w: u32,
    pub lines: &'a [String],
}

impl LogOverlayTile<'_> {
    /// Width a line wraps to: the panel's interior, narrowed by the continuation indent so
    /// second and later rows fit too (the first row just has slack).
    fn wrap_w(&self) -> u32 {
        self.screen_w
            .saturating_sub(2 * LOG_PAD as u32)
            .saturating_sub(LOG_OVERLAY_WRAP_INDENT as u32)
            .max(1)
    }

    /// Visual rows the tail occupies once wrapped. An empty line wraps to zero rows but still
    /// reserves one, so later lines do not creep up.
    fn rows(&self, fonts: &Fonts) -> usize {
        self.lines
            .iter()
            .map(|l| wrap_text(fonts.raster, fonts.caption, l, self.wrap_w()).len().max(1))
            .sum()
    }
}

impl Widget for LogOverlayTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        let font = c.fonts.caption;
        let line_h = c.fonts.raster.height(font) + 4;
        let wrap_w = self.wrap_w();
        c.painter.fill_rounded_rect(
            Rect::new(0, 0, area.width(), area.height()),
            14,
            Color::RGBA(0x14, 0x10, 0x1f, 0xb8),
        );
        let mut row = 0i32;
        for line in self.lines {
            let color = log_line_color(line);
            let wrapped = wrap_text(c.fonts.raster, font, line, wrap_w);
            if wrapped.is_empty() {
                row += 1;
                continue;
            }
            for (i, text) in wrapped.iter().enumerate() {
                let x = if i == 0 {
                    LOG_PAD
                } else {
                    LOG_PAD + LOG_OVERLAY_WRAP_INDENT
                };
                c.text(font, text, x, LOG_PAD + row * line_h, color)?;
                row += 1;
            }
        }
        Ok(())
    }
}

impl TileWidget for LogOverlayTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        let line_h = fonts.raster.height(fonts.caption) + 4;
        (
            self.screen_w.max(1),
            (self.rows(fonts).max(1) as i32 * line_h + 2 * LOG_PAD).max(1) as u32,
        )
    }
}
