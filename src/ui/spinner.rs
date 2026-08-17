//! The punktfunk lens loading animation (matching `web/src/components/ui/spinner.tsx`),
//! rasterized in `tiny_skia`. Two overlapping circles orbit in antiphase along a depth
//! axis, faking perspective with scale and paint order; overlap glows via `Screen` blend.
use crate::ui::render::Color;
use crate::ui::Painter;
use std::time::Duration;

/// One full orbit (seamless loop).
pub const CYCLE: Duration = Duration::from_millis(1600);

/// Frames per cycle (25fps — smooth enough for this motion, light on VRAM).
pub const FRAMES: usize = 40;

/// Rasterized frame size in pixels.
pub const SIZE: u32 = 140;

/// The lens box as a fraction of the pixmap — the rest is the headroom the near lobe needs.
const BOX_FRAC: f32 = 0.8;

const R_DEPTH: f32 = 0.34; // depth amplitude (fraction of box) -> the size change
const PERSP: f32 = 1.05; // perspective distance (fraction of box); smaller -> stronger scaling
const R_PLANE_FIXED: f32 = 0.12; // constant in-plane offset -> the two never fully eclipse
const R_PLANE_SWAY: f32 = 0.05; // small in-plane breathing
const DIAG: (f32, f32) = (-std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2); // lens axis
const LOBE_FRAC: f32 = 0.58; // circle diameter as a fraction of the box

/// One lobe's center offset (from the pixmap center), radius, and depth at phase `t`.
/// `side` is `+1` for the light lobe, `-1` for the deep one — the antiphase pair.
fn lobe(t: f32, side: f32) -> (f32, f32, f32, f32) {
    let s = SIZE as f32 * BOX_FRAC;
    let angle = t * std::f32::consts::TAU;
    let z = side * angle.sin() * R_DEPTH;
    let p = PERSP / (PERSP - z); // nearer -> bigger, farther -> smaller
    let mag = (R_PLANE_FIXED + R_PLANE_SWAY * angle.cos()) * side;
    (mag * DIAG.0 * p * s, mag * DIAG.1 * p * s, LOBE_FRAC / 2.0 * p * s, z)
}

/// The frame at phase `t` (0..1 through [`CYCLE`]), on a transparent [`SIZE`]-square pixmap.
pub fn frame(t: f32, light: Color, deep: Color) -> Painter {
    let mut p = Painter::new(SIZE, SIZE);
    let c = SIZE as f32 / 2.0;
    let mut lobes = [(lobe(t, 1.0), light), (lobe(t, -1.0), deep)];
    // Farther first, so the nearer lobe is the one painted on top.
    lobes.sort_by(|a, b| a.0 .3.total_cmp(&b.0 .3));
    for ((dx, dy, r, _), color) in lobes {
        p.fill_circle_screen(c + dx, c + dy, r, color);
    }
    p
}
