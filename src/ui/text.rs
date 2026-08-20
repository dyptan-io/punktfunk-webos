//! Text/icon cache and drawing. Rasterization itself goes through `TextRaster`, and the
//! font *bytes* come from the app (`crate::assets`) — a widget library names font roles,
//! not a brand.
use crate::ui::prelude::*;
pub use crate::ui::text_raster::{FontId, TextRaster};
use anyhow::Result;
use std::collections::HashMap;
use tiny_skia::Pixmap;

/// Geist weight to load (Bold unembedded; add variant if needed).
#[derive(Clone, Copy)]
pub enum FontWeight {
    Regular,
    Medium,
    SemiBold,
}

/// App UI fonts: a `TextRaster` plus which loaded font each logical role maps to.
pub struct Fonts<'a> {
    pub raster: &'a dyn TextRaster,
    pub label: FontId,
    pub value: FontId,
    pub title: FontId,
    pub icon: FontId,
    /// Smallest weight (stats overlay Green-button hint).
    pub caption: FontId,
}

/// Cap on resident entries before a sweep runs. Comfortably above what one screen draws
/// (a few hundred distinct runs), so steady-state UI text never sweeps at all.
const TEXT_CACHE_CAP: usize = 512;

struct Entry {
    pixmap: Pixmap,
    /// Drawn since the last sweep — its second chance (see [`TextCache::sweep`]).
    used: bool,
}

/// Caches rasterized-text `Pixmap`s across frames, keyed by the exact
/// `(text, color, font)` that produced them. Without this, [`Canvas::text`] re-rasterized
/// (freetype glyph lookup + blend + premultiply) on *every* call — and every draw
/// function in this module is called on every render tick (the pre-stream UI loop
/// runs at ~60fps), so a static label like "Settings" paid that cost 60 times a
/// second for pixels that never changed.
///
/// Most of what this app draws is bounded by its own content (a handful of static labels,
/// a bounded set of settings values, one row per known host/game) and stays resident for
/// free. Some of it is not — a speed-test status line, a pairing status, a log tail — so a
/// [capacity](TEXT_CACHE_CAP) bounds the map: on reaching it, a second-chance sweep drops
/// every entry not drawn since the previous sweep. Steady-state text is drawn every frame
/// and so is never the entry that goes.
/// Hasher for a map whose keys are already hashes.
///
/// [`TextCache::key`] hands the map a `cache::version` output; running `SipHash` over that is
/// a second hash of a hash, once per text draw. `write_u64` is the only method a `u64` key
/// reaches, and it takes the value as it stands.
#[derive(Default)]
struct PassThroughHasher(u64);

impl std::hash::Hasher for PassThroughHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }

    fn write(&mut self, bytes: &[u8]) {
        // Not reached with a `u64` key; folded rather than ignored so a future key type
        // cannot silently collapse every entry onto one bucket.
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }
}

type KeyedEntries = HashMap<u64, Entry, std::hash::BuildHasherDefault<PassThroughHasher>>;

pub struct TextCache {
    entries: KeyedEntries,
    cap: usize,
}

impl TextCache {
    pub fn new() -> Self {
        Self::with_capacity(TEXT_CACHE_CAP)
    }

    /// [`new`](Self::new) with an explicit cap — for a cache whose content is known to be
    /// dynamic (the log overlay's tail), which wants a tighter bound than the menu's.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: KeyedEntries::default(),
            cap: cap.max(1),
        }
    }

    /// Resident entries. Read back by the frame report — this cache is the one that grows
    /// with what the app has *said*, not with what it shows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Hashes `(font, text, color)` into the key the cache stores under.
    ///
    /// Keying by the hash rather than by the tuple is what keeps the *hit* path free of both
    /// an allocation and a second hash: the old key owned a `String`, so every lookup —
    /// including the overwhelmingly common one that finds an already-rasterized glyph run —
    /// copied the label before it could ask whether it was there. A collision would show a
    /// mislabelled tile, which is worth stating plainly; over the few hundred distinct strings
    /// a screen draws, in a 64-bit space, it is not a risk this app will meet.
    fn key(font: FontId, text: &str, color: Color) -> u64 {
        let packed_color = u32::from_be_bytes([color.r, color.g, color.b, color.a]);
        crate::ui::cache::identity(&(text, packed_color, font))
    }

    /// Returns the cached `Pixmap` for `(font, text, color)`, rasterizing (and
    /// caching) it first if this is the first time this exact combination has
    /// been drawn.
    fn get_or_create(&mut self, raster: &dyn TextRaster, font: FontId, text: &str, color: Color) -> Result<&Pixmap> {
        let key = Self::key(font, text, color);
        if self.entries.contains_key(&key) {
            let entry = self.entries.get_mut(&key).expect("just probed");
            entry.used = true;
            return Ok(&entry.pixmap);
        }
        if self.entries.len() >= self.cap {
            self.sweep();
        }
        let pixmap = raster.rasterize(font, text, color)?;
        Ok(&self.entries.entry(key).or_insert(Entry { pixmap, used: true }).pixmap)
    }

    /// Second chance: keep what has been drawn since the last sweep, clear its mark, drop
    /// the rest. No recency ordering to maintain — a clock sweep is enough here, because
    /// the entries worth keeping are re-drawn every single frame.
    fn sweep(&mut self) {
        let before = self.entries.len();
        self.entries.retain(|_, e| std::mem::take(&mut e.used));
        tracing::debug!("text cache swept: {before} -> {}", self.entries.len());
    }
}

impl Default for TextCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas<'_, '_> {
    /// Renders one line of text left-aligned at `(x, y)` (top-left), returning its
    /// width. The glyph cache (see [`TextCache`]) makes repeat calls with the same
    /// `(font, text, color)` — the common case, since most on-screen text is static
    /// from one frame to the next — cheap: no re-rasterization, no re-premultiplying.
    pub fn text(&mut self, font: FontId, s: &str, x: i32, y: i32, color: Color) -> Result<u32> {
        if s.is_empty() {
            return Ok(0);
        }
        let pixmap = self.text_cache.get_or_create(self.fonts.raster, font, s, color)?;
        let width = pixmap.width();
        self.painter.draw_pixmap(x, y, pixmap);
        Ok(width)
    }

    /// [`text`](Self::text), except an overlong line is cut at `max_w` and faded rather than
    /// ellipsized — see
    /// [`Painter::draw_pixmap_faded`](crate::ui::painter::Painter::draw_pixmap_faded).
    pub fn text_faded(&mut self, font: FontId, s: &str, x: i32, y: i32, max_w: u32, color: Color) -> Result<u32> {
        if s.is_empty() || max_w == 0 {
            return Ok(0);
        }
        let pixmap = self.text_cache.get_or_create(self.fonts.raster, font, s, color)?;
        let width = pixmap.width().min(max_w);
        self.painter.draw_pixmap_faded(x, y, pixmap, max_w);
        Ok(width)
    }

    /// Renders one line of text WITHOUT touching [`TextCache`] — for text that is
    /// unique per line and scrolled past once, where caching is pure loss.
    ///
    /// [`TextCache`] is deliberately unbounded (see its docs: entry count is bounded by
    /// the app's own content — a handful of labels, one row per host/game). The About
    /// screen's licence wall breaks that assumption badly: `THIRD-PARTY-NOTICES.txt` is
    /// ~10,000 distinct lines, so scrolling the whole document through a cached
    /// [`text`](Self::text) would leave ~10,000 rasterized `Pixmap`s resident for the rest
    /// of the process — on a TV with no eviction path. These lines are drawn at most a
    /// couple of times each (once per scroll position that shows them), so rasterizing
    /// fresh is both cheaper overall and bounded in memory.
    pub fn text_uncached(&mut self, font: FontId, s: &str, x: i32, y: i32, color: Color) -> Result<u32> {
        if s.is_empty() {
            return Ok(0);
        }
        let pixmap = self.fonts.raster.rasterize(font, s, color)?;
        let width = pixmap.width();
        self.painter.draw_pixmap(x, y, &pixmap);
        Ok(width)
    }

    /// One line centred horizontally within `within`, at `y`. The `x + (w - measure) / 2`
    /// every centred label was spelling out for itself.
    pub fn text_centered(&mut self, font: FontId, s: &str, within: Rect, y: i32, color: Color) -> Result<u32> {
        let w = self.fonts.raster.measure(font, s).0 as i32;
        self.text(font, s, within.x() + (within.width() as i32 - w) / 2, y, color)
    }

    /// Draws one icon glyph (one of the `ICON_*` constants) from the bundled icon
    /// font, scaled to fill `rect` — the same [`TextCache`] that caches on-screen text
    /// caches these too (a font id plus the glyph string is already a unique, stable
    /// cache key, so a second cache wasn't needed just because this one holds icons
    /// instead of words).
    pub fn icon(&mut self, rect: Rect, glyph: &str, color: Color) -> Result<()> {
        let icon_font = self.fonts.icon;
        self.icon_in(icon_font, rect, glyph, color)
    }

    /// [`icon`](Self::icon) with an explicit font, for the tile builders that carry a
    /// glyph font other than `fonts.icon`.
    pub fn icon_in(&mut self, icon_font: FontId, rect: Rect, glyph: &str, color: Color) -> Result<()> {
        let pixmap = self
            .text_cache
            .get_or_create(self.fonts.raster, icon_font, glyph, color)?;
        self.painter.draw_pixmap_scaled(rect, pixmap);
        Ok(())
    }

    /// Draws `text` word-wrapped to `max_w` (see [`wrap_text`]), one line per
    /// `raster.height(font) + line_gap`, starting at `(x, y)`. Returns the y position just
    /// past the last line, so callers can stack more content beneath it without having to
    /// guess how many lines it wrapped to.
    #[allow(clippy::too_many_arguments)]
    pub fn text_wrapped(
        &mut self,
        font: FontId,
        s: &str,
        x: i32,
        y: i32,
        max_w: u32,
        color: Color,
        line_gap: i32,
    ) -> Result<i32> {
        let mut cursor_y = y;
        for line in wrap_text(self.fonts.raster, font, s, max_w) {
            self.text(font, &line, x, cursor_y, color)?;
            cursor_y += self.fonts.raster.height(font) + line_gap;
        }
        Ok(cursor_y)
    }

    /// The title + wrapped subtitle every Pairing/Add-host/Wake/Forget-host modal draws
    /// before its own content, on top of `Painter::modal_card`'s chrome — pulled out once
    /// these four had each grown (then separately re-fixed) the same bug: a subtitle
    /// positioned a further fixed pixel gap below the title, and drawn as a single
    /// unwrapped line, which undersized badly at this app's real TV font scale and let long
    /// content run past the card edge. Settings has no subtitle (a divider instead) and
    /// doesn't call this. Returns the y just past the wrapped subtitle, for the caller's own
    /// content below it. Always drawn in the label/value font pair.
    pub fn modal_header(
        &mut self,
        card: Rect,
        title: &str,
        title_color: Color,
        subtitle: &str,
        subtitle_color: Color,
    ) -> Result<i32> {
        let (label, value) = (self.fonts.label, self.fonts.value);
        let (text_x, subtitle_y, max_w) = modal_header_geometry(self.fonts.raster, label, card);
        self.text(label, title, text_x, card.y() + 28, title_color)?;
        self.text_wrapped(
            value,
            subtitle,
            text_x,
            subtitle_y,
            max_w,
            subtitle_color,
            MODAL_SUBTITLE_LINE_GAP,
        )
    }
}

/// Largest loaded font whose longest word in `text` fits `max_w` (smallest if none does).
/// For wrapped blocks: wrapping never breaks inside a word, so one word wider than the box
/// overflows at any fixed size.
pub fn fitting_font(raster: &dyn TextRaster, text: &str, max_w: u32) -> FontId {
    // Descending point size — see `SdlTextRaster::new`.
    const LADDER: [FontId; 4] = [FontId::Title, FontId::Label, FontId::Value, FontId::Caption];
    let longest_word = |f| {
        text.split_whitespace()
            .map(|w| raster.measure(f, w).0)
            .max()
            .unwrap_or(0)
    };
    LADDER
        .into_iter()
        .find(|&f| longest_word(f) <= max_w)
        .unwrap_or(FontId::Caption)
}

/// Greedily word-wraps `text` into lines no wider than `max_w` px in `font` — for modal
/// copy that's a full sentence or two (status/explanation text), unlike
/// [`Canvas::text_faded`]'s single-line budget for card titles and row labels.
///
/// Tracks the running line width instead of re-measuring the whole (growing) line on
/// every word — the original did a full-prefix measure each time, which is O(line
/// length) per word and so O(n²) over a line, cheap for a sentence but the dominant
/// cost of wrapping About's ~10,000-line document on open (see `ui::wrap_document`).
/// Assumes negligible word-to-word kerning at the space boundary, same as every other
/// width-budget calculation in this UI already does.
pub fn wrap_text(raster: &dyn TextRaster, font: FontId, text: &str, max_w: u32) -> Vec<String> {
    let space_w = raster.measure(font, " ").0;
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0u32;
    for word in text.split_whitespace() {
        let word_w = raster.measure(font, word).0;
        let candidate_w = if current.is_empty() {
            word_w
        } else {
            current_w + space_w + word_w
        };
        if current.is_empty() || candidate_w <= max_w {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            current_w = candidate_w;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_w = word_w;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Line spacing within a modal header's wrapped subtitle.
pub const MODAL_SUBTITLE_LINE_GAP: i32 = 6;

/// The pure geometry [`Canvas::modal_header`] and [`modal_header_end_y`] share:
/// `(text_x, subtitle_y, max_w)` — the one place it's computed, so the two
/// can never drift apart.
pub fn modal_header_geometry(raster: &dyn TextRaster, title_font: FontId, card: Rect) -> (i32, i32, u32) {
    let text_x = card.x() + 32;
    let title_y = card.y() + 28;
    let subtitle_y = title_y + raster.height(title_font) + 18;
    let max_w = card.width().saturating_sub(64);
    (text_x, subtitle_y, max_w)
}

/// The same `y` [`Canvas::modal_header`] would return, computed without drawing —
/// for positioning content below it (e.g. Pairing's PIN row) from `app::App`'s
/// `prepare_tiles`/`draw_list`, which need that position but must not
/// re-render the header just to get it.
pub fn modal_header_end_y(fonts: &Fonts, card: Rect, subtitle: &str) -> i32 {
    let (_, subtitle_y, max_w) = modal_header_geometry(fonts.raster, fonts.label, card);
    let lines = wrap_text(fonts.raster, fonts.value, subtitle, max_w).len() as i32;
    subtitle_y + lines * (fonts.raster.height(fonts.value) + MODAL_SUBTITLE_LINE_GAP)
}
