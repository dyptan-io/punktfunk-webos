//! Timeline plumbing for the video pump: the host-PTS → player-clock mapping NDL needs (it has no
//! PTS clock of its own).
//!
//! One mapping, [`Pacing`]. The fixed anchor it replaced stamped from a constant taken at frame 0
//! and had no rate term at all; see [`Pacing`] for why that shape could not be repaired in place.

/// Plays frames out on the host's own cadence instead of on their arrival instant, by stamping
/// them from [`punktfunk_core::phase::CadenceClock`] rather than from a fixed anchor.
///
/// **Why this is a different shape from the fixed anchor it replaced, not a tweak to it.** That
/// anchor was a constant taken from frame 0 plus a one-off trim: it had no rate term (two
/// free-running crystals produce a ramp, so the session's real lead walks away over minutes with
/// nothing to pull it back), and its jitter margin was the 4 ms of lead its trim deliberately left
/// behind — chosen for latency, and below the arrival spread of an ordinary link. Every frame arriving later than that
/// margin is stamped in the player clock's past, which NDL answers by presenting it at feed
/// cadence: the judder. `CadenceClock` is a type-2 loop over the same quantity (`ready − pts`) and
/// sizes its cushion from the measured mean absolute deviation, capped at one frame interval.
///
/// **It smooths the OFFSET, never the timestamps** — that is core's invariant, tested there by
/// `preserves_source_cadence`, and it is the property that makes this honest: a game genuinely
/// rendering at an irregular rate still looks exactly as irregular as it is. What is removed is
/// the transport's contribution, not the source's.
///
/// **The only mapping**, because the cushion it pays is measured rather than chosen: on a link
/// with nothing wrong it collapses to its 0.5 ms floor and costs essentially nothing, and it only
/// grows where there is real jitter to cover. The anchor was kept selectable for a while as
/// `Settings::direct_playback`; on real hardware it stamped ~17% of frames late at 120 Hz against
/// the loop's ~7%, so it is gone.
pub struct Pacing {
    clock: punktfunk_core::phase::CadenceClock,
    /// The source's nominal interval, and the cushion's ceiling (see `CadenceClock::cushion_ns`)
    /// — see [`Self::new`] for why it is not the panel's.
    source_interval_ns: i64,
    /// Last stamp handed out this run. NDL reads a stamp going backwards as a rewind and answers
    /// by muting the session for good, and the cushion CAN shrink between frames, so the sequence
    /// is clamped monotonic, and reset with the run (a flush restarts the timeline).
    last_base_ns: u64,
    /// Exact repeated source stamps are off-cadence, not estimator observations.
    last_host_pts_ns: Option<u64>,
    /// Frames fed with a stamp already behind the player clock — see [`PacingHealth::late_stamps`].
    /// Counted around the loop rather than inside it: it is a property of the stamp handed to NDL,
    /// not of the estimate that produced it.
    late_stamps: u64,
}

impl Pacing {
    /// `source_interval_ns` is the negotiated STREAM mode's frame interval — the cadence the host
    /// produces — and never the panel's. It is the cushion's ceiling, so a panel period would let a
    /// stream running faster than the panel be held for longer than its own cadence justifies.
    pub fn new(source_interval_ns: u64) -> Self {
        Self {
            // `snapping`, not `free_running`: NDL presents on the panel's own grid, so the snap-up
            // to the next latch already carries roughly half a refresh of implicit slack and the
            // cushion does not have to cover the distribution alone.
            clock: punktfunk_core::phase::CadenceClock::new(punktfunk_core::phase::CadenceTuning::snapping()),
            source_interval_ns: i64::try_from(source_interval_ns).unwrap_or(i64::MAX),
            last_base_ns: 0,
            last_host_pts_ns: None,
            late_stamps: 0,
        }
    }

    /// Fold one frame and return its stamp in the player's clock domain. `player_clock_ns` is when
    /// the frame became presentable, i.e. now — the loop is domain-agnostic, so the constant
    /// between the host's capture clock and NDL's player clock is simply absorbed by the offset
    /// estimate and there is no conversion anywhere in this path.
    pub fn map(&mut self, host_pts_ns: u64, player_clock_ns: u64) -> u64 {
        let ready = i64::try_from(player_clock_ns).unwrap_or(i64::MAX);
        let repeated = self.last_host_pts_ns == Some(host_pts_ns);
        let due = if repeated {
            self.clock.note_off_cadence(ready, self.source_interval_ns)
        } else {
            self.clock.due_ns(host_pts_ns, ready, self.source_interval_ns)
        };
        self.last_host_pts_ns = Some(host_pts_ns);
        // A due time in the past is a late frame and core's contract is "present at the next
        // opportunity" — which is what handing NDL a stamp at or behind its clock already means.
        let base = u64::try_from(due).unwrap_or(0).max(self.last_base_ns);
        self.last_base_ns = base;
        if base <= player_clock_ns {
            self.late_stamps += 1;
        }
        base
    }

    /// Drop the run: the timeline jumped (a freeze-until-reanchor hold), so the offset estimate no
    /// longer describes anything. The loop keeps its jitter estimate on purpose — that describes
    /// the link, not the stream, and a cushion collapsing to its floor after every recovery would
    /// spend the next few hundred frames presenting late.
    pub fn reset(&mut self) {
        self.clock.reset();
        self.last_host_pts_ns = None;
        // `last_base_ns` deliberately SURVIVES the reset. It used to be cleared here because the
        // loss hold flushed NDL, which made the previous run's stamps irrelevant — the hold no
        // longer flushes (`VideoStage::gate`), so the pipeline still holds everything fed before
        // it and a run restarting from 0 would walk the video stamp backwards. NDL answers a
        // rewind by muting, which is the failure this whole path exists to avoid.
    }

    /// What the mapping has to say for itself — see [`PacingHealth`].
    pub fn health(&self) -> PacingHealth {
        let h = self.clock.health();
        PacingHealth {
            jitter_ns: h.jitter_ns,
            cushion_ns: h.cushion_ns,
            late_stamps: self.late_stamps,
            reanchors: h.reanchors,
        }
    }
}

/// What the mapping has to say for itself, on the video heartbeat and the stats overlay.
#[derive(Clone, Copy, Default)]
pub struct PacingHealth {
    /// Measured jitter (mean absolute deviation of `ready − pts`), and what the cadence loop holds
    /// to cover it.
    pub jitter_ns: i64,
    pub cushion_ns: i64,
    /// Frames whose stamp was already behind the player clock when fed: presented at feed cadence
    /// rather than paced, which is the judder — the figure a before/after comparison rests on.
    pub late_stamps: u64,
    /// Times the mapping gave up tracking and re-anchored.
    pub reanchors: u64,
}

/// Nanoseconds as milliseconds, for log lines.
pub(super) fn ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}
