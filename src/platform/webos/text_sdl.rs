//! SDL2_ttf-backed `TextRaster` implementation — the only place in the crate
//! that touches `sdl2::ttf`.
use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{Context, Result};
use sdl2::ttf::{Font, Sdl2TtfContext};
use tiny_skia::{IntSize, Pixmap};

use crate::ui::painter::premultiply_rgba;
use crate::ui::render::Color;
use crate::ui::text::FontId;
use crate::ui::text::FontWeight;
use crate::ui::text::TextRaster;

/// The typefaces to rasterize with. Supplied by the caller: `platform` loads whatever bytes it
/// is handed and deliberately does not know which typeface backs the brand (see `app::assets`).
pub struct Typefaces {
    /// The typeface for a `ui` text weight.
    pub text: fn(FontWeight) -> &'static [u8],
    /// The icon typeface, one face at every size.
    pub icon: &'static [u8],
}

/// Load the text weight at a size proportional to display height (720px reference).
fn load_font<'ttf>(
    ttf: &'ttf Sdl2TtfContext,
    typefaces: &Typefaces,
    height_px: u32,
    design_size: u16,
    weight: FontWeight,
) -> Result<Font<'ttf, 'static>> {
    let bytes = (typefaces.text)(weight);
    let scaled = (u32::from(design_size) * height_px / 720).max(10) as u16;
    let rwops = sdl2::rwops::RWops::from_bytes(bytes).map_err(|e| anyhow::anyhow!("text font rwops: {e}"))?;
    ttf.load_font_from_rwops(rwops, scaled)
        .map_err(|e| anyhow::anyhow!("load_font: {e}"))
}

/// Loads the bundled icon font at a fixed, generously large size — icon glyphs are
/// always drawn through `Canvas::icon`, which composites (and, via `Painter`'s
/// bilinear `draw_pixmap_scaled`, downscales) the rasterized glyph to fit whatever
/// rect the caller actually wants, so a single oversized rasterization (rather than
/// one `load_icon_font` call per distinct icon size, the way the three text fonts
/// each get their own) is enough to stay crisp at every icon size this UI uses.
fn load_icon_font<'ttf>(ttf: &'ttf Sdl2TtfContext, typefaces: &Typefaces) -> Result<Font<'ttf, 'static>> {
    let rwops = sdl2::rwops::RWops::from_bytes(typefaces.icon).map_err(|e| anyhow::anyhow!("icon font rwops: {e}"))?;
    ttf.load_font_from_rwops(rwops, 128)
        .map_err(|e| anyhow::anyhow!("load_icon_font: {e}"))
}

/// Converts an `SDL2_ttf`-rendered glyph-run surface into an owned, premultiplied
/// `tiny_skia::Pixmap`. Goes through `convert_format(RGBA32)` first so the byte
/// order in memory is always R,G,B,A regardless of `SDL2_ttf`'s actual output format
/// or host endianness — the same `RGBA32` convention `main.rs`/`art.rs` already rely
/// on for raw RGBA buffers.
fn pixmap_from_ttf_surface(surface: &sdl2::surface::Surface) -> Result<Pixmap> {
    let surface = surface
        .convert_format(sdl2::pixels::PixelFormatEnum::RGBA32)
        .map_err(|e| anyhow::anyhow!("convert glyph surface: {e}"))?;
    let (w, h) = (surface.width(), surface.height());
    let pitch = surface.pitch() as usize;
    let row_bytes = w as usize * 4;
    let mut rgba = vec![0u8; row_bytes * h as usize];
    surface.with_lock(|src| {
        for y in 0..h as usize {
            let start = y * pitch;
            rgba[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(&src[start..start + row_bytes]);
        }
    });
    premultiply_rgba(&mut rgba);
    Pixmap::from_vec(rgba, IntSize::from_wh(w, h).context("zero-sized glyph surface")?).context("build glyph pixmap")
}

/// Owns the five loaded fonts (sized for a 10-foot TV viewing distance — see
/// `ui`'s `ROW_H`/`ROW_MAX_W` docs) and implements `TextRaster` over them.
pub struct SdlTextRaster<'ttf> {
    label: Font<'ttf, 'static>,
    value: Font<'ttf, 'static>,
    icon: Font<'ttf, 'static>,
    caption: Font<'ttf, 'static>,
    /// Memoized `measure` results, one map per font — see [`SdlTextRaster::measure`].
    measured: RefCell<[MeasureCache; FontId::COUNT]>,
}

/// One font's memoized measurements, keyed by the exact string measured.
type MeasureCache = HashMap<Box<str>, (u32, u32)>;

/// Measurements held per font before the whole memo is dropped.
///
/// The cache exists for the text this UI measures over and over (every word of every wrapped
/// string, every label a layout probes), which is a small set. The About screen's licence wall
/// is the exception — `ui::wrap_document` measures every word of a ~10,000-line document, and
/// on a TV with no eviction path an unbounded memo of that is a leak. Clearing wholesale at a
/// ceiling keeps the common case allocation-free without a per-entry eviction policy: the
/// working set of a menu screen refills in a few hundred lookups.
const MEASURE_CACHE_MAX: usize = 4096;

impl<'ttf> SdlTextRaster<'ttf> {
    pub fn new(ttf: &'ttf Sdl2TtfContext, height_px: u32, typefaces: &Typefaces) -> Result<Self> {
        Ok(Self {
            label: load_font(ttf, typefaces, height_px, 22, FontWeight::Medium)?,
            value: load_font(ttf, typefaces, height_px, 20, FontWeight::Regular)?,
            icon: load_icon_font(ttf, typefaces)?,
            caption: load_font(ttf, typefaces, height_px, 14, FontWeight::Regular)?,
            measured: RefCell::new(std::array::from_fn(|_| HashMap::new())),
        })
    }

    fn font(&self, id: FontId) -> &Font<'ttf, 'static> {
        match id {
            FontId::Label => &self.label,
            FontId::Value => &self.value,
            FontId::Icon => &self.icon,
            FontId::Caption => &self.caption,
        }
    }
}

impl TextRaster for SdlTextRaster<'_> {
    fn rasterize(&self, font: FontId, text: &str, color: Color) -> Result<Pixmap> {
        let surface = self
            .font(font)
            .render(text)
            .blended(sdl2::pixels::Color::RGBA(color.r, color.g, color.b, color.a))
            .map_err(|e| anyhow::anyhow!("render text: {e}"))?;
        pixmap_from_ttf_surface(&surface)
    }

    /// Memoized, because `size_of` is a full freetype glyph-metrics walk and this UI asks for
    /// the same measurement constantly: `wrap_text` measures every word of every string it
    /// wraps (and words repeat), every label measures itself against its width budget,
    /// `Layout::total_length` probes a stack before placing it, and all of that happens again
    /// on the next tile rebuild. None of it can change — a loaded font's metrics are fixed.
    fn measure(&self, font: FontId, text: &str) -> (u32, u32) {
        let mut memo = self.measured.borrow_mut();
        let per_font = &mut memo[font.index()];
        if let Some(&size) = per_font.get(text) {
            return size;
        }
        let size = self.font(font).size_of(text).unwrap_or((0, 0));
        if per_font.len() >= MEASURE_CACHE_MAX {
            per_font.clear();
        }
        per_font.insert(text.into(), size);
        size
    }

    fn height(&self, font: FontId) -> i32 {
        self.font(font).height()
    }
}
