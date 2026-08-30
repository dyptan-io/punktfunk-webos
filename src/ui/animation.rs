use crate::ui::render::Rect;
use std::time::{Duration, Instant};

/// How long a focused widget takes to pop to its zoomed size.
pub const FOCUS_POP: Duration = Duration::from_millis(140);

/// The grid card's focus animation: how long the zoom, the glow bloom and the title
/// strip's wipe take. One duration for all three so they land together — and the clock
/// driving them (`App::focus_anim`) is cleared on it, so nothing may outlast it.
pub const CARD_FOCUS_POP: Duration = Duration::from_millis(160);

/// How long a held card's submenu panel takes to climb the card. Named separately from
/// [`CARD_FOCUS_POP`] (which it currently matches) because it answers a different question:
/// the panel covers several times the title strip's travel, so if the rise ever needs its
/// own timing this is the knob. Eased at both ends ([`anim_frac_smooth`]) so the long travel
/// reads as one motion.
pub const CARD_MENU_RISE: Duration = CARD_FOCUS_POP;

/// Fraction of the remaining distance one 16ms tick covers — the constant this ease was
/// written with, kept as the unit [`ease_scroll`] expresses its rate in.
const SCROLL_STEP_PER_TICK: f64 = 0.35;

/// The tick length [`SCROLL_STEP_PER_TICK`] is quoted at.
pub const SCROLL_STEP_TICK: Duration = Duration::from_millis(16);

/// A frame long enough that the scroll should resume rather than teleport: the app was
/// stalled (or idle between animations), and covering the whole gap at once reads as a jump.
const SCROLL_MAX_DT: Duration = Duration::from_millis(64);

/// Advances an eased scroll by `dt`: cover ~35% of the remaining distance per 16ms, snapping
/// when close so it terminates. Returns whether anything moved. Shared by the card grid and
/// the scrolling modals' viewports so both lists feel identical.
///
/// Rate, not step-per-call: stepping a fixed fraction per *tick* made scroll speed a function
/// of the achieved frame rate, so a frame that overran its budget slowed the motion itself
/// rather than only its smoothness — and pinned the loop's tick budget in place.
pub fn ease_scroll(current: &mut i32, target: i32, dt: Duration) -> bool {
    let d = target - *current;
    if d == 0 {
        return false;
    }
    let ticks = dt.min(SCROLL_MAX_DT).as_secs_f64() / SCROLL_STEP_TICK.as_secs_f64();
    let covered = 1.0 - (1.0 - SCROLL_STEP_PER_TICK).powf(ticks);
    let step = if d.abs() <= 3 {
        d
    } else {
        match (f64::from(d) * covered) as i32 {
            0 => d.signum(),
            s => s,
        }
    };
    *current += step;
    true
}

/// How long a pressed button takes to spring back out of its dip.
pub const PRESS_POP: Duration = Duration::from_millis(120);

/// How far a pressed widget sinks, in px.
const PRESS_DROP: f32 = 5.0;

/// How far a focused widget's tile grows while focused — the pop every composited focus
/// tile rides (see [`focus_tile_rect`]).
pub const FOCUS_GROWTH: f32 = 0.02;

/// A button being pushed in. Same animation wherever a button lives, so only the clock
/// belongs to its owner (`App`'s focused widget, the in-stream `ConfirmDialog`).
#[derive(Default, Clone, Copy)]
pub struct Press(Option<Instant>);

impl Press {
    /// Starts the dip. Purely visual — the action runs the moment the press arrives.
    pub fn arm(&mut self) {
        self.0 = Some(Instant::now());
    }

    /// Whether a dip is in flight — frames are owed while it is.
    pub fn armed(self) -> bool {
        self.0.is_some()
    }

    /// Whether an armed dip has played all the way out.
    pub fn landed(self) -> bool {
        self.0.is_some_and(|t| t.elapsed() >= PRESS_POP)
    }

    /// Disarms, reporting whether anything was armed.
    pub fn take(&mut self) -> bool {
        self.0.take().is_some()
    }

    /// `base` pushed down by however far this press has got.
    ///
    /// A translation, not a scale: the tile blits 1:1, so its label and icon never
    /// resample, and it reads the same on a narrow button as on a full-width row.
    pub fn rect(self, base: Rect) -> Rect {
        base.offset(0, (PRESS_DROP * (1.0 - anim_frac(self.0, PRESS_POP))) as i32)
    }
}

/// Where a composited focus tile is drawn: the focus pop's zoom with any press dip on
/// top. Every focus tile goes through this, so the two motions always compose alike.
pub fn focus_tile_rect(base: Rect, focus_anim: Option<Instant>, press: Press) -> Rect {
    press.rect(zoom_rect(base, anim_frac(focus_anim, FOCUS_POP), FOCUS_GROWTH))
}

/// How far a modal card slides down as it fades out (and up as it fades in), in px.
const MODAL_RISE: f32 = 26.0;

/// How far a modal layer is still offset at fade progress `p` — the rise the entering card,
/// the closing snapshot and the card's own frost pane all ride. Shared so the `App`'s
/// `Screen` modals and the runtime-side confirm dialogs (which have no `App`) travel the
/// same distance on the same curve.
pub fn modal_rise(p: f32) -> i32 {
    ((1.0 - p) * MODAL_RISE) as i32
}

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
    // The clock is read only when there is an animation to measure — most calls, on most
    // frames, are the `None` arm.
    match anim {
        Some(_) => frac_at(anim, dur, Instant::now(), curve),
        None => 1.0,
    }
}

/// [`frac`] against a clock the caller already read. Per-element loops (the grid's visible
/// cards) take one `Instant::now()` for the frame rather than one per element per curve.
fn frac_at(anim: Option<Instant>, dur: Duration, now: Instant, curve: impl Fn(f32) -> f32) -> f32 {
    match anim {
        // Saturating: a clock armed in the future (the reveal wave's stagger) has not started.
        Some(t) => curve((now.saturating_duration_since(t).as_secs_f32() / dur.as_secs_f32()).min(1.0)),
        None => 1.0,
    }
}

/// [`anim_frac`] on a caller-held clock. See [`frac_at`].
pub fn anim_frac_at(anim: Option<Instant>, dur: Duration, now: Instant) -> f32 {
    frac_at(anim, dur, now, ease)
}

/// [`anim_frac_smooth`] on a caller-held clock. See [`frac_at`].
pub fn anim_frac_smooth_at(anim: Option<Instant>, dur: Duration, now: Instant) -> f32 {
    frac_at(anim, dur, now, smoothstep)
}

/// A staggered fade across a surface: one curve, started up to `span` later depending on how
/// far along the sweep the element sits. Whoever owns the surface decides what `progress`
/// means — a card's diagonal position in the grid, a texel's position in an image — and the
/// motion is the same either way, which is the point: the library's cards arrive on one of
/// these and the launch backdrop leaves on one, in the same direction.
///
/// Smoothstep rather than the cubic ease-out the pops use: over a long fade `1-(1-t)³` is
/// near-opaque a sixth of the way through, which lands as a pop.
#[derive(Clone, Copy)]
pub struct Wave {
    /// How much later the far end of the sweep starts than the near end.
    pub span: Duration,
    /// One element's own fade, once its turn comes.
    pub fade: Duration,
}

impl Wave {
    /// How long the element at `progress` (0 at the corner the wave starts from, 1 at the
    /// opposite one) waits before its own fade begins.
    pub fn delay(self, progress: f32) -> Duration {
        self.span.mul_f32(progress.clamp(0.0, 1.0))
    }

    /// That element's 0..=1 progress at `now`, for a wave that started at `start`.
    pub fn frac(self, start: Option<Instant>, progress: f32, now: Instant) -> f32 {
        anim_frac_smooth_at(start.map(|t| t + self.delay(progress)), self.fade, now)
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
