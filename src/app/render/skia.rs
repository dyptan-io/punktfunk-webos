//! The old draw list on a Skia canvas.
//!
//! Transitional (`webos-pointer-ui-overhaul.md` WP3): every screen the CPU raster path still
//! builds keeps building its tiles, and this draws them as images on the console's GL context
//! — or on a raster surface, which is how the host tests see the frame. It dies with the last
//! tile. What a screen port replaces is the tile it needed here, never this module's shape.
//!
//! Every [`DrawCmd`] has a native Skia form. [`DrawCmd::Erase`] is `DstOut`;
//! [`DrawCmd::Frost`] is a backdrop-filtered layer clipped to the pane's shape, which is what
//! the SDL compositor's minification chain approximated.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use skia_safe::canvas::{SaveLayerRec, SrcRectConstraint};
use skia_safe::{
    image_filters, images, AlphaType, BlendMode, Canvas, ClipOp, Color4f, ColorType, Data, FilterMode, Image,
    ImageInfo, MipmapMode, Paint, RRect, Rect as SkRect, SamplingOptions, TileMode, Vector,
};

use crate::ui::painter::Painter;
use crate::ui::render::{Color, Corners, DrawCmd, FrostPane, Rect, RectF, TileId, TileSink};

/// A raw upload's pixel layout. The two the loop feeds: the hero's decoded art and the two
/// dissolve masks, whose only meaningful channel is alpha.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RawFormat {
    Rgb565,
    /// Straight alpha, R first in memory — SDL's `ABGR8888` on a little-endian core.
    Rgba8888,
}

impl RawFormat {
    pub(crate) fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgba8888 => 4,
        }
    }

    pub(crate) fn info(self, w: u32, h: u32) -> ImageInfo {
        let (ct, at) = match self {
            Self::Rgb565 => (ColorType::RGB565, AlphaType::Opaque),
            Self::Rgba8888 => (ColorType::RGBA8888, AlphaType::Unpremul),
        };
        ImageInfo::new((w as i32, h as i32), ct, at, None)
    }
}

/// Every tile with an image, keyed by id. The pixels are copied once at upload; Skia moves
/// them to the GPU on first draw and keeps the texture in its resource cache.
#[derive(Default)]
pub(crate) struct SkiaTiles {
    images: HashMap<TileId, Image>,
}

impl SkiaTiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drop_tile(&mut self, tile: TileId) {
        self.images.remove(&tile);
    }

    /// Draws `cmds` in order. A tile with no image is skipped: the loop uploads before it
    /// presents, so a miss is a tile evicted between the two, and the old compositor drew
    /// nothing for it too.
    pub fn present(&self, canvas: &Canvas, cmds: &[DrawCmd]) {
        for cmd in cmds {
            match cmd {
                DrawCmd::Tex { tile, dst, alpha } => {
                    if let Some(img) = self.images.get(tile) {
                        canvas.draw_image_rect_with_sampling_options(
                            img,
                            None,
                            sk(*dst),
                            nearest(),
                            &alpha_paint(*alpha),
                        );
                    }
                }
                DrawCmd::TexCropped { tile, src, dst, alpha } => {
                    if let Some(img) = self.images.get(tile) {
                        canvas.draw_image_rect_with_sampling_options(
                            img,
                            Some((&sk(*src), SrcRectConstraint::Strict)),
                            sk(*dst),
                            nearest(),
                            &alpha_paint(*alpha),
                        );
                    }
                }
                DrawCmd::TexF { tile, dst, alpha } => {
                    if let Some(img) = self.images.get(tile) {
                        canvas.draw_image_rect_with_sampling_options(
                            img,
                            None,
                            skf(*dst),
                            linear(),
                            &alpha_paint(*alpha),
                        );
                    }
                }
                DrawCmd::Erase { tile, dst } => {
                    if let Some(img) = self.images.get(tile) {
                        let mut p = Paint::default();
                        p.set_blend_mode(BlendMode::DstOut);
                        canvas.draw_image_rect_with_sampling_options(img, None, sk(*dst), linear(), &p);
                    }
                }
                DrawCmd::Fill { rect, color } => {
                    canvas.draw_rect(sk(*rect), &fill(*color));
                }
                DrawCmd::Frost(pane) => frost(canvas, pane),
            }
        }
    }
}

impl TileSink for SkiaTiles {
    /// `opaque` is the SDL path's blend hint; an opaque image just has no alpha to blend.
    fn upload(&mut self, tile: TileId, pm: &Painter, _opaque: bool) -> Result<()> {
        let (w, h) = (pm.width(), pm.height());
        let info = ImageInfo::new((w as i32, h as i32), ColorType::RGBA8888, AlphaType::Premul, None);
        let image = images::raster_from_data(&info, Data::new_copy(pm.data()), w as usize * 4)
            .ok_or_else(|| anyhow!("upload {tile:?}: Skia refused a {w}x{h} tile"))?;
        self.images.insert(tile, image);
        Ok(())
    }
}

/// Blur what the frame has composed under `pane`, cut to its shape, at its alpha. The blur's
/// sigma is half the SDL chain's minification width — the same visual spread. `fallback` is
/// unused: there is no renderer here that cannot blur.
fn frost(canvas: &Canvas, pane: &FrostPane) {
    let Some(blur) = image_filters::blur(
        (pane.blur as f32 * 0.5, pane.blur as f32 * 0.5),
        TileMode::Clamp,
        None,
        None,
    ) else {
        return;
    };
    // The mask is in `shape`'s units; `at` is where the whole shape lands, zoom included.
    let sx = pane.at.width() as f32 / pane.shape.w.max(1) as f32;
    let sy = pane.at.height() as f32 / pane.shape.h.max(1) as f32;
    let r = pane.mask.radius as f32;
    let (rx, ry) = (r * sx, r * sy);
    let shape = match pane.mask.corners {
        Corners::All => RRect::new_rect_xy(sk(pane.at), rx, ry),
        Corners::Bottom => RRect::new_rect_radii(
            sk(pane.at),
            &[
                Vector::new(0.0, 0.0),
                Vector::new(0.0, 0.0),
                Vector::new(rx, ry),
                Vector::new(rx, ry),
            ],
        ),
    };
    let dst = sk(pane.dst);
    let mut paint = Paint::default();
    paint.set_alpha_f(f32::from(pane.alpha) / 255.0);
    canvas.save();
    canvas.clip_rect(dst, ClipOp::Intersect, true);
    canvas.clip_rrect(shape, ClipOp::Intersect, true);
    let rec = SaveLayerRec::default().bounds(&dst).paint(&paint).backdrop(&blur);
    canvas.save_layer(&rec);
    canvas.restore();
    canvas.restore();
}

fn sk(r: Rect) -> SkRect {
    SkRect::from_xywh(r.x() as f32, r.y() as f32, r.width() as f32, r.height() as f32)
}

fn skf(r: RectF) -> SkRect {
    SkRect::from_xywh(r.x, r.y, r.w, r.h)
}

fn fill(c: Color) -> Paint {
    let mut p = Paint::new(
        Color4f::new(
            f32::from(c.r) / 255.0,
            f32::from(c.g) / 255.0,
            f32::from(c.b) / 255.0,
            f32::from(c.a) / 255.0,
        ),
        None,
    );
    p.set_anti_alias(true);
    p
}

fn alpha_paint(alpha: u8) -> Paint {
    let mut p = Paint::default();
    p.set_alpha_f(f32::from(alpha) / 255.0);
    p
}

/// A tile drawn at its own size: whole texels, as SDL's default scale mode did.
fn nearest() -> SamplingOptions {
    SamplingOptions::new(FilterMode::Nearest, MipmapMode::None)
}

/// A subpixel pan, a mask, a zoom: what has to read as motion rather than as steps.
fn linear() -> SamplingOptions {
    SamplingOptions::new(FilterMode::Linear, MipmapMode::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render::{FrostMask, Size};

    fn tiles_with_square(color: [u8; 4]) -> SkiaTiles {
        let mut pm = Painter::new(4, 4);
        let straight = crate::ui::render::Color::RGBA(color[0], color[1], color[2], color[3]);
        pm.fill_rect(Rect::new(0, 0, 4, 4), straight);
        let mut tiles = SkiaTiles::new();
        tiles.upload(TileId(1), &pm, false).unwrap();
        tiles
    }

    fn render(tiles: &SkiaTiles, cmds: &[DrawCmd]) -> Vec<u8> {
        let mut surface = skia_safe::surfaces::raster_n32_premul((8, 8)).unwrap();
        surface.canvas().clear(Color4f::new(0.0, 0.0, 0.0, 1.0));
        tiles.present(surface.canvas(), cmds);
        let info = ImageInfo::new((8, 8), ColorType::RGBA8888, AlphaType::Unpremul, None);
        let mut out = vec![0u8; 8 * 8 * 4];
        assert!(surface.read_pixels(&info, &mut out, 8 * 4, (0, 0)));
        out
    }

    fn px(buf: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * 8 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// A tile lands where its command says, at its command's alpha, and nowhere else.
    #[test]
    fn tex_draws_at_dst_with_alpha() {
        let tiles = tiles_with_square([255, 0, 0, 255]);
        let out = render(
            &tiles,
            &[DrawCmd::Tex {
                tile: TileId(1),
                dst: Rect::new(2, 2, 4, 4),
                alpha: 128,
            }],
        );
        assert_eq!(px(&out, 0, 0), [0, 0, 0, 255]);
        let [r, g, b, a] = px(&out, 3, 3);
        assert!((120..=136).contains(&r), "{r}");
        assert_eq!((g, b, a), (0, 0, 255));
        assert_eq!(px(&out, 7, 7), [0, 0, 0, 255]);
    }

    /// `Erase` removes rather than adds: an opaque mask pixel leaves the target transparent.
    #[test]
    fn erase_is_dst_out() {
        let tiles = tiles_with_square([255, 255, 255, 255]);
        let out = render(
            &tiles,
            &[
                DrawCmd::Fill {
                    rect: Rect::new(0, 0, 8, 8),
                    color: Color::RGB(0, 255, 0),
                },
                DrawCmd::Erase {
                    tile: TileId(1),
                    dst: Rect::new(0, 0, 4, 4),
                },
            ],
        );
        assert_eq!(px(&out, 1, 1), [0, 0, 0, 0]);
        assert_eq!(px(&out, 6, 6), [0, 255, 0, 255]);
    }

    /// A frost pane blurs what is under it: a hard edge between two fills softens inside the
    /// pane and stays hard outside it.
    #[test]
    fn frost_blurs_only_inside_the_pane() {
        let tiles = SkiaTiles::new();
        let split = |x0: i32| DrawCmd::Fill {
            rect: Rect::new(x0, 0, 4, 8),
            color: Color::RGB(255, 255, 255),
        };
        let pane = FrostPane {
            shape: Size::new(8, 4),
            at: Rect::new(0, 0, 8, 4),
            dst: Rect::new(0, 0, 8, 4),
            mask: FrostMask {
                radius: 0,
                corners: Corners::All,
            },
            blur: 4,
            alpha: 255,
            fallback: None,
        };
        let out = render(&tiles, &[split(4), DrawCmd::Frost(Box::new(pane))]);
        // Inside the pane the edge pixel is a mix; below it the edge is still black|white.
        let inside = px(&out, 3, 1)[0];
        assert!(inside > 0 && inside < 255, "inside the pane: {inside}");
        assert_eq!(px(&out, 3, 6)[0], 0);
        assert_eq!(px(&out, 4, 6)[0], 255);
    }
}
