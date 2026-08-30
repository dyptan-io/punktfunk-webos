//! GPU composition of the pre-stream UI (the `opengles2` SDL renderer confirmed
//! live on-device): tiny-skia rasterizes widgets into cached tiles
//! ([`crate::ui`]'s `render_*_tile` helpers — the AA/soft-shadow look is
//! unchanged), and this module owns their GPU textures and executes `App`'s
//! per-frame draw list. Position, scroll, the focus pop's scale, and fades are
//! all texture-copy parameters here — per-frame CPU rasterization cost is gone,
//! which is what makes 60fps animation feasible on this hardware (the previous
//! CPU compositor measured ~25-45ms/frame; see docs/NOTES.md).

use std::collections::HashMap;

use anyhow::Result;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{BlendMode, Canvas, ScaleMode, Texture, TextureCreator};
use sdl2::video::{Window, WindowContext};

use crate::ui::render::{Color, Corners, DrawCmd, FrostMask, FrostPane, Rect, TileId};
use crate::ui::theme::Glass;
use crate::ui::Painter;

/// Logged once per process: a renderer without custom blend modes cannot punch a hole in the
/// graphics plane, and repeating that per frame would bury the log.
static BLEND_WARNED: std::sync::Once = std::sync::Once::new();

/// The blend behind [`DrawCmd::Erase`]: `dst = dst * (1 - srcA)`, colour and alpha alike, so a
/// mask subtracts what is already drawn instead of painting over it. Composed per call — it is
/// a pure function of its six arguments, and SDL hands back a packed value rather than
/// allocating anything.
fn erase_blend() -> sdl2::sys::SDL_BlendMode {
    use sdl2::sys::{SDL_BlendFactor::*, SDL_BlendOperation::*};
    unsafe {
        sdl2::sys::SDL_ComposeCustomBlendMode(
            SDL_BLENDFACTOR_ZERO,
            SDL_BLENDFACTOR_ONE_MINUS_SRC_ALPHA,
            SDL_BLENDOPERATION_ADD,
            SDL_BLENDFACTOR_ZERO,
            SDL_BLENDFACTOR_ONE_MINUS_SRC_ALPHA,
            SDL_BLENDOPERATION_ADD,
        )
    }
}

fn to_sdl_rect(r: Rect) -> sdl2::rect::Rect {
    sdl2::rect::Rect::new(r.x(), r.y(), r.width(), r.height())
}

fn to_sdl_color(c: Color) -> sdl2::pixels::Color {
    sdl2::pixels::Color::RGBA(c.r, c.g, c.b, c.a)
}

/// How many freed textures are kept for reuse. The grid's card tiles are all one size, so a
/// scroll frees and immediately re-creates the same shape; a handful of spares covers a row
/// either way without holding VRAM the app isn't about to use again.
const TEXTURE_POOL_CAP: usize = 16;

/// A texture's dimensions and pixel format — what makes one interchangeable with another, so
/// what the pool is searched by. Cached rather than re-read: `Texture::query` is an FFI call,
/// and a pool scan would otherwise make one per candidate per acquire.
type Shape = (u32, u32, PixelFormatEnum);

/// One tile's GPU texture and everything the compositor knows about it.
///
/// One record rather than a map per attribute: a texture's premultiplied-ness and its last
/// applied mod are only meaningful for as long as *that* texture is this tile's, so they are
/// invalidated by construction whenever it is replaced or released.
struct Tile {
    texture: Texture,
    shape: Shape,
    /// Whether the texture holds premultiplied pixels, so `present` knows to scale its colour
    /// alongside its alpha (see [`Compositor::faded_tile`]).
    premultiplied: bool,
    /// The `(alpha, colour)` mod last applied, so a frame that doesn't move a fade issues no
    /// SDL calls at all. `None` until set — a recycled texture carries the previous tile's.
    mods: Option<(u8, u8)>,
    /// A number unique to this texture's current *contents*, stamped by every upload. What
    /// lets [`Compositor::backdrop_key`] tell "the same draw list" from "the same draw list,
    /// but one of those tiles has new pixels in it" — per tile, so a tile uploaded this frame
    /// only invalidates the frost capture when the capture actually referenced it.
    gen: u64,
}

pub struct Compositor {
    tiles: HashMap<TileId, Tile>,
    /// Textures freed by [`Compositor::drop_tile`], kept to be handed straight back out at the
    /// same shape. Scrolling the grid otherwise runs a `glDeleteTextures` plus a `glGenTextures`
    /// and a fresh storage allocation per card per row, for an object the next row wants again.
    pool: Vec<(Shape, Texture)>,
    /// Reused staging buffer for the premultiplied → straight-alpha conversion, on the
    /// fallback path only (see [`premultiplied_blend`]).
    staging: Vec<u8>,
    /// The composed premultiplied blend mode, probed once — `None` until probed, `Some(None)`
    /// if the renderer refused it.
    premultiplied: Option<Option<sdl2::sys::SDL_BlendMode>>,
    /// Everything [`DrawCmd::Frost`] needs, built the first frame one appears. `None` until
    /// then — but a focused grid card frosts its own title strip, so in the menu that is the
    /// first frame with focus, and a screen's worth of render target is allocated for the rest
    /// of the session. Only a stream that never returns to the menu avoids it.
    frost: Option<Frost>,
    /// Source of [`Tile::gen`] — bumped once per upload so no two contents ever share a
    /// number, including across a drop and a re-acquire of the same id.
    next_gen: u64,
}

/// Frosted glass, entirely in the sampler: the layers under the first frosted card are drawn
/// into [`Chain::backdrop`] instead of the default framebuffer, minified twice (each bilinear
/// copy is a box filter over the texels it merges), then magnified back under the card and
/// masked to its rounded shape.
///
/// Nothing is read back. `glReadPixels` off the default framebuffer is the obvious way to get
/// the pixels under a dialog and it stalls the pipeline for milliseconds on this chip; a CPU
/// box blur over a card-sized region is worse again (~4M read-modify-writes on a soft-float
/// core). Both were rejected for this.
struct Frost {
    /// The [`crate::ui::theme::epoch`] this was built at. A pick that moves it retires the
    /// whole thing — see [`Compositor::release_theme`].
    epoch: u64,
    /// The blur chain, or `None` on a renderer with no render targets, no [`mask_blend`], or a
    /// look with no glass at all — the frost then degrades to a flat rounded fill of the
    /// caller's fallback colour.
    chain: Option<Chain>,
    /// Alpha masks, each with the shape it was rasterized for. Keyed off the pane's *unscaled*
    /// shape, so a card's focus zoom never touches one.
    ///
    /// A handful rather than one: a single frame draws the modal card's pane and the two
    /// scroll-edge fades', at three different shapes, so one slot would rebuild and re-upload
    /// all three every frame.
    masks: Vec<(Texture, MaskKey)>,
}

/// What makes one frost mask reusable for another pane.
type MaskKey = (u32, u32, FrostMask);

/// How many masks and scratches are kept, keyed by shape. Three shapes is the busiest frame
/// there is — a modal card over a focused card's title strip, or over the taller submenu panel
/// grown out of it — and the spare absorbs a modal cross-fade on top of that. Note the
/// eviction in [`slot_for`] is FIFO, not LRU, so a fourth live shape would thrash.
///
/// The compositor's own figure, not the theme's: the mask cache also serves the flat fallback,
/// which runs on a renderer that has no [`Glass`] to ask.
const FROST_SLOTS: usize = 4;

/// Index of `key` in `slots`, building it with `build` and evicting the oldest entry when the
/// cache is full. Shared by the mask and scratch caches, which differ only in what they hold.
fn slot_for<K: PartialEq>(
    slots: &mut Vec<(Texture, K)>,
    key: K,
    build: impl FnOnce() -> Result<Texture>,
) -> Result<usize> {
    if let Some(i) = slots.iter().position(|(_, k)| *k == key) {
        return Ok(i);
    }
    let tex = build()?;
    if slots.len() == FROST_SLOTS {
        let (old, _) = slots.remove(0);
        // SAFETY: `unsafe_textures`; the evicted texture has just left the only vec holding it.
        unsafe { old.destroy() };
    }
    slots.push((tex, key));
    Ok(slots.len() - 1)
}

struct Chain {
    /// The material this chain was built for. Held rather than re-read per frame so the
    /// textures and the numbers that sized them cannot disagree — a pick that changes them
    /// drops the whole [`Frost`] instead (see [`Compositor::release_theme`]).
    glass: Glass,
    /// This frame composed up to the first frosted card — the blur's source, and what gets
    /// blitted to the screen in place of those commands.
    backdrop: Texture,
    /// `backdrop` minified once per [`FROST_STEPS`] entry, each off the one before. The last
    /// is what every frosted card samples.
    levels: Vec<Texture>,
    /// The output size these were built for.
    screen: (u32, u32),
    /// The scratches a pane's blur is magnified into and masked in, each at one shape's size.
    /// Cached in the same number as the masks, and for the same reason.
    scratches: Vec<(Texture, (u32, u32))>,
    /// One tile of etch grain, tiled over every pane's blur. This is what separates frosted
    /// glass from a plain blur: real frosting scatters light off a rough surface, and a pure
    /// minification chain is perfectly smooth. See [`grain_tile`].
    grain: Option<Texture>,
    /// [`mask_blend`], composed once and accepted by this renderer.
    mask_mode: sdl2::sys::SDL_BlendMode,
    /// What [`backdrop`](Self::backdrop) and the levels under it currently hold — see
    /// [`Compositor::backdrop_key`]. `None` on a chain that has captured nothing yet.
    backdrop_key: Option<u64>,
}

/// The blur chain's minification steps, each applied to the level before it — cumulative
/// divisors 4, 8, 16, 32 and 64, which are the blur widths a [`FrostPane`] can ask for. Steps
/// of 2 past the first rather than one big jump: a bilinear sample only ever averages the 2x2
/// texels around it, so a single large minification point-samples and aliases instead of
/// blurring. Each extra link costs one copy of a texture that is already tiny, and buys a
/// wider, smoother spread — successive 2x averages converge on a gaussian, where one deep
/// jump would not.
const FROST_STEPS: [u32; 5] = [4, 2, 2, 2, 2];

/// `dst.rgb` untouched, `dst.a *= src.a` — the rounded-rect mask, applied over an already
/// drawn blur.
///
/// Alpha only, rather than multiplying the whole texel: that leaves the scratch in *straight*
/// alpha, so the masked result composites back with the ordinary [`BlendMode::Blend`] instead
/// of depending on the premultiplied mode this renderer may have refused ([`premultiplied_blend`]).
fn mask_blend() -> sdl2::sys::SDL_BlendMode {
    use sdl2::sys::{SDL_BlendFactor, SDL_BlendOperation};
    // SAFETY: as `premultiplied_blend` — a pure value computation, no renderer state.
    unsafe {
        sdl2::sys::SDL_ComposeCustomBlendMode(
            SDL_BlendFactor::SDL_BLENDFACTOR_ZERO,
            SDL_BlendFactor::SDL_BLENDFACTOR_ONE,
            SDL_BlendOperation::SDL_BLENDOPERATION_ADD,
            SDL_BlendFactor::SDL_BLENDFACTOR_ZERO,
            SDL_BlendFactor::SDL_BLENDFACTOR_SRC_ALPHA,
            SDL_BlendOperation::SDL_BLENDOPERATION_ADD,
        )
    }
}

fn rect_key(r: Rect) -> (i32, i32, u32, u32) {
    (r.x(), r.y(), r.width(), r.height())
}

/// A render target that samples smoothly and overwrites rather than blends — every link in
/// the blur chain.
fn blur_target(creator: &TextureCreator<WindowContext>, w: u32, h: u32) -> Result<Texture> {
    let mut tex = creator
        .create_texture_target(PixelFormatEnum::RGBA32, w.max(1), h.max(1))
        .map_err(|e| anyhow::anyhow!("frost target {w}x{h}: {e}"))?;
    tex.set_scale_mode(ScaleMode::Linear);
    tex.set_blend_mode(BlendMode::None);
    Ok(tex)
}

impl Chain {
    fn new(
        creator: &TextureCreator<WindowContext>,
        (w, h): (u32, u32),
        mask_mode: sdl2::sys::SDL_BlendMode,
        glass: Glass,
    ) -> Result<Self> {
        let mut size = (w, h);
        let mut levels = Vec::with_capacity(FROST_STEPS.len());
        for step in FROST_STEPS {
            size = (size.0.div_ceil(step), size.1.div_ceil(step));
            levels.push(blur_target(creator, size.0, size.1)?);
        }
        Ok(Self {
            glass,
            backdrop: blur_target(creator, w, h)?,
            levels,
            screen: (w, h),
            scratches: Vec::new(),
            backdrop_key: None,
            // Optional: the grain is the finish, not the effect. A renderer that will not hand
            // one over still gets the blur.
            grain: grain_tile(creator, glass.grain).ok(),
            mask_mode,
        })
    }

    /// The scratch at exactly `(w, h)`, built on first use at that size.
    fn scratch_for(&mut self, creator: &TextureCreator<WindowContext>, w: u32, h: u32) -> Result<usize> {
        slot_for(&mut self.scratches, (w, h), || blur_target(creator, w, h))
    }

    /// Which chain level a pane should read: the most minified one no wider than the `blur` it
    /// asked for. Returns the level's index, its cumulative divisor and its size.
    ///
    /// The pane's request decides this and nothing else — deliberately not its size. Sizing the
    /// blur to the pane made a card's one-line title strip and the tall submenu it grows into
    /// land on different levels, so the frost visibly changed the instant the panel opened,
    /// which is the one thing a surface growing out of another must not do. A pane too short to
    /// hold the spread it asked for magnifies a texel or two into a near-flat wash, and that is
    /// the correct answer: a 64px blur across a 50px band is a flat wash.
    fn level_for(&self, blur: u32) -> (Option<usize>, u32, (u32, u32)) {
        // Seeded on the backdrop, not on level 0: a request finer than the first step samples
        // the unminified frame — no blur, the honest degradation, where naming level 0 anyway
        // would pair its texture with the *screen's* dimensions and read a src rect from far
        // outside it.
        let mut chosen = (None, 1, self.screen);
        let mut div = 1;
        let mut size = self.screen;
        for (i, step) in FROST_STEPS.iter().enumerate() {
            div *= step;
            size = (size.0.div_ceil(*step), size.1.div_ceil(*step));
            if div > blur {
                break;
            }
            chosen = (Some(i), div, size);
        }
        chosen
    }

    /// `rect`, in screen pixels, expressed in the texels of the level `div`/`(w, h)` describe
    /// and clipped to it.
    fn blurred_src(&self, rect: Rect, div: u32, size: (u32, u32)) -> Rect {
        let div = div as i32;
        texel_rect(
            rect.x() / div,
            rect.y() / div,
            (rect.right() + div - 1) / div,
            (rect.bottom() + div - 1) / div,
            size,
        )
    }

    fn textures(self) -> impl Iterator<Item = Texture> {
        std::iter::once(self.backdrop)
            .chain(self.levels)
            .chain(self.scratches.into_iter().map(|(t, _)| t))
            .chain(self.grain)
    }
}

impl Frost {
    /// Probes what this renderer can actually do, once. A refusal is not an error: the frost
    /// has a flat fallback, and a modal is still a modal without a blur behind it.
    fn probe(canvas: &Canvas<Window>, creator: &TextureCreator<WindowContext>, glass: Option<Glass>) -> Self {
        let mode = mask_blend();
        let ok = glass.is_some()
            && canvas.render_target_supported()
            && probe_blend_mode(creator.create_texture_target(PixelFormatEnum::RGBA32, 1, 1).ok(), mode);
        let chain = ok
            .then(|| canvas.output_size().ok())
            .flatten()
            .zip(glass)
            .and_then(|(size, glass)| Chain::new(creator, size, mode, glass).ok());
        tracing::info!("frosted modals: {}", chain.is_some());
        Self {
            epoch: crate::ui::theme::epoch(),
            chain,
            masks: Vec::new(),
        }
    }

    /// The chain, rebuilt if the output size moved under it. `None` where this renderer has no
    /// blur at all.
    fn chain_for(&mut self, creator: &TextureCreator<WindowContext>, size: (u32, u32)) -> Option<&mut Chain> {
        let chain = self.chain.as_mut()?;
        if chain.screen != size {
            let (mode, glass) = (chain.mask_mode, chain.glass);
            let fresh = Chain::new(creator, size, mode, glass).ok()?;
            for tex in std::mem::replace(chain, fresh).textures() {
                // SAFETY: `unsafe_textures`; the replaced chain has left the field.
                unsafe { tex.destroy() };
            }
        }
        Some(chain)
    }

    fn destroy(self) {
        // SAFETY: `unsafe_textures`; `self` is consumed, so nothing can reach these again.
        for tex in self
            .chain
            .into_iter()
            .flat_map(Chain::textures)
            .chain(self.masks.into_iter().map(|(t, _)| t))
        {
            unsafe { tex.destroy() };
        }
    }
}

/// The `(alpha, colour)` mod a draw list's per-tile fade `alpha` becomes.
///
/// A premultiplied source needs its colour scaled by the same factor as its alpha, which
/// `set_alpha_mod` alone does not do — SDL modulates the texel by the vertex colour, whose RGB
/// stays 255 when only alpha mod is set, so the source arrives as `(rgb, a * alpha)`: full
/// brightness under shrinking coverage, i.e. a modal that holds near-opaque through most of the
/// fade and then cuts. Colour mod at `alpha` too gives `(rgb * alpha, a * alpha)`.
///
/// Straight-alpha and opaque tiles must keep colour mod at 255 — their colour is independent of
/// coverage, so scaling it would darken them as they faded.
fn tile_alpha_mod(alpha: u8, premultiplied: bool) -> (u8, u8) {
    (alpha, if premultiplied { alpha } else { 255 })
}

/// Premultiplied-source blending: `dst = src + dst * (1 - src.a)`, i.e. the same result the
/// straight-alpha `BlendMode::Blend` gives *without* the source first being divided back out
/// by its own alpha.
///
/// `tiny_skia` rasterizes premultiplied, so every non-opaque tile used to be un-premultiplied on
/// the CPU before upload — three integer divides per pixel by a runtime alpha, ~6M of them for
/// a full-screen modal, on a core whose NEON unit has no integer divide at all. Declaring the
/// blend equation instead moves the whole operation into the GPU's blender, where it is free,
/// and removes the staging buffer's second full-size copy with it.
///
/// Per-tile fade alpha then has to scale source colour *and* alpha together, which alpha mod
/// alone does not do — see [`tile_alpha_mod`].
fn premultiplied_blend() -> sdl2::sys::SDL_BlendMode {
    use sdl2::sys::{SDL_BlendFactor, SDL_BlendOperation};
    // SAFETY: a pure value computation in SDL — it packs the factors into a blend-mode enum
    // and touches no renderer state.
    unsafe {
        sdl2::sys::SDL_ComposeCustomBlendMode(
            SDL_BlendFactor::SDL_BLENDFACTOR_ONE,
            SDL_BlendFactor::SDL_BLENDFACTOR_ONE_MINUS_SRC_ALPHA,
            SDL_BlendOperation::SDL_BLENDOPERATION_ADD,
            SDL_BlendFactor::SDL_BLENDFACTOR_ONE,
            SDL_BlendFactor::SDL_BLENDFACTOR_ONE_MINUS_SRC_ALPHA,
            SDL_BlendOperation::SDL_BLENDOPERATION_ADD,
        )
    }
}

/// Applies a raw SDL blend mode to `tex`, reporting whether the renderer accepted it.
///
/// The safe wrapper's `BlendMode` enum has no variant for a composed mode, so this is the one
/// place that goes through `sdl2::sys`. The GLES2 backend answers via its `SupportsBlendMode`
/// hook, so a refusal is a real "this renderer can't" rather than an error to propagate.
/// Whether this renderer accepts `mode`, asked of a throwaway 1x1 `probe` so the answer is
/// known before any real pixels are uploaded on the strength of it. The caller picks the
/// texture kind, because a static texture and a render target can be answered differently.
fn probe_blend_mode(probe: Option<Texture>, mode: sdl2::sys::SDL_BlendMode) -> bool {
    probe.is_some_and(|probe| {
        let ok = set_raw_blend_mode(&probe, mode);
        // SAFETY: `unsafe_textures` makes destruction the owner's job; this probe was never
        // stored anywhere, so this is its only destroy.
        unsafe { probe.destroy() };
        ok
    })
}

fn set_raw_blend_mode(tex: &Texture, mode: sdl2::sys::SDL_BlendMode) -> bool {
    // SAFETY: `tex.raw()` is a live texture owned by `self.tiles`, and `SDL_SetTextureBlendMode`
    // only reads the mode value.
    unsafe { sdl2::sys::SDL_SetTextureBlendMode(tex.raw(), mode) == 0 }
}

/// One channel of the fallback un-premultiply: `c * 255 / a`, rounded, as a multiply and a
/// shift. `+ 0x8000` is the half-ulp that makes it round rather than truncate.
fn unpremultiply(c: u8, recip: u32) -> u8 {
    ((u32::from(c) * recip + 0x8000) >> 16).min(255) as u8
}

/// `1/a` in 16.16 fixed point, scaled by 255 — the fallback un-premultiply's multiply-and-shift
/// in place of a divide. Index 0 is unused (the `a == 0` fast path takes it).
static RECIP_ALPHA: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut a = 1;
    while a < 256 {
        table[a] = (255 << 16) / a as u32;
        a += 1;
    }
    table
};

impl Compositor {
    pub fn new() -> Self {
        Self {
            tiles: HashMap::new(),
            pool: Vec::new(),
            staging: Vec::new(),
            premultiplied: None,
            frost: None,
            next_gen: 0,
        }
    }

    /// Installs `shape`'s texture as `tile`'s — recycled from the pool if one is waiting,
    /// created otherwise — and releases whatever it replaces. Only the GL object is reused;
    /// every caller overwrites the pixels with `update` before the tile is drawn.
    fn acquire(&mut self, creator: &TextureCreator<WindowContext>, tile: TileId, shape: Shape) -> Result<()> {
        let (w, h, format) = shape;
        let texture = match self.pool.iter().position(|(s, _)| *s == shape) {
            Some(i) => self.pool.swap_remove(i).1,
            None => creator
                .create_texture_static(format, w, h)
                .map_err(|e| anyhow::anyhow!("create texture {tile:?} {w}x{h}: {e}"))?,
        };
        let replaced = self.tiles.insert(
            tile,
            Tile {
                texture,
                shape,
                premultiplied: false,
                mods: None,
                gen: 0,
            },
        );
        if let Some(old) = replaced {
            self.release(old);
        }
        Ok(())
    }

    /// Hands a tile's texture back for reuse, destroying it once the pool is full.
    ///
    /// SAFETY (both branches): `unsafe_textures` makes destruction the owner's job. The tile
    /// has already left `tiles`, so this is its only owner and the pool never holds a texture
    /// that is still reachable as a tile.
    fn release(&mut self, tile: Tile) {
        if self.pool.len() < TEXTURE_POOL_CAP {
            self.pool.push((tile.shape, tile.texture));
        } else {
            unsafe { tile.texture.destroy() };
        }
    }

    /// The composed premultiplied blend mode if this renderer supports it, probed once against
    /// a throwaway 1×1 texture so the answer is known before any real pixels are uploaded on
    /// the strength of it. Logged, because which path a device took is the first thing worth
    /// knowing when a tile's alpha looks wrong on a model neither developer owns.
    fn premultiplied_mode(&mut self, creator: &TextureCreator<WindowContext>) -> Option<sdl2::sys::SDL_BlendMode> {
        if let Some(cached) = self.premultiplied {
            return cached;
        }
        let mode = premultiplied_blend();
        let supported = probe_blend_mode(creator.create_texture_static(PixelFormatEnum::RGBA32, 1, 1).ok(), mode);
        tracing::info!("premultiplied texture blending: {supported}");
        let resolved = supported.then_some(mode);
        self.premultiplied = Some(resolved);
        resolved
    }

    /// Uploads already-decoded pixels to a GPU texture, replacing whatever `tile` held.
    ///
    /// Replaces rather than no-ops: a cached texture used to be taken as already correct, so
    /// the one caller that reuses a tile id (the hero) had to `drop_tile` first, and any
    /// future one that forgot would get last image's pixels and no error. The texture object
    /// itself is still reused when the shape matches — same idiom as [`upload`](Self::upload).
    ///
    /// `format` is the caller's, not this module's — a straight-RGBA8 source and a 16-bit one
    /// blend identically here, since a fade comes from the texture's alpha mod rather than from
    /// its pixels. It is checked against `pixels` rather than trusted: a producer that changes
    /// its encoding without its call site following would otherwise upload at the wrong pitch,
    /// which shows up as a skewed texture and nothing else.
    pub fn upload_raw(
        &mut self,
        creator: &TextureCreator<WindowContext>,
        tile: TileId,
        w: u32,
        h: u32,
        format: PixelFormatEnum,
        pixels: &[u8],
    ) -> Result<()> {
        let pitch = w as usize * format.byte_size_per_pixel();
        let expected = pitch * h as usize;
        anyhow::ensure!(
            pixels.len() == expected,
            "upload {tile:?}: {} bytes for {w}x{h} {format:?} (want {expected})",
            pixels.len(),
        );
        let shape = (w, h, format);
        if self.tiles.get(&tile).map(|t| t.shape) != Some(shape) {
            self.acquire(creator, tile, shape)?;
        }
        self.next_gen = self.next_gen.wrapping_add(1);
        let gen = self.next_gen;
        let entry = self.tiles.get_mut(&tile).expect("acquired above or already fresh");
        entry.gen = gen;
        entry.premultiplied = false;
        entry
            .texture
            .update(None, pixels, pitch)
            .map_err(|e| anyhow::anyhow!("upload raw: {e}"))?;
        entry.texture.set_blend_mode(BlendMode::Blend);
        Ok(())
    }

    /// Creates/updates tile's texture from a rasterized painter. Opaque tiles upload directly
    /// and don't blend; the rest upload their premultiplied pixels as they are and let the
    /// blender account for it ([`premultiplied_blend`]), falling back to converting them to
    /// straight alpha on the CPU where the renderer won't take a composed blend mode.
    ///
    /// `opaque` is the caller's to declare: which tiles cover every pixel they occupy is a
    /// fact about the app's layout, and this module has no business matching on tile ids
    /// to find out.
    pub fn upload(
        &mut self,
        creator: &TextureCreator<WindowContext>,
        tile: TileId,
        pm: &Painter,
        opaque: bool,
    ) -> Result<()> {
        let (w, h) = (pm.width(), pm.height());
        let premultiplied = (!opaque).then(|| self.premultiplied_mode(creator)).flatten();
        let shape = (w, h, PixelFormatEnum::RGBA32);
        if self.tiles.get(&tile).map(|t| t.shape) != Some(shape) {
            self.acquire(creator, tile, shape)?;
        }
        self.next_gen = self.next_gen.wrapping_add(1);
        let gen = self.next_gen;
        let entry = self.tiles.get_mut(&tile).expect("acquired above or already fresh");
        entry.gen = gen;
        entry.premultiplied = premultiplied.is_some();
        let tex = &mut entry.texture;
        let pitch = w as usize * 4;
        if opaque {
            tex.update(None, pm.data(), pitch)
                .map_err(|e| anyhow::anyhow!("upload {tile:?}: {e}"))?;
            tex.set_blend_mode(BlendMode::None);
        } else if let Some(mode) = premultiplied {
            // The pixels go up exactly as tiny_skia produced them; the blender does the rest.
            tex.update(None, pm.data(), pitch)
                .map_err(|e| anyhow::anyhow!("upload {tile:?}: {e}"))?;
            set_raw_blend_mode(tex, mode);
        } else {
            let src = pm.data();
            self.staging.clear();
            self.staging.reserve(src.len());
            for px in src.chunks_exact(4) {
                let a = px[3] as usize;
                if a == 0 || a == 255 {
                    self.staging.extend_from_slice(px);
                } else {
                    // premultiplied -> straight: c * 255 / a, rounded (not floored) so the
                    // round-trip doesn't bias colours down — see `painter::fade_step`. Through
                    // the reciprocal table rather than a divide: this core has no vector
                    // integer divide and a scalar `udiv` costs multiple cycles, three per
                    // pixel over as much as a full screen.
                    let recip = RECIP_ALPHA[a];
                    self.staging.push(unpremultiply(px[0], recip));
                    self.staging.push(unpremultiply(px[1], recip));
                    self.staging.push(unpremultiply(px[2], recip));
                    self.staging.push(px[3]);
                }
            }
            tex.update(None, &self.staging, pitch)
                .map_err(|e| anyhow::anyhow!("upload {tile:?}: {e}"))?;
            tex.set_blend_mode(BlendMode::Blend);
        }
        Ok(())
    }

    pub fn has_tile(&self, tile: TileId) -> bool {
        self.tiles.contains_key(&tile)
    }

    /// Frees the blur chain when the look changed under it — the other half of
    /// [`clear_all`](Self::clear_all)'s job, for a switch that never enters a stream.
    ///
    /// Checked every frame rather than pushed from `app`: it is one relaxed load, and the
    /// alternative is a release call the one caller that picks a theme has to remember. The
    /// next frosted frame re-probes and rebuilds from the new [`Glass`]; a look with no glass
    /// never has one, so picking it hands a screen's worth of render targets straight back.
    fn release_theme(&mut self) {
        let epoch = crate::ui::theme::epoch();
        if let Some(frost) = self.frost.take_if(|f| f.epoch != epoch) {
            frost.destroy();
        }
    }

    /// Destroys all cached GPU textures (call on stream start to free VRAM).
    pub fn clear_all(&mut self) {
        // SAFETY: `unsafe_textures` detaches each `Texture` from its creator's
        // lifetime, making the owner responsible for destruction. We drain both
        // collections so nothing can reach these textures again, then destroy each
        // one exactly once. Same invariant as `release`.
        let tiles = self.tiles.drain().map(|(_, t)| t.texture);
        for tex in tiles.chain(self.pool.drain(..).map(|(_, t)| t)) {
            unsafe { tex.destroy() };
        }
        if let Some(frost) = self.frost.take() {
            frost.destroy();
        }
    }

    /// Releases tile's GPU texture. Needed for windowed card tiles when they scroll out of
    /// view; the texture goes to the pool for the row scrolling in to reuse, and is destroyed
    /// outright once that is full.
    pub fn drop_tile(&mut self, tile: TileId) {
        if let Some(tile) = self.tiles.remove(&tile) {
            self.release(tile);
        }
    }

    /// The tile's texture with this frame's fade `alpha` applied, or `None` if it hasn't been
    /// uploaded yet (e.g. art still loading) and the caller should skip the draw.
    fn faded_tile(&mut self, id: &TileId, alpha: u8) -> Option<&Texture> {
        let tile = self.tiles.get_mut(id)?;
        let wanted = tile_alpha_mod(alpha, tile.premultiplied);
        if tile.mods != Some(wanted) {
            tile.mods = Some(wanted);
            tile.texture.set_alpha_mod(wanted.0);
            tile.texture.set_color_mod(wanted.1, wanted.1, wanted.1);
        }
        Some(&tile.texture)
    }

    /// Executes one frame's draw list. The caller has already cleared the canvas
    /// to the background color.
    ///
    /// A list containing a [`DrawCmd::Frost`] is split in two: everything under the first
    /// frosted card is drawn into an offscreen copy the blur can sample, that copy is blitted
    /// to the screen in place of those commands, and the rest of the list runs normally. A
    /// list without one — every in-stream frame — takes the same path it always did.
    pub fn present(&mut self, canvas: &mut Canvas<Window>, cmds: &[DrawCmd]) -> Result<()> {
        // A `Fill` needs alpha blending on the canvas itself; every caller sets the mode it
        // wants before calling, so leaving one behind would silently blend the *next* frame's
        // clear. Restored below rather than set per command — the canvas keeps the state.
        let entry_blend = canvas.blend_mode();
        self.release_theme();
        let Some(first_frost) = cmds.iter().position(|c| matches!(c, DrawCmd::Frost(_))) else {
            self.run(canvas, cmds, None, None)?;
            canvas.set_blend_mode(entry_blend);
            return Ok(());
        };
        // The clear the caller just did, so the offscreen copy starts from the same ground.
        let clear = canvas.draw_color();
        let creator = canvas.texture_creator();
        let size = canvas.output_size().map_err(|e| anyhow::anyhow!("output size: {e}"))?;
        // Out of `self` for the duration: the blur's textures and the tile textures are drawn
        // in the same calls, and `with_texture_canvas` wants its target exclusively.
        let mut frost = match self.frost.take() {
            Some(frost) => frost,
            None => Frost::probe(canvas, &creator, crate::ui::theme::glass().copied()),
        };
        // Every exit from here puts `frost` back before propagating: dropping it on an error
        // path would leak a screen's worth of textures per failing frame, since the next
        // frame would probe and build a fresh one.
        let result = self.frosted_frame(canvas, cmds, first_frost, clear, &creator, size, &mut frost);
        self.frost = Some(frost);
        result?;
        canvas.set_blend_mode(entry_blend);
        Ok(())
    }

    /// What the captured backdrop and the blur chain built off it currently hold: the commands
    /// drawn into it, the ground they were cleared to, the size of the target, and the pixels the
    /// tiles those commands name held at the time.
    ///
    /// A frosted frame whose key matches the last one reuses the capture instead of redrawing and
    /// re-minifying it — which is the common case by a wide margin: a card's title strip animates
    /// its wipe, a modal fades in, and the grid under either is identical frame to frame. That
    /// skips the under-layer's draws, all [`FROST_STEPS`] minifications and six render-target
    /// binds, leaving one full-screen blit as the whole cost of the backdrop.
    ///
    /// The tile contents fold in per *referenced* tile ([`Tile::gen`]), not as one counter over
    /// every upload: the tiles that re-raster every frame — the modal's focused row through a
    /// toggle slide, the card submenu's labels through a row move — are all drawn *above* the
    /// first frost pane and so are not in this slice at all. Keyed off a global counter they
    /// invalidated the capture on exactly the frames the effect is running, which is the whole of
    /// what this cache exists to prevent. A tile with no entry (dropped, or cleared) keys as
    /// absent, and a re-acquired one gets a fresh number, so neither can alias its old contents.
    ///
    /// Hashed rather than compared: keeping the previous list would mean cloning a frame's
    /// commands every frame to save comparing them, and a 64-bit hash of a draw list this size
    /// collides on a timescale nobody will be running this TV for.
    fn backdrop_key(&self, cmds: &[DrawCmd], clear: sdl2::pixels::Color, size: (u32, u32)) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (size, (clear.r, clear.g, clear.b, clear.a)).hash(&mut h);
        let gen = |tile: &TileId| self.tiles.get(tile).map(|t| t.gen);
        for cmd in cmds {
            match cmd {
                DrawCmd::Tex { tile, dst, alpha } => (0u8, tile, gen(tile), rect_key(*dst), alpha).hash(&mut h),
                DrawCmd::TexCropped { tile, src, dst, alpha } => {
                    (1u8, tile, gen(tile), rect_key(*src), rect_key(*dst), alpha).hash(&mut h);
                }
                // Through the bits: the fractional destination is the whole point of this variant,
                // so rounding it here would let a sub-pixel pan reuse a stale capture.
                DrawCmd::Erase { tile, dst } => (5u8, tile, gen(tile), rect_key(*dst)).hash(&mut h),
                DrawCmd::TexF { tile, dst, alpha } => (
                    2u8,
                    tile,
                    gen(tile),
                    dst.x.to_bits(),
                    dst.y.to_bits(),
                    dst.w.to_bits(),
                    dst.h.to_bits(),
                    alpha,
                )
                    .hash(&mut h),
                DrawCmd::Fill { rect, color } => {
                    (3u8, rect_key(*rect), (color.r, color.g, color.b, color.a)).hash(&mut h);
                }
                // Unreachable: the slice hashed here ends at the frame's first frost. Hashed as a
                // bare tag anyway rather than skipped, so a future caller that passes one cannot
                // silently make two different lists key alike.
                DrawCmd::Frost(_) => 4u8.hash(&mut h),
            }
        }
        h.finish()
    }

    /// [`present`](Self::present)'s frosted path: capture the under-layers offscreen, minify
    /// them twice, blit the capture back in their place, then run the rest of the list — the
    /// frost commands in it now have something to sample.
    #[allow(clippy::too_many_arguments)]
    fn frosted_frame(
        &mut self,
        canvas: &mut Canvas<Window>,
        cmds: &[DrawCmd],
        first_frost: usize,
        clear: sdl2::pixels::Color,
        creator: &TextureCreator<WindowContext>,
        size: (u32, u32),
        frost: &mut Frost,
    ) -> Result<()> {
        let key = self.backdrop_key(&cmds[..first_frost], clear, size);
        let rest = if let Some(chain) = frost.chain_for(creator, size) {
            let stale = chain.backdrop_key != Some(key);
            // Cleared before the capture, not after: an error below leaves the texture holding
            // half a frame, and a key still naming it would show that half next frame.
            chain.backdrop_key = None;
            let Chain { backdrop, levels, .. } = chain;
            if stale {
                let mut err = None;
                canvas
                    .with_texture_canvas(backdrop, |c| {
                        c.set_draw_color(clear);
                        c.clear();
                        err = self.run(c, &cmds[..first_frost], None, None).err();
                    })
                    .map_err(|e| anyhow::anyhow!("frost backdrop: {e}"))?;
                if let Some(e) = err {
                    return Err(e);
                }
                // Down the chain. Skipped with the capture: the levels are a pure function of
                // the backdrop, so an unchanged one leaves them all still valid.
                let mut src: &Texture = backdrop;
                for level in levels.iter_mut() {
                    copy_into(canvas, level, src, None)?;
                    src = level;
                }
            }
            // The copy that stands in for the commands captured — reused or not, the screen
            // still needs them.
            canvas
                .copy(backdrop, None, None)
                .map_err(|e| anyhow::anyhow!("frost backdrop blit: {e}"))?;
            chain.backdrop_key = Some(key);
            &cmds[first_frost..]
        } else {
            cmds
        };
        self.run(canvas, rest, Some(creator), Some(frost))
    }

    /// One frame's commands against whatever target is bound. `creator`/`frost` are `Some`
    /// only on the on-screen pass — the offscreen backdrop pass never contains a frost.
    fn run(
        &mut self,
        canvas: &mut Canvas<Window>,
        cmds: &[DrawCmd],
        creator: Option<&TextureCreator<WindowContext>>,
        mut frost: Option<&mut Frost>,
    ) -> Result<()> {
        for cmd in cmds {
            match cmd {
                DrawCmd::Tex { tile, dst, alpha } => {
                    let Some(tex) = self.faded_tile(tile, *alpha) else {
                        continue;
                    };
                    canvas
                        .copy(tex, None, Some(to_sdl_rect(*dst)))
                        .map_err(|e| anyhow::anyhow!("copy {tile:?}: {e}"))?;
                }
                DrawCmd::TexCropped { tile, src, dst, alpha } => {
                    let Some(tex) = self.faded_tile(tile, *alpha) else {
                        continue;
                    };
                    canvas
                        .copy(tex, Some(to_sdl_rect(*src)), Some(to_sdl_rect(*dst)))
                        .map_err(|e| anyhow::anyhow!("copy cropped {tile:?}: {e}"))?;
                }
                DrawCmd::Erase { tile, dst } => {
                    let Some(tex) = self.faded_tile(tile, 0xff) else {
                        continue;
                    };
                    // Restored right after: every other command on this texture — and every
                    // other texture — composites normally.
                    let raw = tex.raw();
                    let restore = unsafe { sdl2::sys::SDL_SetTextureBlendMode(raw, erase_blend()) } == 0;
                    canvas
                        .copy(tex, None, Some(to_sdl_rect(*dst)))
                        .map_err(|e| anyhow::anyhow!("erase {tile:?}: {e}"))?;
                    if restore {
                        unsafe { sdl2::sys::SDL_SetTextureBlendMode(raw, sdl2::sys::SDL_BlendMode::SDL_BLENDMODE_BLEND) };
                    } else {
                        // No custom blend on this renderer: the mask blends normally instead,
                        // so the dissolve fades to the mask's own black rather than to the
                        // plane. Same shape, no punch-through.
                        BLEND_WARNED.call_once(|| {
                            tracing::warn!("SDL custom blend unsupported — hero dissolve falls back to fading to black");
                        });
                    }
                }
                DrawCmd::TexF { tile, dst, alpha } => {
                    let Some(tex) = self.faded_tile(tile, *alpha) else {
                        continue;
                    };
                    canvas
                        .copy_f(tex, None, Some(sdl2::rect::FRect::new(dst.x, dst.y, dst.w, dst.h)))
                        .map_err(|e| anyhow::anyhow!("copy float {tile:?}: {e}"))?;
                }
                DrawCmd::Fill { rect, color } => {
                    if canvas.blend_mode() != BlendMode::Blend {
                        canvas.set_blend_mode(BlendMode::Blend);
                    }
                    canvas.set_draw_color(to_sdl_color(*color));
                    canvas
                        .fill_rect(Some(to_sdl_rect(*rect)))
                        .map_err(|e| anyhow::anyhow!("fill: {e}"))?;
                }
                DrawCmd::Frost(pane) => {
                    if let (Some(creator), Some(frost)) = (creator, frost.as_deref_mut()) {
                        draw_frost(canvas, creator, frost, pane)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// `src` scaled into the whole of the render-target texture `dst`, or into `src_rect` of it.
fn copy_into(canvas: &mut Canvas<Window>, dst: &mut Texture, src: &Texture, src_rect: Option<Rect>) -> Result<()> {
    let mut err = None;
    canvas
        .with_texture_canvas(dst, |c| {
            err = c.copy(src, src_rect.map(to_sdl_rect), None).err();
        })
        .map_err(|e| anyhow::anyhow!("frost target: {e}"))?;
    err.map_or(Ok(()), |e| Err(anyhow::anyhow!("frost copy: {e}")))
}

/// One frosted pane: the blurred backdrop magnified back over `pane.at`, cut to the pane's
/// rounded shape, and the visible part of that blitted to `pane.dst`. Falls back to a flat
/// fill of the same shape where the renderer has no blur chain.
fn draw_frost(
    canvas: &mut Canvas<Window>,
    creator: &TextureCreator<WindowContext>,
    frost: &mut Frost,
    pane: &FrostPane,
) -> Result<()> {
    let (w, h) = (pane.shape.w, pane.shape.h);
    if w == 0 || h == 0 || pane.alpha == 0 || pane.dst.width() == 0 || pane.dst.height() == 0 {
        return Ok(());
    }
    let Frost { chain, masks, .. } = frost;
    let mask_i = slot_for(masks, (w, h, pane.mask), || frost_mask(creator, w, h, pane.mask))?;
    let Some(chain) = chain else {
        // No blur here, so a pane that only exists to re-lay one has nothing to say.
        let Some(fallback) = pane.fallback else {
            return Ok(());
        };
        let (mask, _) = &mut masks[mask_i];
        mask.set_blend_mode(BlendMode::Blend);
        mask.set_alpha_mod(pane.alpha);
        mask.set_color_mod(fallback.r, fallback.g, fallback.b);
        return canvas
            .copy(
                &*mask,
                Some(to_sdl_rect(visible_src(pane))),
                Some(to_sdl_rect(pane.dst)),
            )
            .map_err(|e| anyhow::anyhow!("frost fallback: {e}"));
    };
    let scratch_i = chain.scratch_for(creator, w, h)?;
    let (level, div, size) = chain.level_for(pane.blur);
    let src = chain.blurred_src(pane.at, div, size);
    let mask_mode = chain.mask_mode;
    let Chain {
        backdrop,
        levels,
        scratches,
        grain,
        ..
    } = chain;
    let blurred: &Texture = level.map_or(backdrop, |i| &levels[i]);
    let (scratch, _) = &mut scratches[scratch_i];
    let (mask, _) = &masks[mask_i];
    set_raw_blend_mode(mask, mask_mode);
    let grain = grain.as_ref();
    let mut err = None;
    // Blur, grain and mask in one binding of the scratch. Three separate `with_texture_canvas`
    // calls would be three render-target switches per pane per frame, and on a tiled GPU each
    // one resolves and reloads the whole attachment — far more than the copies cost.
    canvas
        .with_texture_canvas(scratch, |c| {
            // The whole shape, not just the visible window: the scratch is shape-sized and
            // cached, so blurring all of it is one fixed copy instead of a per-frame resize
            // during a wipe.
            err = c.copy(blurred, Some(to_sdl_rect(src)), None).err();
            // Grain next, then the mask: the mask only writes alpha, so anything laid after
            // it would paint outside the rounded shape.
            if let Some(grain) = grain {
                for ty in (0..h).step_by(GRAIN_TILE as usize) {
                    for tx in (0..w).step_by(GRAIN_TILE as usize) {
                        let tile = Rect::new(tx as i32, ty as i32, GRAIN_TILE, GRAIN_TILE);
                        if err.is_none() {
                            err = c.copy(grain, None, Some(to_sdl_rect(tile))).err();
                        }
                    }
                }
            }
            if err.is_none() {
                err = c.copy(mask, None, None).err();
            }
        })
        .map_err(|e| anyhow::anyhow!("frost scratch target: {e}"))?;
    if let Some(e) = err {
        return Err(anyhow::anyhow!("frost scratch copy: {e}"));
    }
    scratch.set_blend_mode(BlendMode::Blend);
    scratch.set_alpha_mod(pane.alpha);
    canvas
        .copy(
            &*scratch,
            Some(to_sdl_rect(visible_src(pane))),
            Some(to_sdl_rect(pane.dst)),
        )
        .map_err(|e| anyhow::anyhow!("frost blit: {e}"))
}

/// Edge of the square grain tile, in texels. Tiled 1:1 over the pane, so this only trades
/// texture bytes against blit count: 256 covers a full-screen pane in ~32 copies of a 256 KB
/// texture, where a 64px tile would cost ~500.
const GRAIN_TILE: u32 = 256;

/// One tile of neutral etch grain — per-texel white noise around mid grey, tiled over a pane's
/// blur so the surface reads as *rough* rather than merely out of focus.
///
/// Grey rather than white or black: at [`Glass::grain`] strength a neutral wash pulls the blur
/// very slightly toward mid and leaves the tint above it to set the level, so brightening and
/// darkening cancel across the pane instead of fogging it.
fn grain_tile(creator: &TextureCreator<WindowContext>, alpha: u8) -> Result<Texture> {
    let n = (GRAIN_TILE * GRAIN_TILE) as usize;
    let mut px = Vec::with_capacity(n * 4);
    // An LCG, not `rand`: the tile is built once and only has to be uncorrelated, not random.
    let mut seed: u32 = 0x9e37_79b9;
    for _ in 0..n {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // Two draws averaged, so the distribution centres instead of being flat — extremes
        // are what read as noise.
        let v = (((seed >> 24) as u16 + ((seed >> 8) & 0xff) as u16) / 2) as u8;
        px.extend_from_slice(&[v, v, v, 0xff]);
    }
    let mut tex = creator
        .create_texture_static(PixelFormatEnum::RGBA32, GRAIN_TILE, GRAIN_TILE)
        .map_err(|e| anyhow::anyhow!("frost grain: {e}"))?;
    tex.update(None, &px, GRAIN_TILE as usize * 4)
        .map_err(|e| anyhow::anyhow!("frost grain upload: {e}"))?;
    tex.set_blend_mode(BlendMode::Blend);
    tex.set_alpha_mod(alpha);
    Ok(tex)
}

/// One pane's alpha mask, at its unscaled shape size. White throughout — only the alpha is read
/// on the masking path, and the fallback's colour mod wants an unmodified base.
///
/// Forced back to *straight* alpha after rasterizing: a `Painter` hands back premultiplied
/// pixels, so an antialiased corner texel is `(a, a, a, a)`. The mask path reads nothing but
/// the alpha and cannot tell, but the fallback path blits this with an ordinary
/// [`BlendMode::Blend`], which multiplies by the alpha a second time — the flat panes' rounded
/// corners came out darker and harder than the shape they are cut from. White is the same in
/// both encodings once the RGB is pinned at full, and it is what the fallback's colour mod
/// wants anyway.
fn frost_mask(creator: &TextureCreator<WindowContext>, w: u32, h: u32, mask: FrostMask) -> Result<Texture> {
    const WHITE: Color = Color::RGBA(0xff, 0xff, 0xff, 0xff);
    let mut p = Painter::new(w, h);
    let shape = match mask.corners {
        Corners::All => Rect::new(0, 0, w, h),
        // Grown past the top edge so only the bottom pair of corners lands on the mask — the
        // same trick `ui::widgets::bottom_rounded` plays on the tiles.
        Corners::Bottom => Rect::new(0, -mask.radius, w, h + mask.radius as u32),
    };
    p.fill_rounded_rect(shape, mask.radius, WHITE);
    let mut px = p.data().to_vec();
    for texel in px.chunks_exact_mut(4) {
        texel[..3].fill(0xff);
    }
    let mut tex = creator
        .create_texture_static(PixelFormatEnum::RGBA32, w, h)
        .map_err(|e| anyhow::anyhow!("frost mask {w}x{h}: {e}"))?;
    tex.update(None, &px, w as usize * 4)
        .map_err(|e| anyhow::anyhow!("frost mask upload: {e}"))?;
    Ok(tex)
}

/// `pane.dst` as a rect in the pane's own shape-sized texels — where the visible window falls
/// within the whole shape, undoing whatever zoom `at` carries.
fn visible_src(pane: &FrostPane) -> Rect {
    let (aw, ah) = (i64::from(pane.at.width().max(1)), i64::from(pane.at.height().max(1)));
    let map = |v: i32, span: i64, extent: u32| ((i64::from(v) * i64::from(extent)) / span) as i32;
    texel_rect(
        map(pane.dst.x() - pane.at.x(), aw, pane.shape.w),
        map(pane.dst.y() - pane.at.y(), ah, pane.shape.h),
        map(pane.dst.right() - pane.at.x(), aw, pane.shape.w),
        map(pane.dst.bottom() - pane.at.y(), ah, pane.shape.h),
        (pane.shape.w, pane.shape.h),
    )
}

/// The edges `x`/`y`/`right`/`bottom` as a rect inside a `(w, h)` texture, never empty.
///
/// The origin clamps to `w - 1`, not `w`: a card scrolled clear of the viewport maps past the
/// last texel, and `clamp(x + 1, w)` panics the moment its min exceeds its max. Leaving one
/// texel of room turns that into a degenerate sample instead of a crash.
fn texel_rect(x: i32, y: i32, right: i32, bottom: i32, (w, h): (u32, u32)) -> Rect {
    let (w, h) = (w.max(1) as i32, h.max(1) as i32);
    let x = x.clamp(0, w - 1);
    let y = y.clamp(0, h - 1);
    Rect::new(
        x,
        y,
        (right.clamp(x + 1, w) - x) as u32,
        (bottom.clamp(y + 1, h) - y) as u32,
    )
}
