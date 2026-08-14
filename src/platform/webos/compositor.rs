//! GPU composition of the pre-stream UI (the `opengles2` SDL renderer confirmed
//! live on-device): tiny-skia rasterizes widgets into cached tiles
//! ([`crate::ui`]'s `render_*_tile` helpers — the AA/soft-shadow look is
//! unchanged), and this module owns their GPU textures and executes `App`'s
//! per-frame draw list. Position, scroll, the focus pop's scale, and fades are
//! all texture-copy parameters here — per-frame CPU rasterization cost is gone,
//! which is what makes 60fps animation feasible on this hardware (the previous
//! CPU compositor measured ~25-45ms/frame; see docs/NOTES.md).

use std::collections::{HashMap, HashSet};

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

pub struct Compositor {
    textures: HashMap<TileId, Texture>,
    /// Reused staging buffer for the premultiplied → straight-alpha conversion, on the
    /// fallback path only (see [`premultiplied_blend`]).
    staging: Vec<u8>,
    /// The composed premultiplied blend mode, probed once — `None` until probed, `Some(None)`
    /// if the renderer refused it.
    premultiplied: Option<Option<sdl2::sys::SDL_BlendMode>>,
    /// Tiles whose texture holds premultiplied pixels, so `present` knows to scale their colour
    /// alongside their alpha (see [`set_tile_alpha`]).
    premul_tiles: HashSet<TileId>,
}

/// Applies a draw list's per-tile fade `alpha` to a texture.
///
/// A premultiplied source needs its colour scaled by the same factor as its alpha, which
/// `set_alpha_mod` alone does not do — SDL modulates the texel by the vertex colour, whose RGB
/// stays 255 when only alpha mod is set, so the source arrives as `(rgb, a * alpha)`: full
/// brightness under shrinking coverage, i.e. a modal that holds near-opaque through most of the
/// fade and then cuts. Colour mod at `alpha` too gives `(rgb * alpha, a * alpha)`.
///
/// Straight-alpha and opaque tiles must keep colour mod at 255 — their colour is independent of
/// coverage, so scaling it would darken them as they faded.
fn set_tile_alpha(tex: &mut Texture, alpha: u8, premultiplied: bool) {
    tex.set_alpha_mod(alpha);
    let c = if premultiplied { alpha } else { 255 };
    tex.set_color_mod(c, c, c);
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
/// alone does not do — see [`set_tile_alpha`].
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
    // SAFETY: `tex.raw()` is a live texture owned by `self.textures`, and `SDL_SetTextureBlendMode`
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
            textures: HashMap::new(),
            staging: Vec::new(),
            premultiplied: None,
            premul_tiles: HashSet::new(),
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

    /// Uploads straight-RGBA8 bytes to a new GPU texture. No-op if already cached.
    pub fn upload_raw(
        &mut self,
        creator: &TextureCreator<WindowContext>,
        tile: TileId,
        w: u32,
        h: u32,
        rgba_straight: &[u8],
    ) -> Result<()> {
        if self.textures.contains_key(&tile) {
            return Ok(());
        }
        let mut tex = creator
            .create_texture_static(PixelFormatEnum::RGBA32, w, h)
            .map_err(|e| anyhow::anyhow!("create texture {tile:?} {w}x{h}: {e}"))?;
        let pitch = w as usize * 4;
        tex.update(None, rgba_straight, pitch)
            .map_err(|e| anyhow::anyhow!("upload {tile:?}: {e}"))?;
        tex.set_blend_mode(BlendMode::Blend);
        self.textures.insert(tile, tex);
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
        let recreate = match self.textures.get(&tile) {
            Some(t) => {
                let q = t.query();
                q.width != w || q.height != h
            }
            None => true,
        };
        if recreate {
            let tex = creator
                .create_texture_static(PixelFormatEnum::RGBA32, w, h)
                .map_err(|e| anyhow::anyhow!("create texture {tile:?} {w}x{h}: {e}"))?;
            self.textures.insert(tile, tex);
        }
        let tex = self.textures.get_mut(&tile).expect("just inserted");
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
        if premultiplied.is_some() {
            self.premul_tiles.insert(tile);
        } else {
            self.premul_tiles.remove(&tile);
        }
        Ok(())
    }

    /// Destroys all cached GPU textures (call on stream start to free VRAM).
    pub fn clear_all(&mut self) {
        // SAFETY: `unsafe_textures` detaches each `Texture` from its creator's
        // lifetime, making the owner responsible for destruction. We drain the
        // map so nothing can reach these textures again, then destroy each one
        // exactly once. Same invariant as `drop_tile`.
        for (_, tex) in self.textures.drain() {
            unsafe { tex.destroy() };
        }
        self.premul_tiles.clear();
    }

    /// Drops tile's GPU texture. Needed for windowed card tiles to free VRAM
    /// when scrolled out of view (SDL object must be explicitly destroyed).
    pub fn drop_tile(&mut self, tile: TileId) {
        if let Some(tex) = self.textures.remove(&tile) {
            // SAFETY: see `clear_all`.
            unsafe { tex.destroy() };
        }
        self.premul_tiles.remove(&tile);
    }

    /// The tile's texture with this frame's fade `alpha` applied, or `None` if it hasn't been
    /// uploaded yet (e.g. art still loading) and the caller should skip the draw.
    fn faded_tile(&mut self, tile: &TileId, alpha: u8) -> Option<&Texture> {
        let premultiplied = self.premul_tiles.contains(tile);
        let tex = self.textures.get_mut(tile)?;
        set_tile_alpha(tex, alpha, premultiplied);
        Some(tex)
    }

    /// Executes one frame's draw list. The caller has already cleared the canvas
    /// to the background color.
    pub fn present(&mut self, canvas: &mut Canvas<Window>, cmds: &[DrawCmd]) -> Result<()> {
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
                    canvas.set_blend_mode(BlendMode::Blend);
                    canvas.set_draw_color(to_sdl_color(*color));
                    canvas
                        .fill_rect(Some(to_sdl_rect(*rect)))
                        .map_err(|e| anyhow::anyhow!("fill: {e}"))?;
                }
            }
        }
        Ok(())
    }
}
