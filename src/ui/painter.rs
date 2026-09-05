//! Anti-aliased software rendering backend (`tiny_skia` Pixmap framebuffer).
use crate::ui::render::{Color, DrawList, Rect, TileId};
use std::cell::RefCell;
use std::collections::HashMap;
use tiny_skia::{
    Color as SkColor, FillRule, FilterQuality, IntSize, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

pub fn sk_color(c: Color) -> SkColor {
    SkColor::from_rgba8(c.r, c.g, c.b, c.a)
}

/// Builds a premultiplied pixmap from straight-alpha RGBA8 pixels.
pub fn rgba_pixmap(width: u32, height: u32, mut pixels: Vec<u8>) -> Option<Pixmap> {
    premultiply_rgba(&mut pixels);
    Pixmap::from_vec(pixels, IntSize::from_wh(width, height)?)
}

/// Flat-color paint (no gradients/patterns). Anti-aliasing off for cheaper scan-conversion (~15-25% faster).
pub fn solid_paint(color: Color) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(sk_color(color));
    paint.anti_alias = false;
    paint
}

/// [`solid_paint`] with anti-aliasing on, for curved or diagonal edges. Affordable where
/// `solid_paint` isn't because AA costs per edge-pixel, not per covered pixel: a corner arc
/// pays for a handful, a full-screen fill for all of them.
fn aa_paint(color: Color) -> Paint<'static> {
    let mut paint = solid_paint(color);
    paint.anti_alias = true;
    paint
}

/// [`rounded_rect_path`] over a `Rect` — `None` for an empty one, so callers need no
/// size guard of their own. The rect must already be painter-local (see `Painter::off`).
fn rect_path(rect: Rect, radius: i32) -> Option<tiny_skia::Path> {
    rounded_rect_path(
        rect.x() as f32,
        rect.y() as f32,
        rect.width() as f32,
        rect.height() as f32,
        radius as f32,
    )
}

/// The radius [`rounded_rect_path`] will actually draw with: a caller's request clamped to
/// what the box can hold. Below half a pixel it draws a plain rect instead, so this doubles
/// as the test for whether a shape has corner arcs worth anti-aliasing.
fn effective_radius(w: f32, h: f32, radius: f32) -> f32 {
    let r = radius.max(0.0).min(w / 2.0).min(h / 2.0);
    if r < 0.5 {
        0.0
    } else {
        r
    }
}

/// Rounded-rect as Bezier path (`tiny_skia` has no built-in); falls back to plain rect if radius ~0.
pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    const K: f32 = 0.552_284_7;

    let r = effective_radius(w, h, radius);
    let mut pb = PathBuilder::new();
    if r == 0.0 {
        pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let k = K * r;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// One frame's whole-screen framebuffer. `App::render` draws every screen into a
/// single `Painter`; `main.rs` uploads the result to one SDL2 texture and presents
/// it, rather than issuing a texture copy per widget as the old canvas-based
/// version did.
#[derive(Clone)]
pub struct Painter {
    pixmap: Pixmap,
    /// Drawing offset: every coordinate a caller passes is shifted by `-origin`
    /// before it hits the buffer. Lets a painter whose buffer covers only a
    /// sub-region of the screen still be fed absolute, screen-space geometry — the
    /// modal tile uses this so it can be sized to just the card's bounding box
    /// (a fraction of the full-screen raster + GPU upload it used to be) while the
    /// per-screen render fns keep computing centered, absolute card rects. `(0, 0)`
    /// for every full-screen painter, so their behaviour is unchanged.
    origin: (i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShadowKey {
    w: u32,
    h: u32,
    radius: i32,
    blur_bits: u32,
    opacity: u8,
}

thread_local! {
    /// Rendered (padded, box-blurred) shadow shapes, keyed by the params that fully
    /// determine their pixels — shared process-wide, not a `Painter` field: every
    /// grid card gets its own fresh `Painter` (`render_card_tile` calls
    /// `Painter::new` per card), so a cache on `Painter` itself would never hit past
    /// the first card of a build. `thread_local` (not a plain `static`) is safe here
    /// without `unsafe`/atomics since every `Painter` is built on the single
    /// SDL/render thread.
    static SHADOW_CACHE: RefCell<HashMap<ShadowKey, Pixmap>> = RefCell::new(HashMap::new());
}

impl Painter {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixmap: Pixmap::new(width.max(1), height.max(1)).expect("nonzero framebuffer size"),
            origin: (0, 0),
        }
    }

    fn off(&self, rect: Rect) -> Rect {
        rect.offset(-self.origin.0, -self.origin.1)
    }

    /// Raw premultiplied RGBA8 bytes, row-major, `width() * height() * 4` long —
    /// the exact byte order `sdl2::pixels::PixelFormatEnum::RGBA32` expects, so
    /// `main.rs` can upload it to an SDL2 texture with no further conversion (every
    /// frame starts with an opaque `clear`, so alpha is 255 everywhere by the time
    /// this is read — premultiplied and straight are then identical).
    pub fn data(&self) -> &[u8] {
        self.pixmap.data()
    }

    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: i32, color: Color) {
        let Some(path) = rect_path(self.off(rect), radius) else {
            return;
        };
        let curved = effective_radius(rect.width() as f32, rect.height() as f32, radius as f32) > 0.0;
        self.fill_with(&path, &if curved { aa_paint(color) } else { solid_paint(color) });
    }

    /// Always anti-aliased, even for a square rect: a fractional-width stroke straddles the
    /// pixel grid on every edge, so hard scan-conversion renders it at uneven widths.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: i32, color: Color, width: f32) {
        let Some(path) = rect_path(self.off(rect), radius) else {
            return;
        };
        let paint = aa_paint(color);
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        self.pixmap
            .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    fn fill_with(&mut self, path: &tiny_skia::Path, paint: &Paint<'_>) {
        self.pixmap
            .fill_path(path, paint, FillRule::Winding, Transform::identity(), None);
    }

    /// A soft, real (box-blurred) drop shadow for a rounded-rect shape, offset by
    /// `(dx, dy)` — replaces the old flat single-offset hard-edged rect, which had
    /// no actual softness to sell "shadow" at TV viewing distance.
    ///
    /// The blurred shape only depends on `(rect.width(), rect.height(), radius,
    /// blur, opacity)`, not position — every card of the same size/style (the
    /// whole game grid, every sidebar row) reuses one cached shape instead of
    /// re-running the box blur per card per frame.
    pub fn fill_shadow(&mut self, rect: Rect, radius: i32, dx: f32, dy: f32, blur: f32, opacity: u8) {
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let pad = shadow_pad(blur);
        let (ox, oy) = self.origin;
        let key = ShadowKey {
            w: rect.width(),
            h: rect.height(),
            radius,
            blur_bits: blur.to_bits(),
            opacity,
        };
        SHADOW_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let shape = match cache.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let Some(shape) = render_shadow_shape(rect.width(), rect.height(), radius, pad, blur, opacity)
                    else {
                        return;
                    };
                    e.insert(shape)
                }
            };
            blit_src_over(
                &mut self.pixmap,
                rect.x() - pad + dx.round() as i32 - ox,
                rect.y() - pad + dy.round() as i32 - oy,
                shape,
            );
        });
    }

    /// [`draw_pixmap`](Self::draw_pixmap) cut to `max_w`, its last [`FADE_EDGE_W`] px ramped
    /// out — how an overlong label says "this continues" without spending width on an
    /// ellipsis. A `src` that already fits is drawn untouched.
    ///
    /// The ramp is [`fade_step`]'s, the same one the vertical scroll-edge fade runs on — this
    /// one modulates the buffer where that one fills it, and runs along x rather than y, but
    /// the curve and its easing are shared so the two read as one effect.
    pub fn draw_pixmap_faded(&mut self, x: i32, y: i32, src: &Pixmap, max_w: u32) {
        if src.width() <= max_w {
            self.draw_pixmap(x, y, src);
            return;
        }
        let Some(mut cut) = Pixmap::new(max_w, src.height()) else {
            return;
        };
        // Identity blit into a narrower buffer: the crop is the clip. `cut` is freshly
        // zeroed, so source-over is the same copy a shader-path blit would make.
        blit_src_over(&mut cut, 0, 0, src);
        let w = max_w as usize;
        let fade = (FADE_EDGE_W as usize).min(w);
        // Per column, not per pixel: the ramp doesn't vary down the line.
        // Reversed: the ramp is dense where the label still reads and clear at the cut.
        let ramp: Vec<u16> = (0..fade).map(|i| u16::from(fade_step(fade - 1 - i, fade))).collect();
        for row in cut.data_mut().chunks_exact_mut(w * 4) {
            for (pixel, &k) in row[(w - fade) * 4..].chunks_exact_mut(4).zip(&ramp) {
                for b in pixel {
                    *b = ((u16::from(*b) * k + 127) / 255) as u8;
                }
            }
        }
        self.draw_pixmap(x, y, &cut);
    }

    pub fn draw_pixmap(&mut self, x: i32, y: i32, src: &Pixmap) {
        blit_src_over(&mut self.pixmap, x - self.origin.0, y - self.origin.1, src);
    }

    /// Composites `src` scaled to exactly fill `dst` — only ever at tile-build time
    /// (glyph scaling in `ui::text`, and cover art via
    /// [`draw_pixmap_rounded`](Self::draw_pixmap_rounded)), not per frame: the result is
    /// cached into the tile and only re-scaled when the source or size changes. `Bilinear`
    /// (rather than `Nearest`) is worth its modest per-call cost here since it's paid
    /// once per card build, not every frame — plain `Nearest` scaling left visible
    /// jaggies on art whose source resolution didn't cleanly divide into the card size.
    pub fn draw_pixmap_scaled(&mut self, dst: Rect, src: &Pixmap) {
        let dst = self.off(dst);
        let (dw, dh) = (dst.width() as f32, dst.height() as f32);
        let (sw, sh) = (src.width() as f32, src.height() as f32);
        if dw <= 0.0 || dh <= 0.0 || sw <= 0.0 || sh <= 0.0 {
            return;
        }
        let transform = Transform::from_scale(dw / sw, dh / sh).post_translate(dst.x() as f32, dst.y() as f32);
        let paint = PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        };
        self.pixmap.draw_pixmap(0, 0, src.as_ref(), &paint, transform, None);
    }
}

/// Width of [`Painter::draw_pixmap_faded`]'s ramp: wide enough not to read as a hard cut,
/// narrow enough to eat no more of the line than an ellipsis would.
pub const FADE_EDGE_W: u32 = 28;

/// Step `i` of `len` along the app's one edge-fade ramp, as an alpha in `0..=255` rising from
/// clear to full.
///
/// Both fades run on it: [`Painter::draw_pixmap_faded`], which dissolves the tail of a label
/// too wide for its row, and `app::render::compose`'s `push_faded`, which dissolves a
/// partially scrolled row into the top or bottom of a modal's viewport. They differ only in
/// axis and in what they modulate; sharing the curve is what makes them look like the same
/// effect turned ninety degrees.
///
/// Smoothstep rather than a straight line or a square. Linear puts visible tint across the
/// whole band; squaring (the first attempt) held so much of the band near clear that the fade
/// barely read at all. Smoothstep is symmetric — half-strength at the midpoint — with both
/// ends easing out, so neither edge shows a seam.
pub fn fade_step(i: usize, len: usize) -> u8 {
    let last = len.saturating_sub(1).max(1) as f32;
    let eased = crate::ui::animation::smoothstep((i as f32 / last).clamp(0.0, 1.0));
    (eased * 255.0).round().clamp(0.0, 255.0) as u8
}

/// How far a shadow's blur extends past the shape casting it, in px — a fixed
/// constant (not derived from anything) picked to read as a soft TV-scale shadow.
pub const SHADOW_BLUR: f32 = 14.0;

/// The alpha [`Painter::card_shadow`] casts its shadow at.
pub const SHADOW_OPACITY: u8 = 0x60;

/// Where [`Painter::card_shadow`]'s shadow falls relative to the shape casting it.
pub const SHADOW_OFFSET: (i32, i32) = (3, 5);

/// The inset a blurred shape is drawn at inside its own pixmap, so the blur has room to
/// spread on every side. Every shadow's geometry — the cached shape, where it blits, and
/// [`crate::ui::widgets::modal_shadow_rect`]'s atlas rect — is measured from this one rule.
pub fn shadow_pad(blur: f32) -> i32 {
    blur.ceil().max(0.0) as i32 + 1
}

/// Slack past the blur's exact reach, so a nine-slice's slice boundary sits in provably flat
/// pixels rather than on the last one that still varies. A pixel of error here would tile as a
/// seam down the middle of every panel; four pixels of atlas is the cheapest insurance.
const SHADOW_SLICE_MARGIN: i32 = 4;

/// Side of the corner slice in [`shadow_atlas`]: everything whose alpha still varies — the pad
/// the blur spreads into, the corner's own radius, and the blur's reach past it (three box
/// passes of `SHADOW_BLUR / 2`). Past this the shadow is flat in both axes, which is what
/// makes the nine-slice exact rather than an approximation of the blit it replaces.
pub fn shadow_slice(radius: i32) -> u32 {
    (shadow_pad(SHADOW_BLUR) + radius + 3 * (SHADOW_BLUR as i32 / 2) + SHADOW_SLICE_MARGIN) as u32
}

/// Side of the square atlas [`shadow_atlas`] builds: two corner slices either side of the two
/// flat pixels that stretch across the middle.
pub fn shadow_atlas_side(radius: i32) -> u32 {
    2 * shadow_slice(radius) + 2
}

/// The rect [`shadow_atlas`] is stretched over for `rect`: the surface, moved by
/// [`SHADOW_OFFSET`] and grown by the pad its blur needs on every side.
pub fn shadow_rect(rect: Rect) -> Rect {
    let (dx, dy) = SHADOW_OFFSET;
    let pad = shadow_pad(SHADOW_BLUR);
    Rect::new(
        rect.x() + dx - pad,
        rect.y() + dy - pad,
        rect.width() + 2 * pad as u32,
        rect.height() + 2 * pad as u32,
    )
}

/// Composites a panel shadow from the shared nine-slice atlas.
pub fn push_shadow(cmds: &mut DrawList, tile: TileId, radius: i32, panel: Rect, alpha: u8) {
    crate::ui::render::push_nine_slice(
        cmds,
        tile,
        shadow_atlas_side(radius),
        shadow_slice(radius),
        shadow_rect(panel),
        alpha,
    );
}

/// Rasterizes a `(w, h)` rounded-rect shape into a small padded alpha buffer and
/// box-blurs it (3 passes — a cheap approximation of a Gaussian blur, good enough
/// at TV viewing distance for a drop shadow), returning the standalone shadow
/// shape as a black, premultiplied `Pixmap` ready to be composited at any
/// position — see `Painter::fill_shadow`'s cache, keyed on everything that
/// determines these pixels (size/radius/blur/opacity, not position).
pub fn render_shadow_shape(w: u32, h: u32, radius: i32, pad: i32, blur: f32, opacity: u8) -> Option<Pixmap> {
    let (pw, ph) = (w as i32 + 2 * pad, h as i32 + 2 * pad);
    if pw <= 0 || ph <= 0 {
        return None;
    }
    let mut shape = Pixmap::new(pw as u32, ph as u32)?;
    let path = rounded_rect_path(pad as f32, pad as f32, w as f32, h as f32, radius as f32)?;
    let paint = solid_paint(Color::RGBA(0, 0, 0, opacity));
    shape.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

    // tiny-skia stores premultiplied RGBA; a pure-black shape's R/G/B channels are
    // always 0, so its alpha channel alone fully describes the shape — blur that
    // channel directly rather than blurring all 4 for no visual difference.
    let mut alpha: Vec<u8> = shape.data().iter().skip(3).step_by(4).copied().collect();
    let radius_px = (blur / 2.0).round().max(1.0) as usize;
    let (pwu, phu) = (pw as usize, ph as usize);
    let mut tmp = vec![0u8; alpha.len()];
    let mut prefix = vec![0u32; pwu.max(phu) + 1];
    for _ in 0..3 {
        box_blur(&mut alpha, &mut tmp, &mut prefix, pwu, phu, radius_px);
    }
    for (i, a) in alpha.into_iter().enumerate() {
        shape.data_mut()[i * 4 + 3] = a; // R/G/B stay 0 (premultiplied black)
    }

    Some(shape)
}

/// Separable box blur (horizontal pass into `tmp`, then vertical back into
/// `pixels`) — both passes are the same 1D sliding-window average, just walking
/// the buffer in a different direction (see [`blur_line`]).
///
/// Both scratch buffers belong to the caller, so a three-pass blur allocates once rather
/// than once per pass: `tmp` sized like `pixels`, `prefix` at least `w.max(h) + 1`.
pub fn box_blur(pixels: &mut [u8], tmp: &mut [u8], prefix: &mut [u32], w: usize, h: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    for y in 0..h {
        blur_line(prefix, w, radius, |x| pixels[y * w + x], |x, v| tmp[y * w + x] = v);
    }
    for x in 0..w {
        blur_line(prefix, h, radius, |y| tmp[y * w + x], |y, v| pixels[y * w + x] = v);
    }
}

/// A 1D sliding-window average over `len` samples (read/written through the given
/// accessors, so the same core serves both a blur's horizontal and vertical passes), via a
/// prefix sum so each output sample is O(1) regardless of `radius`.
///
/// The prefix buffer belongs to the caller so that a blur pass allocates once rather than
/// once per line — a card-sized shadow runs this ~2,200 times per pass, and the allocation
/// was most of what a line cost. It must hold at least `len + 1` entries; anything past that
/// is left alone, which is what lets one buffer serve both a pass's rows and its columns.
fn blur_line(
    prefix: &mut [u32],
    len: usize,
    radius: usize,
    read: impl Fn(usize) -> u8,
    mut write: impl FnMut(usize, u8),
) {
    if len == 0 {
        return;
    }
    prefix[0] = 0;
    for i in 0..len {
        prefix[i + 1] = prefix[i] + u32::from(read(i));
    }
    for i in 0..len {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius).min(len - 1);
        let count = (hi - lo + 1) as u32;
        write(i, ((prefix[hi + 1] - prefix[lo]) / count) as u8);
    }
}

/// Composites `src` over `dst` at `(x, y)`, one row at a time, clipped to `dst`.
///
/// This is what every glyph, icon and cached text run in the app is drawn with, so it is worth
/// not going through a shader for. `Pixmap::draw_pixmap` at an identity transform still sets up
/// `tiny_skia`'s pattern pipeline and samples per pixel, which measured ~5.6 megapixels a
/// second on this hardware — the About document's licence lines cost 5.2ms *each* to blit, and
/// a modal's card-sized shadow cost 205ms, both of them straight copies. Both operands are
/// premultiplied, so source-over is `src + dst * (1 - src_a)` per channel and needs no shader
/// at all.
///
/// Blends in integer arithmetic where `tiny_skia` uses floats, so a blended channel can land a
/// single step off what the pipeline produced — see the tests, which hold it to that.
fn blit_src_over(dst: &mut Pixmap, x: i32, y: i32, src: &Pixmap) {
    let (dst_w, dst_h) = (dst.width() as i32, dst.height() as i32);
    let (src_w, src_h) = (src.width() as i32, src.height() as i32);
    // The overlap, in destination space: a run placed past an edge draws the part that lands.
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + src_w).min(dst_w), (y + src_h).min(dst_h));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let cols = (x1 - x0) as usize;
    let (dst_stride, src_stride) = (dst.width() as usize, src.width() as usize);
    let src_data = src.data();
    let dst_data = dst.data_mut();
    for row in y0..y1 {
        let src_start = ((row - y) as usize * src_stride + (x0 - x) as usize) * 4;
        let dst_start = (row as usize * dst_stride + x0 as usize) * 4;
        let src_row = &src_data[src_start..src_start + cols * 4];
        let dst_row = &mut dst_data[dst_start..dst_start + cols * 4];
        for (s, d) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
            // Fixed-size views of an exact chunk (a no-op at runtime): every index below is
            // then in range at compile time, so the four blends unroll without bounds checks.
            let s: &[u8; 4] = s.try_into().expect("chunks_exact(4)");
            let d: &mut [u8; 4] = d.try_into().expect("chunks_exact_mut(4)");
            match s[3] {
                // Glyph runs and shadows are mostly one or the other, and skipping the clear
                // pixels is what makes text cheap.
                0 => {}
                255 => d.copy_from_slice(s),
                alpha => {
                    let inv = 255 - u32::from(alpha);
                    for channel in 0..4 {
                        let scaled = u32::from(d[channel]) * inv;
                        // `(v + 128 + ((v + 128) >> 8)) >> 8` — the usual rounding division by
                        // 255, exact for every value a channel can hold.
                        let rounded = (scaled + 128 + ((scaled + 128) >> 8)) >> 8;
                        d[channel] = s[channel].saturating_add(rounded as u8);
                    }
                }
            }
        }
    }
}

/// `tiny-skia` stores premultiplied alpha; `SDL2_ttf`'s `.blended()` glyph surfaces
/// and `image`'s decoded covers are both straight alpha — every raw-RGBA buffer
/// feeding a `Pixmap` (see `pixmap_from_ttf_surface`, `art.rs`) goes through this
/// first.
pub fn premultiply_rgba(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        px[0] = ((u32::from(px[0]) * a) / 255) as u8;
        px[1] = ((u32::from(px[1]) * a) / 255) as u8;
        px[2] = ((u32::from(px[2]) * a) / 255) as u8;
    }
}
