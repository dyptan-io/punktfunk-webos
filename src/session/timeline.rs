//! Timeline plumbing for the video pump: the panel-reconciled frame interval and the
//! host-PTS → player-clock anchor NDL needs (it has no PTS clock of its own).

use crate::platform::webos::sdl_webos;

/// Returns panel refresh in Hz, or `None` on query failure/implausible values — including
/// an SDL that has no such query (`platform::webos::sdl_webos`), where the caller falls back
/// to the stream's own rate.
fn panel_refresh_hz() -> Option<u32> {
    let fns = sdl_webos::fns().ok()?;
    let mut rate: std::os::raw::c_int = 0;
    // SAFETY: single out-param, no aliasing; read-only panel query.
    let ok = unsafe { (fns.get_refresh_rate)(&mut rate) };
    (ok != 0 && (20..=240).contains(&rate)).then_some(rate as u32)
}

/// Frame interval (ns): panel cadence if within ±2 Hz of stream, else stream rate.
pub fn reconciled_frame_interval_ns(stream_hz: u32) -> u64 {
    let hz = match panel_refresh_hz() {
        Some(panel_hz) if stream_hz.abs_diff(panel_hz) <= 2 => {
            tracing::info!("frame interval anchored to panel {panel_hz}Hz (stream {stream_hz}Hz)");
            panel_hz
        }
        Some(panel_hz) => {
            tracing::info!("frame interval on stream {stream_hz}Hz (panel {panel_hz}Hz differs by >2Hz)");
            stream_hz
        }
        None => stream_hz,
    };
    1_000_000_000 / u64::from(hz.max(1))
}

/// Frames the cadence loop folds before the audio plane may latch its mapping
/// ([`CadencePacer::ready_for_audio`]). Half a second at 60 Hz — long enough for the offset
/// estimate to leave its cold-start sample behind, short enough that offloaded audio is not
/// audibly missing at session start. The trim path's own gate is [`TRIM_SETTLE_NS`], which is a
/// different quantity and cannot be shared: that one waits for trimming to STOP, and this loop
/// never stops moving.
const PACER_AUDIO_LATCH_FRAMES: u64 = 30;

/// Plays frames out on the host's own cadence instead of on their arrival instant, by stamping
/// them from [`punktfunk_core::phase::CadenceClock`] rather than from a fixed anchor.
///
/// **Why this is a different shape from [`HostPtsAnchor`], not a tweak to it.** The anchor is a
/// constant taken from frame 0 plus a one-off trim: it has no rate term (two free-running crystals
/// produce a ramp, so the session's real lead walks away over minutes with nothing to pull it
/// back), and its jitter margin is whatever [`TRIM_KEEP_NS`] happens to be — 4 ms, chosen for
/// latency, and below the arrival spread of an ordinary link. Every frame arriving later than that
/// margin is stamped in the player clock's past, which NDL answers by presenting it at feed
/// cadence: the judder. `CadenceClock` is a type-2 loop over the same quantity (`ready − pts`) and
/// sizes its cushion from the measured mean absolute deviation, capped at one frame interval.
///
/// **It smooths the OFFSET, never the timestamps** — that is core's invariant, tested there by
/// `preserves_source_cadence`, and it is the property that makes this honest: a game genuinely
/// rendering at an irregular rate still looks exactly as irregular as it is. What is removed is
/// the transport's contribution, not the source's.
///
/// The cost is the cushion, so this is opt-in: `Settings::smooth_playback`, gating every
/// latency-adding measure on this path (see `SinkConfig::smooth_playback`). The loop still runs
/// when the setting is off — its health line is what says whether turning it on is worth it — but
/// its output is discarded and the anchor above keeps stamping.
pub struct CadencePacer {
    clock: punktfunk_core::phase::CadenceClock,
    /// Nominal source interval, and the cushion's ceiling (see `CadenceClock::cushion_ns`).
    frame_interval_ns: i64,
    /// Last stamp handed out this run. NDL reads a stamp going backwards as a rewind and answers
    /// by muting the session for good, and the cushion CAN shrink between frames, so the sequence
    /// is clamped monotonic — exactly as [`HostPtsAnchor::map`] does, and reset with the run for
    /// the same reason (a flush restarts the timeline).
    last_base_ns: u64,
    /// Frames folded since the last [`Self::reset`], for [`Self::ready_for_audio`]. Not read off
    /// `CadenceHealth::frames`, which counts the whole session.
    frames_this_run: u64,
    /// Frames whose stamp was already behind the player clock when it was fed — the direct
    /// "the cushion is too small" signal, counted for whichever mapping is actually in use rather
    /// than for the loop, so it is comparable across the setting.
    late_stamps: u64,
}

impl CadencePacer {
    pub fn new(frame_interval_ns: u64) -> Self {
        Self {
            // `snapping`, not `free_running`: NDL presents on the panel's own grid, so the snap-up
            // to the next latch already carries roughly half a refresh of implicit slack and the
            // cushion does not have to cover the distribution alone.
            clock: punktfunk_core::phase::CadenceClock::new(punktfunk_core::phase::CadenceTuning::snapping()),
            frame_interval_ns: i64::try_from(frame_interval_ns).unwrap_or(i64::MAX),
            last_base_ns: 0,
            frames_this_run: 0,
            late_stamps: 0,
        }
    }

    /// Fold one frame and return its stamp in the player's clock domain. `player_clock_ns` is when
    /// the frame became presentable, i.e. now — the loop is domain-agnostic, so the constant
    /// between the host's capture clock and NDL's player clock is simply absorbed by the offset
    /// estimate and there is no conversion anywhere in this path.
    pub fn map(&mut self, host_pts_ns: u64, player_clock_ns: u64) -> u64 {
        self.frames_this_run += 1;
        let ready = i64::try_from(player_clock_ns).unwrap_or(i64::MAX);
        let due = self.clock.due_ns(host_pts_ns, ready, self.frame_interval_ns);
        // A due time in the past is a late frame and core's contract is "present at the next
        // opportunity" — which is what handing NDL a stamp at or behind its clock already means.
        let base = u64::try_from(due).unwrap_or(0).max(self.last_base_ns);
        self.last_base_ns = base;
        base
    }

    /// Count one fed frame whose stamp had already passed. Called with the stamp actually used, so
    /// the figure describes the session the user is watching and not the shadow loop.
    pub fn note_stamp(&mut self, base_ns: u64, player_clock_ns: u64) {
        if base_ns <= player_clock_ns {
            self.late_stamps += 1;
        }
    }

    /// Drop the run: the timeline jumped (a freeze-until-reanchor hold), so the offset estimate no
    /// longer describes anything. The loop keeps its jitter estimate on purpose — that describes
    /// the link, not the stream, and a cushion collapsing to its floor after every recovery would
    /// spend the next few hundred frames presenting late.
    pub fn reset(&mut self) {
        self.clock.reset();
        self.frames_this_run = 0;
        // Cleared with the run, like the anchor's own: NDL has been flushed, so the stamps that
        // came before it are no longer a floor this run has to clear.
        self.last_base_ns = 0;
    }

    /// Whether the audio plane may latch this mapping — see [`PACER_AUDIO_LATCH_FRAMES`].
    pub fn ready_for_audio(&self) -> bool {
        self.frames_this_run >= PACER_AUDIO_LATCH_FRAMES
    }

    /// `(jitter_ns, cushion_ns, late_due, reanchors, late_stamps)` for the video heartbeat: the
    /// loop's own health plus the one figure it cannot know (see [`Self::note_stamp`]).
    pub fn health(&self) -> (i64, i64, u64, u64, u64) {
        let h = self.clock.health();
        (h.jitter_ns, h.cushion_ns, h.late, h.reanchors, self.late_stamps)
    }
}

/// Window the lead minimum is taken over (see [`HostPtsAnchor::observe_lead`]). Short enough
/// that several windows fit inside [`TRIM_SETTLE_NS`], long enough (30 frames at 60 Hz) that a
/// single early arrival can't be mistaken for the standing floor.
const TRIM_WINDOW_NS: u64 = 500_000_000;
/// How long after an anchor trimming is allowed — and therefore how long the audio plane waits
/// before it latches this mapping ([`HostPtsAnchor::ready_for_audio`]).
///
/// One [`TRIM_WINDOW_NS`] plus slack, not the several seconds the measurement alone would like.
/// The reason is the audio plane: its stamps ride this mapping's offset and can only ever move
/// FORWARD (a rewind mutes the session for good), so a trim taken after audio has latched lands as
/// lip-sync error instead of latency saved. Trimming therefore has to be finished before audio
/// joins — and audio joining ~600 ms into a session costs nothing, because `run_clock_plane` is
/// pacing the picture through exactly that window anyway.
const TRIM_SETTLE_NS: u64 = 600_000_000;
/// Lead deliberately left in place. Trimming to exactly zero puts every frame's stamp at or
/// behind the player clock, which is where NDL presents at feed cadence — the very thing the
/// audio plane's pacing exists to avoid. One 240 Hz frame of slack keeps the stamps ahead
/// without being a queue.
const TRIM_KEEP_NS: u64 = 4_000_000;
/// Smallest trim worth taking. Below this the correction is inside the jitter it was measured
/// through, and each step costs a log line.
const TRIM_MIN_STEP_NS: u64 = 4_000_000;
/// Fraction of one frame interval the trim may pay off per frame. The stamps this mapping hands
/// NDL must never go backwards, and `raw` only advances by one frame interval per frame — so a
/// debt taken in one step would emit a stamp *behind* its predecessor. Paid off at a quarter of
/// the interval instead: the frame spacing tightens by 25% until the debt clears (a 40 ms debt is
/// ~10 frames of it) and the sequence stays strictly increasing throughout.
const TRIM_RAMP_DIVISOR: u64 = 4;

/// Maps the host's capture-clock PTS onto NDL's own player clock, anchored once at the
/// first frame of a run: `base = player_anchor + (host_pts - host_anchor)`. Keeps the
/// video and offloaded audio on one shared mapping (see
/// [`crate::session::stage::VideoStage`]). Same anchoring as SS4S's
/// `ndl_player.c::SS4S_NDL_webOS5_NextVideoPts`. Reset after a freeze-until-reanchor hold,
/// where the timeline jumps.
///
/// **Plus a lead trim, which is where the latency is.** The anchor bakes frame 0's own delivery
/// latency into the mapping: every later frame that arrives faster than frame 0 did gets a stamp
/// in NDL's *future*, and `pauseAtDecodeTime` holds it there. Frame 0 is the session's first
/// keyframe, arriving behind the connect handshake and (on Automatic) the ABR capacity probe —
/// i.e. very likely the worst-latency frame of the whole run, and the lead it leaves is a
/// standing cost for as long as the anchor lives. SS4S sidesteps this by stamping arrival time
/// (`now - mediaLoadedTime`), which never holds a frame but also throws away the relative
/// spacing NDL paces on. This keeps the spacing and removes the constant: the minimum lead over
/// a window is, by definition, slack no frame in that window needed, so it is subtracted.
#[derive(Default)]
pub struct HostPtsAnchor {
    /// `(host_pts_ns, player_clock_ns)` of the frame the current run anchored on.
    anchor: Option<(u64, u64)>,
    /// Whether [`Self::map`] is allowed to trim (see [`Self::new`]). Re-armed on every
    /// [`Self::reset`] — each run's anchor bakes in its own lead.
    trim: bool,
    /// Whether any run of this session has finished trimming. Survives [`Self::reset`], and is
    /// what lets a re-anchor latch audio immediately instead of muting it for another
    /// [`TRIM_SETTLE_NS`] — see [`Self::ready_for_audio`].
    settled_once: bool,
    /// Trim actually applied so far, ramped toward `trim_target_ns` (see [`TRIM_RAMP_DIVISOR`]).
    trim_ns: u64,
    /// Trim the windows have asked for. `trim_ns` catches up to it over the following frames.
    trim_target_ns: u64,
    /// Per-frame ramp step: a quarter of the reconciled frame interval.
    ramp_ns: u64,
    /// Player clock at the start of the current measurement window, and the smallest lead seen
    /// inside it. `None` until the first mapped frame after an anchor.
    window: Option<(u64, u64)>,
    /// Player clock at the most recent [`Self::map`], so [`Self::ready_for_audio`] can answer
    /// without a clock read of its own.
    last_player_ns: u64,
    /// Last base this run handed out. The ramp is sized against the frame interval, so a delivery
    /// whose host PTS did not advance by one (a repeated stamp, a variable-rate source) would
    /// otherwise subtract more trim than `raw` gained and emit a stamp BEHIND its predecessor —
    /// which NDL reads as a rewind and answers by muting the session for good.
    last_base_ns: u64,
}

impl HostPtsAnchor {
    /// The trim is always armed; what keeps it compatible with audio on NDL's plane is
    /// [`TRIM_SETTLE_NS`] plus [`Self::ready_for_audio`], not a per-route switch.
    pub fn new(frame_interval_ns: u64) -> Self {
        Self {
            trim: true,
            ramp_ns: (frame_interval_ns / TRIM_RAMP_DIVISOR).max(1),
            ..Self::default()
        }
    }

    /// Whether this mapping has stopped moving, i.e. whether NDL's audio plane may latch it.
    ///
    /// `false` for the first [`TRIM_SETTLE_NS`] of a run: audio latched inside that window would
    /// be anchored to a timeline the trim is still pulling earlier, and it cannot follow. The audio
    /// pump drops its packets until this turns true and says so in the log
    /// (`NdlVideo::play_audio`), while the clock plane keeps the picture paced.
    ///
    /// **Only the session's FIRST run waits.** Once a run has settled, every later one is ready on
    /// its first mapped frame — see [`Self::settled_once`]. Making each run wait afresh cost
    /// [`TRIM_SETTLE_NS`] of *silence* after every freeze-until-reanchor recovery, and a loss event
    /// is exactly when the user is least willing to lose the sound too. What the early latch pays
    /// instead is the trim that run goes on to take, landing as lip-sync error rather than latency
    /// saved — bounded by one window's standing lead, i.e. tens of ms against 600 of dropout.
    pub fn ready_for_audio(&self) -> bool {
        let Some((_, anchor_player_ns)) = self.anchor else {
            return false;
        };
        if self.settled_once {
            return true;
        }
        self.trim_ns == self.trim_target_ns && self.last_player_ns.saturating_sub(anchor_player_ns) >= TRIM_SETTLE_NS
    }

    /// Drop the mapping — the timeline jumped and nothing derived from it holds. Trimming re-arms
    /// with it: the new anchor bakes in its own delivery latency (a recovery keyframe arriving late
    /// behind a loss burst is a bad one to inherit), so the lead has to be measured again.
    /// [`Self::settled_once`] is what does NOT reset.
    pub fn reset(&mut self) {
        self.anchor = None;
        self.trim = true;
        self.trim_ns = 0;
        self.trim_target_ns = 0;
        self.window = None;
        self.last_base_ns = 0;
    }

    /// Base reference for `host_pts_ns`. First call anchors on `player_clock_ns` and
    /// returns it verbatim; later calls project the host PTS delta forward, floored at 0
    /// (a host PTS going backwards vs. the anchor would otherwise underflow), less whatever
    /// standing lead has been trimmed off the run.
    pub fn map(&mut self, host_pts_ns: u64, player_clock_ns: u64) -> u64 {
        self.last_player_ns = player_clock_ns;
        let Some((host0, player0)) = self.anchor else {
            self.anchor = Some((host_pts_ns, player_clock_ns));
            self.window = Some((player_clock_ns, u64::MAX));
            self.last_base_ns = player_clock_ns;
            return player_clock_ns;
        };
        self.trim_ns = self.trim_target_ns.min(self.trim_ns + self.ramp_ns);
        let delta = host_pts_ns as i64 - host0 as i64;
        let raw = (player0 as i64 + delta).max(0) as u64;
        let base = raw.saturating_sub(self.trim_ns).max(self.last_base_ns);
        self.last_base_ns = base;
        if self.trim {
            self.observe_lead(base.saturating_sub(player_clock_ns), player_clock_ns, player0);
        }
        base
    }

    /// Folds one frame's lead into the current window and takes the trim when the window closes.
    ///
    /// The minimum is the whole point: it is the slack the *earliest-arriving* frame of the
    /// window still had, so subtracting it cannot make any frame in that window late. Frames
    /// that arrive later than the anchor's baked-in latency read a lead of 0 and pin the window
    /// to no trim at all, which is the correct answer — that session has no standing lead to
    /// give back.
    fn observe_lead(&mut self, lead_ns: u64, player_clock_ns: u64, anchor_player_ns: u64) {
        // A window measured while the ramp still owes trim would count the same slack twice —
        // the debt is real but not yet subtracted, so it still reads as lead.
        if self.trim_ns != self.trim_target_ns {
            self.window = Some((player_clock_ns, u64::MAX));
            return;
        }
        let Some((started, min_lead)) = self.window else {
            self.window = Some((player_clock_ns, lead_ns));
            return;
        };
        let min_lead = min_lead.min(lead_ns);
        if player_clock_ns.saturating_sub(started) < TRIM_WINDOW_NS {
            self.window = Some((started, min_lead));
            return;
        }
        // Past the settle window the anchor's own lead has long since been measured; a lead
        // appearing now belongs to the host timeline and is not ours to remove. Windows stop
        // being collected with it, so the steady state is one subtraction per frame.
        if player_clock_ns.saturating_sub(anchor_player_ns) >= TRIM_SETTLE_NS {
            self.trim = false;
            self.settled_once = true;
            tracing::debug!("pts lead: settled with {:.1}ms trimmed", ms(self.trim_ns));
            return;
        }
        self.window = Some((player_clock_ns, u64::MAX));
        let step = min_lead.saturating_sub(TRIM_KEEP_NS);
        if step < TRIM_MIN_STEP_NS {
            return;
        }
        self.trim_target_ns += step;
        // INFO, once or twice a session: this is the one line that says how much standing
        // decoder-hold latency the session started with, which is not observable any other way.
        tracing::info!(
            "pts lead: trimming {:.1}ms (window min {:.1}ms, {:.1}ms total)",
            ms(step),
            ms(min_lead),
            ms(self.trim_target_ns),
        );
    }

    /// Standing lead removed from this run's mapping, for the session log.
    pub fn trimmed_ns(&self) -> u64 {
        self.trim_target_ns
    }
}

/// Nanoseconds as milliseconds, for log lines.
pub(super) fn ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame every 16.6 ms of host PTS, arriving `arrival_lag_ns` after the anchor's own lag.
    fn run(anchor_lag_ns: u64, later_lag_ns: u64, frames: u32) -> HostPtsAnchor {
        let interval = 16_666_666u64;
        let mut a = HostPtsAnchor::new(interval);
        for i in 0..frames {
            let host_pts = u64::from(i) * interval;
            let lag = if i == 0 { anchor_lag_ns } else { later_lag_ns };
            a.map(host_pts, host_pts + lag);
        }
        a
    }

    #[test]
    fn trims_the_lead_the_anchor_baked_in() {
        // Frame 0 arrived 50 ms late, everything after it 10 ms late: 40 ms of pure hold.
        let a = run(50_000_000, 10_000_000, 400);
        let trimmed = a.trimmed_ns();
        assert!(trimmed > 30_000_000 && trimmed < 40_000_000, "trimmed {trimmed}ns");
    }

    #[test]
    fn leaves_a_session_with_no_standing_lead_alone() {
        assert_eq!(run(10_000_000, 10_000_000, 400).trimmed_ns(), 0);
    }

    #[test]
    fn holds_the_audio_latch_until_the_trim_has_settled() {
        let interval = 16_666_666u64;
        let mut a = HostPtsAnchor::new(interval);
        assert!(!a.ready_for_audio(), "no anchor yet");
        // Frame 0 anchors 50 ms late; the rest arrive 10 ms late, so there is a trim to take.
        for i in 0..12u64 {
            let host_pts = i * interval;
            a.map(host_pts, host_pts + if i == 0 { 50_000_000 } else { 10_000_000 });
        }
        assert!(!a.ready_for_audio(), "still inside the settle window");
        for i in 12..80u64 {
            let host_pts = i * interval;
            a.map(host_pts, host_pts + 10_000_000);
        }
        assert!(a.ready_for_audio());
        assert!(a.trimmed_ns() > 0);
    }

    /// A freeze-until-reanchor reset must not re-open the settle window: `play_audio` drops every
    /// packet while the mapping is unlatched, so re-arming the wait is 600 ms of silence on every
    /// loss recovery. Planting the defect (re-arming `settled_once` in `reset`) fails this.
    #[test]
    fn a_re_anchor_does_not_mute_audio_for_another_settle_window() {
        let mut a = run(50_000_000, 10_000_000, 400);
        assert!(a.ready_for_audio(), "the first run should have settled");
        a.reset();
        assert!(!a.ready_for_audio(), "no anchor yet");
        // The recovery keyframe anchors 50 ms late, as a frame behind a loss burst does.
        a.map(0, 1_050_000_000);
        assert!(a.ready_for_audio(), "the re-anchor must be ready on its first frame");
        // …and the new run still trims: that keyframe's own lead is not inherited for the session.
        for i in 1..400u64 {
            let host_pts = i * 16_666_666;
            a.map(host_pts, 1_010_000_000 + host_pts);
        }
        assert!(a.trimmed_ns() > 0, "the re-anchored run took no trim");
    }

    /// The ramp is sized against the frame interval, so anything that stalls `raw` WHILE the trim
    /// is still being paid off emits a stamp behind its predecessor. NDL reads that as a rewind and
    /// mutes the session for the rest of the run, so it must be impossible, not merely unlikely.
    ///
    /// The repeat has to land mid-ramp to bite: once `trim_ns` reaches its target it stops moving
    /// and a repeated PTS merely re-emits the same stamp. Proven non-vacuous by planting the defect
    /// (drop the `.max(last_base_ns)` in `map`), which fails this with a stamp ~4 ms behind.
    #[test]
    fn a_repeated_host_pts_mid_ramp_cannot_walk_the_stamp_backwards() {
        let interval = 16_666_666u64;
        let mut a = HostPtsAnchor::new(interval);
        a.map(0, 80_000_000);
        // Frame 0 anchored 75 ms late, so there is a large trim to ramp off. Feed clean frames
        // until the first window closes and the ramp starts owing.
        let mut i = 1u64;
        while a.trimmed_ns() == 0 {
            a.map(i * interval, i * interval + 5_000_000);
            i += 1;
            assert!(i < 1_000, "no trim was ever taken");
        }
        // The ramp now owes `trimmed_ns()` and pays it at a quarter interval per frame. A delivery
        // whose host PTS does not advance gains nothing in `raw` to cover that quarter.
        let stuck = i * interval;
        let mut last = a.map(stuck, stuck + 5_000_000);
        for repeat in 0..8u64 {
            let base = a.map(stuck, stuck + 5_000_000 + repeat);
            assert!(base >= last, "repeat {repeat}: {base} < {last}");
            last = base;
        }
    }

    /// The cadence loop's cushion moves with the measured jitter, and a repeated host PTS gains
    /// nothing to cover a shrinking one — either way the stamp would step BACKWARDS, which NDL
    /// reads as a rewind and answers by muting the session for the rest of the run. Proven
    /// non-vacuous by planting the defect (dropping the `.max(last_base_ns)` in
    /// `CadencePacer::map`), which fails this.
    #[test]
    fn the_pacer_never_walks_a_stamp_backwards_within_a_run() {
        let interval = 16_666_666u64;
        let mut p = CadencePacer::new(interval);
        let mut last = 0;
        // A jittery opening (which builds a cushion), then a dead-calm tail (which shrinks it),
        // with a repeated stamp thrown in where the shrink is fastest.
        for i in 0..600u64 {
            let host_pts = i * interval;
            let jitter = if i < 200 { (i % 7) * 2_000_000 } else { 0 };
            let base = p.map(host_pts, host_pts + 10_000_000 + jitter);
            assert!(base >= last, "frame {i}: {base} < {last}");
            last = base;
            if i == 200 {
                let repeat = p.map(host_pts, host_pts + 10_000_000);
                assert!(repeat >= last, "repeat: {repeat} < {last}");
                last = repeat;
            }
        }
    }

    #[test]
    fn mapping_stays_monotonic_across_a_trim() {
        let mut a = HostPtsAnchor::new(16_666_666);
        let mut last = 0;
        for i in 0..600u64 {
            let host_pts = i * 16_666_666;
            let lag = if i == 0 { 80_000_000 } else { 5_000_000 };
            let base = a.map(host_pts, host_pts + lag);
            assert!(base >= last, "frame {i}: {base} < {last}");
            last = base;
        }
        assert!(a.trimmed_ns() > 0);
    }
}
