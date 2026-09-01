//! The grid's loading spinner: whether the grid has revealed yet, and the two clocks that
//! decide it. Also the dissolve that follows: the same mask-and-erase technique as the
//! launch backdrop's exit (`app::hero`), run over the card grid instead.
//!
//! Split out of `App` because the four fields are only ever moved together and the reasons they
//! are *two* clocks rather than one are subtle enough to be worth a home of their own (see
//! [`GridReveal::restart`]).
use std::time::{Duration, Instant};

/// Loading spinner timeout: cover art that never arrives (a failed fetch) would hold the
/// spinner forever, so cap the wait on it. Only on the art — see [`PageReady`].
const SPINNER_MAX_WAIT: Duration = Duration::from_millis(900);

/// Grid first-page readiness (what spinner waits for).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageReady {
    /// Card has no tile; never revealed (wave opens built cards; late arrivals pop after).
    Building,
    /// All tiles rasterized, some waiting on art.
    Tiles,
    /// Nothing outstanding.
    All,
}

/// Whether the grid's initial build for the current library has finished, and the spinner shown
/// until it has.
///
/// Revealing is one-shot per library: only a fresh library load stands it back up, and later
/// scrolling into an unbuilt row does not.
#[derive(Default)]
pub(crate) struct GridReveal {
    revealed: bool,
    /// The spinner frame currently uploaded, so an unchanged phase re-uploads nothing.
    frame: Option<usize>,
    /// Spinner rotation phase clock (runs from fetch; no jump when games land mid-spin).
    since: Option<Instant>,
    /// Build deadline (separate from since; must not run during fetch).
    build_since: Option<Instant>,
    /// Grid revealed clock; dissolve clock (None once wave done).
    dissolve_since: Option<Instant>,
    /// Buffer for `dissolve_mask`.
    mask: Vec<u8>,
}

impl GridReveal {
    /// A grid with nothing to wait for.
    pub fn revealed() -> Self {
        Self {
            revealed: true,
            ..Self::default()
        }
    }

    pub fn is_revealed(&self) -> bool {
        self.revealed
    }

    /// How long the spinner has been turning — the phase `assets::spinner_frame_at` takes.
    pub fn phase(&self) -> f32 {
        self.since.map_or(0.0, |s| s.elapsed().as_secs_f32())
    }

    /// Reveal grid, stand down spinner, start dissolve (cards already built).
    pub fn reveal(&mut self) {
        let mask = std::mem::take(&mut self.mask);
        *self = Self {
            revealed: true,
            dissolve_since: Some(Instant::now()),
            mask,
            ..Self::default()
        };
    }

    /// Stand spinner back up for fresh library. Build deadline always restarts.
    /// Rotation restarts only if not already spinning (avoids visible jump on fetch land).
    pub fn restart(&mut self) {
        let was_spinning = !self.revealed;
        self.revealed = false;
        self.build_since = None;
        self.dissolve_since = None;
        if !was_spinning {
            self.since = None;
            self.frame = None;
        }
    }

    /// Reveal dissolve still running (gates mask upload and grid cover draw).
    pub fn dissolving(&self) -> bool {
        self.dissolve_since
            .is_some_and(|t| t.elapsed() < crate::app::GRID_REVEAL_WAVE.span + crate::app::GRID_REVEAL_WAVE.fade)
    }

    /// Dissolve mask: background alpha falling opaque→zero as wave passes (cover, not erase).
    /// Unlike hero backdrop (erases to uncover video), grid composites as ordinary alpha texture
    /// because it has nothing to fall back to (cards are only content).
    pub fn dissolve_mask(&mut self, now: Instant) -> (u32, u32, &[u8]) {
        let bg = crate::ui::theme::palette().bg;
        let elapsed = self
            .dissolve_since
            .map_or(f32::MAX, |t| now.saturating_duration_since(t).as_secs_f32());
        crate::ui::animation::diagonal_mask(
            &mut self.mask,
            [bg.r, bg.g, bg.b],
            crate::app::GRID_REVEAL_WAVE,
            elapsed,
            |revealed| 1.0 - revealed,
        );
        (crate::ui::animation::MASK_W, crate::ui::animation::MASK_H, &self.mask)
    }

    /// Advance spinner, reveal when page complete (or past `SPINNER_MAX_WAIT` if tiles ready).
    /// Page only, not prefetch (wave reveals on-screen, rows below have no arrival).
    /// `fetch_in_flight` prevents reveal during network wait. Returns frame to upload if changed.
    pub fn advance(&mut self, fetch_in_flight: bool, page: impl FnOnce() -> PageReady) -> Option<usize> {
        let since = *self.since.get_or_insert_with(Instant::now);
        if !fetch_in_flight {
            let build_since = *self.build_since.get_or_insert_with(Instant::now);
            let ready = match page() {
                PageReady::All => true,
                PageReady::Tiles => build_since.elapsed() >= SPINNER_MAX_WAIT,
                PageReady::Building => false,
            };
            if ready {
                self.reveal();
                return None;
            }
        }
        let (idx, _) = crate::app::assets::spinner_frame_at(since.elapsed().as_secs_f32());
        (self.frame != Some(idx)).then(|| {
            self.frame = Some(idx);
            idx
        })
    }
}
