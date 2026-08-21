//! Shared open/close fade bookkeeping for modal-like overlays — the pre-stream `App`'s
//! `Screen` modals and the in-stream disconnect dialog both use one of these so every
//! dialog in the app opens/closes on the same clock and curve instead of each re-deriving
//! it (see `docs/NOTES.md` for why that drifted apart once: the disconnect dialog and
//! stats overlay live in `main.rs`'s streaming loop, which has no `App`/`Screen` to hook).

use crate::ui::animation::{anim_frac, anim_frac_in};
use std::time::{Duration, Instant};

/// Shared fade-in/out duration for transient in-stream overlays (toast notifications,
/// the stats overlay, the log-tail overlay) — one curve so toggling any of them feels
/// the same.
pub const OVERLAY_FADE: Duration = Duration::from_millis(400);

/// `T` is whatever the caller needs preserved while closing (e.g. which screen was
/// open) so the fade-out can keep rendering it after the live state has already moved
/// on — use `()` if there's only ever one thing this could be.
pub struct ModalFade<T = ()> {
    open_since: Option<Instant>,
    closing: Option<(Instant, T)>,
    /// Whether the close in flight has something opening behind it — see [`Self::close_cross`].
    cross: bool,
}

impl<T: Copy + PartialEq> Default for ModalFade<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq> ModalFade<T> {
    pub fn new() -> Self {
        Self {
            open_since: None,
            closing: None,
            cross: false,
        }
    }

    /// Starts (or restarts) the open fade. Leaves an in-flight close alone.
    pub fn open(&mut self) {
        self.open_since = Some(Instant::now());
    }

    /// `open`, but cancels any in-flight close (for single-overlay callers).
    pub fn reopen(&mut self) {
        self.open();
        self.closing = None;
    }

    /// Starts the close fade, carrying `payload` for [`Self::closing_frame`] to hand back.
    pub fn close(&mut self, payload: T) {
        self.closing = Some((Instant::now(), payload));
        self.cross = false;
    }

    /// [`Self::close`] for an overlay with another one opening behind it: the two are made
    /// each other's exact inverse (see [`Self::closing_frame_against`]) on one clock
    /// ([`Self::close_dur`]).
    ///
    /// Their own curves do not compose — an ease-out closing against an ease-in opening sums
    /// to a quarter at the halfway point, and whatever is behind both shows through the seam.
    pub fn close_cross(&mut self, payload: T) {
        self.close(payload);
        self.cross = true;
    }

    /// How long the close in flight runs: a cross-fade matches the open exactly, a lone close
    /// takes `solo`'s slower dissolve — there is nothing arriving to hide it.
    pub fn close_dur(&self, open: Duration, solo: Duration) -> Duration {
        if self.cross {
            open
        } else {
            solo
        }
    }

    /// Re-stamps whichever fades are in flight to now — for a caller that does expensive
    /// work between starting a fade and the first frame that can show it (rasterizing a
    /// modal outlasts `MODAL_FADE`, so the clock is spent before there are pixels and the
    /// card snaps in opaque). Both clocks move together, keeping a cross-fade in step.
    pub fn restart(&mut self) {
        let now = Instant::now();
        if self.open_since.is_some() {
            self.open_since = Some(now);
        }
        if let Some((t, _)) = self.closing.as_mut() {
            *t = now;
        }
    }

    /// Cancels an in-flight close only if it's fading out `payload`.
    pub fn cancel_closing(&mut self, payload: T) {
        if self.closing.is_some_and(|(_, p)| p == payload) {
            self.closing = None;
        }
    }

    /// Returns `(alpha, payload)` while a close is in flight; `None` otherwise.
    pub fn closing_frame(&self, dur: Duration) -> Option<(f32, T)> {
        let (t, payload) = self.closing.filter(|(t, _)| t.elapsed() < dur)?;
        Some((1.0 - anim_frac(Some(t), dur), payload))
    }

    /// [`Self::closing_frame`] for the one caller that draws a cross-fade: `open` is the alpha
    /// of whatever is opening this frame, and a cross-fade's closing overlay is its inverse
    /// rather than its own ease, so the pair sums to one at every instant.
    pub fn closing_frame_against(&self, dur: Duration, open: f32) -> Option<(f32, T)> {
        match self.closing_frame(dur)? {
            (_, payload) if self.cross => Some((1.0 - open, payload)),
            frame => Some(frame),
        }
    }

    /// Whether a close is in flight, for callers that need its presence but not its alpha.
    pub fn is_closing(&self, dur: Duration) -> bool {
        self.closing.is_some_and(|(t, _)| t.elapsed() < dur)
    }

    /// Open-fade alpha: eases 0.0 -> 1.0, `1.0` once finished or if never opened.
    /// Ease-*in*, so it is `closing_frame`'s curve played backwards.
    pub fn open_alpha(&self, dur: Duration) -> f32 {
        anim_frac_in(self.open_since, dur)
    }

    /// Alpha for a simple show/hide overlay driven by `shown`: `Some` through the close
    /// fade even after `shown` has already flipped to `false` (so the last frame doesn't
    /// cut instantly), and `None` once fully faded and hidden.
    pub fn visibility_alpha(&self, dur: Duration, shown: bool) -> Option<f32> {
        if let Some((alpha, _)) = self.closing_frame(dur) {
            return Some(alpha);
        }
        shown.then(|| self.open_alpha(dur))
    }

    /// Whether an open or close fade is still mid-flight (i.e. hasn't yet reached its
    /// steady state). Pure and non-mutating, unlike `tick` — safe to call just to pick a
    /// redraw cadence.
    pub fn is_animating(&self, dur: Duration) -> bool {
        self.open_since.is_some_and(|t| t.elapsed() < dur) || self.closing.is_some_and(|(t, _)| t.elapsed() < dur)
    }

    /// Advances the clock; returns whether either fade is still in flight.
    pub fn tick(&mut self, dur: Duration) -> bool {
        self.tick_split(dur, dur)
    }

    /// [`tick`](Self::tick) for a caller whose two directions run at different speeds —
    /// each clock clears on its own duration, or the shorter keeps asking for dead redraws.
    pub fn tick_split(&mut self, open_dur: Duration, close_dur: Duration) -> bool {
        let mut animating = false;
        if let Some(t) = self.open_since {
            if t.elapsed() >= open_dur {
                self.open_since = None;
            }
            animating = true;
        }
        if let Some((t, _)) = self.closing {
            if t.elapsed() >= close_dur {
                self.closing = None;
            }
            animating = true;
        }
        animating
    }
}
