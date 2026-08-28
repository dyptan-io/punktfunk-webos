//! Timeline plumbing for the video pump: the panel-reconciled frame interval, and the
//! host-PTS → player-clock mapping NDL needs (it has no PTS clock of its own).
//!
//! Two mappings, picked by [`Pacing`]: the cadence loop ([`CadencePacer`], the default) and the
//! fixed anchor ([`HostPtsAnchor`], the Experimental escape hatch). Exactly one is live per
//! session — see [`Pacing`] for why they are not both folded.

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

/// Evidence before the audio plane may latch the cadence mapping.
///
/// This used to be 30 frames, which made the wait inversely proportional to the delivered frame
/// rate: 500 ms at 60 Hz became 3 seconds at 10 Hz. Wall clock is the honest quantity, so the gate
/// is eight on-cadence samples AND half a second — a floor that costs a 240 Hz stream 500 ms where
/// the frame count charged it 125, which is the right way round: a converged estimate is what the
/// audio plane is waiting on, not a frame tally.
///
/// The deadline caps the silence at a fixed wall time once ONE genuine sample has anchored the
/// estimate and a second delivery has arrived, whatever kind. A static desktop delivers one genuine
/// stamp and then repeats indefinitely; gating on genuine samples alone leaves that session with no
/// audio at all. The deadline is deliberately degraded operation: a small A/V offset is better than
/// silence.
const PACER_AUDIO_LATCH_FRAMES: u64 = 8;
const PACER_AUDIO_LATCH_NS: u64 = 500_000_000;
const PACER_AUDIO_DEADLINE_NS: u64 = 1_000_000_000;

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
/// **The default**, because the cushion it pays is measured rather than chosen: on a link with
/// nothing wrong it collapses to its 0.5 ms floor and costs essentially nothing, and it only grows
/// where there is real jitter to cover. `Settings::direct_playback` (Experimental) is the way back
/// to the anchor.
pub struct CadencePacer {
    clock: punktfunk_core::phase::CadenceClock,
    /// The source's nominal interval, and the cushion's ceiling (see `CadenceClock::cushion_ns`)
    /// — see [`Self::new`] for why it is not the panel's.
    source_interval_ns: i64,
    /// Last stamp handed out this run. NDL reads a stamp going backwards as a rewind and answers
    /// by muting the session for good, and the cushion CAN shrink between frames, so the sequence
    /// is clamped monotonic — exactly as [`HostPtsAnchor::map`] does, and reset with the run for
    /// the same reason (a flush restarts the timeline).
    last_base_ns: u64,
    /// On-cadence frames folded since the last [`Self::reset`], for [`Self::ready_for_audio`]. Not
    /// read off `CadenceHealth::frames`, which counts the whole session.
    frames_this_run: u64,
    /// Every delivery this run, repeats included — what the deadline path counts, so a repeat-only
    /// stream still reaches it.
    deliveries_this_run: u64,
    /// Player-clock start of this run's convergence window.
    run_started_ns: Option<u64>,
    /// Once a run converged the NORMAL way, recovery runs may latch on their first accepted frame.
    /// Never set from the deadline path: promoting a deliberately degraded latch would let every
    /// later run in the session skip the wait on the strength of one that never converged.
    settled_once: bool,
    /// This run reached the full evidence bar, as opposed to the deadline.
    converged_normally: bool,
    /// This run has enough evidence to latch if the decoder accepts the frame.
    convergence_ready: bool,
    /// Exact repeated source stamps are off-cadence, not estimator observations.
    last_host_pts_ns: Option<u64>,
}

impl CadencePacer {
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
            frames_this_run: 0,
            deliveries_this_run: 0,
            run_started_ns: None,
            settled_once: false,
            converged_normally: false,
            convergence_ready: false,
            last_host_pts_ns: None,
        }
    }

    /// Fold one frame and return its stamp in the player's clock domain. `player_clock_ns` is when
    /// the frame became presentable, i.e. now — the loop is domain-agnostic, so the constant
    /// between the host's capture clock and NDL's player clock is simply absorbed by the offset
    /// estimate and there is no conversion anywhere in this path.
    pub fn map(&mut self, host_pts_ns: u64, player_clock_ns: u64) -> u64 {
        let started_ns = *self.run_started_ns.get_or_insert(player_clock_ns);
        let ready = i64::try_from(player_clock_ns).unwrap_or(i64::MAX);
        let repeated = self.last_host_pts_ns == Some(host_pts_ns);
        let due = if repeated {
            self.clock.note_off_cadence(ready, self.source_interval_ns)
        } else {
            self.frames_this_run += 1;
            self.clock.due_ns(host_pts_ns, ready, self.source_interval_ns)
        };
        self.last_host_pts_ns = Some(host_pts_ns);
        self.deliveries_this_run += 1;
        let elapsed_ns = player_clock_ns.saturating_sub(started_ns);
        let normal = self.frames_this_run >= PACER_AUDIO_LATCH_FRAMES && elapsed_ns >= PACER_AUDIO_LATCH_NS;
        // One genuine sample has anchored the estimate; the second delivery may be a repeat, whose
        // stamp is that same estimate plus the cushion — see [`PACER_AUDIO_DEADLINE_NS`].
        let deadline =
            self.frames_this_run >= 1 && self.deliveries_this_run >= 2 && elapsed_ns >= PACER_AUDIO_DEADLINE_NS;
        self.converged_normally |= normal;
        self.convergence_ready |= normal || deadline;
        // A due time in the past is a late frame and core's contract is "present at the next
        // opportunity" — which is what handing NDL a stamp at or behind its clock already means.
        let base = u64::try_from(due).unwrap_or(0).max(self.last_base_ns);
        self.last_base_ns = base;
        base
    }

    /// Drop the run: the timeline jumped (a freeze-until-reanchor hold), so the offset estimate no
    /// longer describes anything. The loop keeps its jitter estimate on purpose — that describes
    /// the link, not the stream, and a cushion collapsing to its floor after every recovery would
    /// spend the next few hundred frames presenting late.
    pub fn reset(&mut self) {
        self.clock.reset();
        self.frames_this_run = 0;
        self.deliveries_this_run = 0;
        self.run_started_ns = None;
        self.converged_normally = false;
        self.convergence_ready = false;
        self.last_host_pts_ns = None;
        // `last_base_ns` deliberately SURVIVES the reset. It used to be cleared here because the
        // loss hold flushed NDL, which made the previous run's stamps irrelevant — the hold no
        // longer flushes (`VideoStage::gate`), so the pipeline still holds everything fed before
        // it and a run restarting from 0 would walk the video stamp backwards. NDL answers a
        // rewind by muting, which is the failure this whole path exists to avoid.
    }

    /// Whether an accepted completed AU may latch this mapping. A repeat may carry it: by the time
    /// either gate opens, at least one genuine sample has anchored the estimate, and a repeat is
    /// stamped from that same estimate. Refusing them instead would mute a static desktop, which
    /// delivers one genuine stamp and then repeats.
    pub fn ready_for_audio(&self) -> bool {
        self.settled_once || self.convergence_ready
    }

    /// Records that NDL accepted the frame carrying the mapping the audio plane latched — see
    /// [`Self::settled_once`], which only a normally converged run sets.
    pub fn note_audio_latched(&mut self) {
        self.settled_once |= self.converged_normally;
    }

    fn health(&self) -> punktfunk_core::phase::CadenceHealth {
        self.clock.health()
    }
}

/// What the live mapping has to say for itself, on the video heartbeat and the stats overlay.
/// One shape for both mappings, so a report reads the same whichever is in use — a field a mapping
/// has no notion of is simply `0`.
#[derive(Clone, Copy, Default)]
pub struct PacingHealth {
    /// Measured jitter (mean absolute deviation of `ready − pts`), and what the cadence loop holds
    /// to cover it. Both `0` under the anchor, which measures neither.
    pub jitter_ns: i64,
    pub cushion_ns: i64,
    /// Frames whose stamp was already behind the player clock when fed: presented at feed cadence
    /// rather than paced, which is the judder. **The one figure both mappings publish**, so it is
    /// what a before/after comparison rests on.
    pub late_stamps: u64,
    /// Times the mapping gave up tracking and re-anchored.
    pub reanchors: u64,
    /// Standing lead the anchor has trimmed off this run, in ns. `0` under the cadence loop, which
    /// has no trim — its offset estimate is the whole mechanism.
    pub trimmed_ns: u64,
}

/// The host-PTS → player-clock mapping this session stamps with.
///
/// **Only the live one is folded.** Both are stateful over the whole run, so running the idle one
/// in the shadows would cost a mapping that is never read and a second set of numbers describing a
/// session nobody is watching. What makes the two comparable is [`PacingHealth::late_stamps`],
/// computed from the stamp actually used and so present on both paths.
pub struct Pacing {
    mode: Mode,
    /// Frames fed with a stamp already behind the player clock — see [`PacingHealth::late_stamps`].
    /// Held here rather than in either mapping precisely because it must mean the same thing on
    /// both.
    late_stamps: u64,
}

enum Mode {
    /// The default — see [`CadencePacer`].
    Cadence(CadencePacer),
    /// `Settings::direct_playback`: the fixed anchor plus its one-off trim, the mapping this client
    /// shipped before the cadence loop. Kept as the escape hatch for a set where the loop
    /// misbehaves, and as the lowest-latency answer for anyone who would rather have the judder
    /// than the cushion.
    Anchor(HostPtsAnchor),
}

impl Pacing {
    /// `source_interval_ns` is the stream mode's own interval — see [`CadencePacer::new`]. The
    /// anchor's ramp is sized against the same quantity: it pays trim debt off per FRAME, and a
    /// frame is what the source, not the panel, delivers.
    pub fn new(source_interval_ns: u64, direct: bool) -> Self {
        Self {
            mode: if direct {
                Mode::Anchor(HostPtsAnchor::new(source_interval_ns))
            } else {
                Mode::Cadence(CadencePacer::new(source_interval_ns))
            },
            late_stamps: 0,
        }
    }

    /// This frame's stamp in the player's clock domain, and the late-stamp bookkeeping with it.
    pub fn map(&mut self, host_pts_ns: u64, player_clock_ns: u64) -> u64 {
        let base = match &mut self.mode {
            Mode::Cadence(p) => p.map(host_pts_ns, player_clock_ns),
            Mode::Anchor(a) => a.map(host_pts_ns, player_clock_ns),
        };
        if base <= player_clock_ns {
            self.late_stamps += 1;
        }
        base
    }

    /// Drop the run: the timeline jumped and nothing derived from it holds.
    pub fn reset(&mut self) {
        match &mut self.mode {
            Mode::Cadence(p) => p.reset(),
            Mode::Anchor(a) => a.reset(),
        }
    }

    /// Whether the audio plane may latch this mapping. The two settle on different evidence and
    /// cannot share a gate: the anchor waits for trimming to STOP, and the cadence loop never stops
    /// moving, so it waits for a converged offset estimate instead (see
    /// [`CadencePacer::ready_for_audio`]).
    pub fn ready_for_audio(&self) -> bool {
        match &self.mode {
            Mode::Cadence(p) => p.ready_for_audio(),
            Mode::Anchor(a) => a.ready_for_audio(),
        }
    }

    /// Records the accepted frame on which the shared audio mapping latched.
    pub fn note_audio_latched(&mut self) {
        if let Mode::Cadence(p) = &mut self.mode {
            p.note_audio_latched();
        }
    }

    pub fn health(&self) -> PacingHealth {
        let late_stamps = self.late_stamps;
        match &self.mode {
            Mode::Cadence(p) => {
                let h = p.health();
                PacingHealth {
                    jitter_ns: h.jitter_ns,
                    cushion_ns: h.cushion_ns,
                    late_stamps,
                    reanchors: h.reanchors,
                    trimmed_ns: 0,
                }
            }
            Mode::Anchor(a) => PacingHealth {
                late_stamps,
                trimmed_ns: a.trimmed_ns(),
                ..PacingHealth::default()
            },
        }
    }

    /// Which mapping is live, for the log line and the overlay.
    pub fn label(&self) -> &'static str {
        match &self.mode {
            Mode::Cadence(_) => "paced",
            Mode::Anchor(_) => "direct",
        }
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
