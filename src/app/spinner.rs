//! The grid's loading spinner: whether the grid has revealed yet, and the two clocks that
//! decide it.
//!
//! Split out of `App` because the four fields are only ever moved together and the reasons they
//! are *two* clocks rather than one are subtle enough to be worth a home of their own (see
//! [`GridReveal::restart`]).
use std::time::{Duration, Instant};

/// Loading spinner timeout: failed fetches never become ready, so cap the wait.
const SPINNER_MAX_WAIT: Duration = Duration::from_millis(900);

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
    /// Feeds the spinner's rotation phase. Runs from the *fetch*, continuously, so the rotation
    /// does not jump when the games land mid-spin.
    since: Option<Instant>,
    /// What [`SPINNER_MAX_WAIT`] is measured against, armed on the first frame the grid has a
    /// library to build from. Separate from `since` precisely because it must not run during the
    /// fetch — the deadline must not expire on time the build never got.
    build_since: Option<Instant>,
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

    /// Reveals the grid and stands the spinner down, clocks and all.
    pub fn reveal(&mut self) {
        *self = Self::revealed();
    }

    /// Stands the spinner back up for a fresh library.
    ///
    /// The build deadline always restarts — this rebuild *is* the build it times. The rotation
    /// only restarts if the spinner was not already turning: a landing fetch triggers this on the
    /// handover from waiting to building, and starting the phase over there reads as the spinner
    /// visibly jumping.
    pub fn restart(&mut self) {
        let was_spinning = !self.revealed;
        self.revealed = false;
        self.build_since = None;
        if !was_spinning {
            self.since = None;
            self.frame = None;
        }
    }

    /// Advances the spinner one frame while the grid is still building, and reveals it once
    /// `window_ready` says every card in the visible window has landed — or once the build has
    /// outrun [`SPINNER_MAX_WAIT`].
    ///
    /// `fetch_in_flight` covers the network round trip before there is anything to build: the
    /// window check would find nothing outstanding and reveal an empty grid, and the deadline
    /// must not be running either.
    ///
    /// Returns the spinner frame to upload, if it changed this tick.
    pub fn advance(&mut self, fetch_in_flight: bool, window_ready: impl FnOnce() -> bool) -> Option<usize> {
        let since = *self.since.get_or_insert_with(Instant::now);
        if !fetch_in_flight {
            let build_since = *self.build_since.get_or_insert_with(Instant::now);
            if window_ready() || build_since.elapsed() >= SPINNER_MAX_WAIT {
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
