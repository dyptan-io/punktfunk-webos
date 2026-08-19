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
use sdl2::render::{BlendMode, Canvas, Texture, TextureCreator};
use sdl2::video::{Window, WindowContext};

use crate::ui::render::{Color, DrawCmd, Rect, TileId};
use crate::ui::Painter;

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
        let supported = creator
            .create_texture_static(PixelFormatEnum::RGBA32, 1, 1)
            .ok()
            .is_some_and(|probe| {
                let ok = set_raw_blend_mode(&probe, mode);
                // SAFETY: `unsafe_textures` makes destruction the owner's job; this probe was
                // never stored anywhere, so this is its only destroy.
                unsafe { probe.destroy() };
                ok
            });
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
        let entry = self.tiles.get_mut(&tile).expect("acquired above or already fresh");
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
        let entry = self.tiles.get_mut(&tile).expect("acquired above or already fresh");
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
                    // round-trip doesn't bias colours down — see `fill_vertical_fade`. Through
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
    pub fn present(&mut self, canvas: &mut Canvas<Window>, cmds: &[DrawCmd]) -> Result<()> {
        // A `Fill` needs alpha blending on the canvas itself; every caller sets the mode it
        // wants before calling, so leaving one behind would silently blend the *next* frame's
        // clear. Restored below rather than set per command — the canvas keeps the state.
        let entry_blend = canvas.blend_mode();
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
            }
        }
        canvas.set_blend_mode(entry_blend);
        Ok(())
    }
}
