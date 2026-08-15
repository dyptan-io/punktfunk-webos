use crate::ui::render::Rect;
use std::time::{Duration, Instant};

/// How long a focused widget takes to pop to its zoomed size.
pub const FOCUS_POP: Duration = Duration::from_millis(140);

/// The grid card's focus animation: how long the zoom, the glow bloom and the title
/// strip's wipe take. One duration for all three so they land together — and the clock
/// driving them (`App::focus_anim`) is cleared on it, so nothing may outlast it.
pub const CARD_FOCUS_POP: Duration = Duration::from_millis(160);

/// Cubic ease-out function.
pub fn ease(f: f32) -> f32 {
    1.0 - (1.0 - f).powi(3)
}

/// Eased at both ends, unlike [`ease`]'s instant start.
pub fn smoothstep(f: f32) -> f32 {
    f * f * (3.0 - 2.0 * f)
}

/// Eased progress 0..=1 of animation; 1.0 when done/absent.
pub fn anim_frac(anim: Option<Instant>, dur: Duration) -> f32 {
    frac(anim, dur, ease)
}

/// [`anim_frac`] on a cubic ease-*in* — the exact time-mirror of [`ease`]. Fade-ins use
/// this so they read like the fade-outs (`1 - ease(p)`) played backwards; on ease-out a
/// fade-in is already near-opaque a sixth of the way through, so it lands as a pop.
pub fn anim_frac_in(anim: Option<Instant>, dur: Duration) -> f32 {
    frac(anim, dur, |f| f.powi(3))
}

/// [`anim_frac`] on [`smoothstep`]. For the grid card's focus pop: a cubic ease-out puts
/// most of the scale change in the first frames, which at card size reads as a snap
/// followed by a drift rather than one motion.
pub fn anim_frac_smooth(anim: Option<Instant>, dur: Duration) -> f32 {
    frac(anim, dur, smoothstep)
}

fn frac(anim: Option<Instant>, dur: Duration, curve: impl Fn(f32) -> f32) -> f32 {
    match anim {
        Some(t) => curve((t.elapsed().as_secs_f32() / dur.as_secs_f32()).min(1.0)),
        None => 1.0,
    }
}

/// Scales `base` by `1.0 + growth * frac` around its own center — the GPU
/// zoom-in technique behind every focus-pop in the app. The source tile is
/// rasterized once, at its literal size; only this destination rect changes
/// per frame, so the zoom costs nothing beyond a GPU texture copy at a
/// different size.
pub fn zoom_rect(base: Rect, frac: f32, growth: f32) -> Rect {
    scale_about(base, base, zoom_scale(frac, growth))
}

/// The factor [`zoom_rect`] scales by — for a piece composited onto a zooming tile,
/// which has to fold the same factor into its own transform.
pub fn zoom_scale(frac: f32, growth: f32) -> f32 {
    1.0 + growth * frac
}

/// Scale up from (1.0 - shrink) to full size. "Pop in" counterpart to `zoom_rect`.
pub fn pop_in_rect(base: Rect, frac: f32, shrink: f32) -> Rect {
    scale_about(base, base, pop_in_scale(frac, shrink))
}

/// The factor [`pop_in_rect`] scales by at `frac` — for a piece composited onto a
/// popping tile, which has to fold the same factor into its own transform.
pub fn pop_in_scale(frac: f32, shrink: f32) -> f32 {
    if frac >= 1.0 {
        1.0
    } else {
        1.0 - shrink * (1.0 - frac)
    }
}

/// Scales `rect` by `scale` about `pivot`'s center — for a sub-rect composited on top of
/// an already-scaled tile, which must ride that tile's transform. Passing the sub-rect as
/// its own pivot ([`zoom_rect`]) scales it in place; a piece sitting off-center (the grid
/// card's title strip, pinned to its bottom edge) needs the whole card as pivot or it
/// drifts away from the art beneath it.
pub fn scale_about(rect: Rect, pivot: Rect, scale: f32) -> Rect {
    let cx = pivot.x() as f32 + pivot.width() as f32 / 2.0;
    let cy = pivot.y() as f32 + pivot.height() as f32 / 2.0;
    let x = cx + (rect.x() as f32 - cx) * scale;
    let y = cy + (rect.y() as f32 - cy) * scale;
    Rect::new(
        x as i32,
        y as i32,
        (rect.width() as f32 * scale) as u32,
        (rect.height() as f32 * scale) as u32,
    )
}
