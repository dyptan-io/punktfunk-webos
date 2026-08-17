//! The single place that talks to the video decoder.
//!
//! Everything between "an access unit arrived" and "NDL has been fed" lives here: host-PTS
//! anchoring, the refresh-rate-reconciled [`PtsPacer`], backpressure metering,
//! freeze-until-reanchor, and keyframe-request throttling. The video pump keeps only the
//! parts that are wire-shaped — pulling frames, and *how* a keyframe is asked for, which it
//! answers to [`SinkResult::NeedKeyframe`] with `NativeClient::request_keyframe`.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use punktfunk_core::quic;

use crate::platform::webos::ndl::v1::NdlV1Video;
use crate::platform::webos::ndl::{NdlVideo, NotLoadedYet};
use crate::platform::webos::smp::SmpVideo;
use crate::session::pacing::{reconciled_pace_interval_ns, HostPtsAnchor, PtsPacer};
use crate::session::StreamStats;

/// Freeze duration after which we resume even without a clean re-anchor.
const HOLD_GIVE_UP: Duration = Duration::from_secs(2);
/// Feed calls slower than this suggest decoder backpressure rather than network loss.
const FEED_BACKPRESSURE_WARN: Duration = Duration::from_millis(20);
/// How often the sink refreshes NDL's render-buffer depth for the decode-latency signal —
/// three samples per 750 ms ABR report window; see [`NdlSink::decode_us`].
const BACKLOG_POLL: Duration = Duration::from_millis(250);
/// Render-buffer frames NDL holds as a *standing* present cushion, excluded from the ABR decode
/// figure (see [`NdlSink::decode_us`]). NDL presents off its own clock, so a healthy session
/// sits at 1-2 frames of depth indefinitely — lead, not backlog. Folded in raw it manufactures a
/// constant 8-17ms of apparent decode latency, crossing `abr`'s 15ms decode-rise threshold and
/// backing off bitrate for no real reason (observed on the CX 2026-08-10: 1440p120 driven from
/// 25 Mbps to the 5 Mbps floor with zero loss and flat OWD). Subtracting it keeps the real
/// signal — a queue that GROWS past this is genuine falling-behind — while dropping the constant.
///
/// Only a floor: the settled depth is NDL's business and one frame is 16.6ms at 60Hz, so a mode
/// idling at three frames would reproduce the bug against a fixed two. The excluded depth is the
/// rolling min over [`CUSHION_MIN_POLLS`], clamped into this..=[`CUSHION_MAX_FRAMES`] — this value
/// covers only the first seconds, before that minimum has seen a settled queue.
const STANDING_CUSHION_FRAMES: u64 = 2;
/// Polls whose minimum depth is the self-calibrating cushion (see [`STANDING_CUSHION_FRAMES`]).
/// 20 × [`BACKLOG_POLL`] = 5s: long enough that `abr`'s two-window (1.5s) decode backoff still
/// fires on a queue that genuinely grows, short enough to follow a mode or content change.
const CUSHION_MIN_POLLS: usize = 20;
/// Ceiling on the self-calibrated cushion. Without it, a decoder that stays overloaded for longer
/// than [`CUSHION_MIN_POLLS`] teaches the rolling minimum its own backlog and the signal goes
/// quiet — the same baseline-absorption trap this fix exists to escape, just faster. Lead is a
/// couple of frames by construction, so anything past this is queue, and queue must stay visible.
const CUSHION_MAX_FRAMES: u64 = 4;

/// This frame's video end-to-end latency, in the shape [`punktfunk_core::audio::AvSync`] compares
/// against: the instant it reaches the glass expressed in the HOST capture clock, minus its host
/// PTS. Published for the audio plane, which steers its ring depth to land audio WITH the picture.
///
/// **Why an estimate at all.** NDL is submit-only — `NDL_DirectVideoPlay` returns nothing about
/// presentation and there is no glass callback — so the true stamp the Vulkan/Android/Apple
/// presenters publish does not exist here. This is the fallback core's own `video_e2e_ns` field
/// docs already sanction ("a TRUE on-glass stamp where `VK_KHR_present_wait` is available, and the
/// submit instant otherwise"), plus the one further term this platform *can* observe: NDL's
/// standing render queue. That middle term is the one that MOVES — a decoder falling behind
/// buffers deeper — and it is precisely the ratchet the sync loop exists to cancel.
///
/// **The estimate omits one term, and that is why nothing steers on it.** NDL's decode + panel
/// latency *after* the queue drains cannot be observed from the app at all, so it is simply absent
/// here — which biases this figure low, biases the measured A/V offset high, and would aim the
/// audio ring correspondingly shallow, i.e. **play audio early by the whole missing term**. At
/// 60 Hz a plausible 2–5 frame pipeline is 33–83 ms, far outside `AvSync`'s 10 ms deadband, so
/// acting on this would be a systematic error larger than the drift being corrected. Hence
/// measure-only: the offset reaches the stats overlay and no target reaches `JitterPolicy` (see
/// `platform::webos::audio`'s `observe_av`). Arming it means measuring that term on real hardware
/// first and folding it back in as a constant — `docs/NOTES.md` § "A/V sync" has the procedure.
///
/// `None` when the arithmetic would place the frame on the glass *before* its own capture — a
/// wall-clock step, a stale PTS, or a host clock that has not settled. The caller then publishes
/// nothing, core reads the untouched cell as "nothing presented yet", and the loop stays inert
/// rather than steering on a figure it cannot believe.
fn video_e2e_ns(
    submit_realtime_ns: i128,
    clock_offset_ns: i64,
    frame_pts_ns: u64,
    backlog_frames: u64,
    frame_interval_ns: u64,
) -> Option<u64> {
    let pipeline_ns = backlog_frames.saturating_mul(frame_interval_ns);
    let glass_host_ns = submit_realtime_ns
        .checked_add(i128::from(pipeline_ns))?
        .checked_add(i128::from(clock_offset_ns))?;
    u64::try_from(glass_host_ns.checked_sub(i128::from(frame_pts_ns))?).ok()
}

/// The decode-stage latency reported to `punktfunk_core::abr`: submission time plus the part of
/// NDL's render queue that is genuinely backlog. See [`NdlSink::decode_us`] for why the queue
/// belongs in it at all, and [`STANDING_CUSHION_FRAMES`] for why `cushion` comes out first.
///
/// Split out as a free function purely so it is testable — [`NdlSink`] owns a live NDL handle and
/// cannot be built off-device.
fn decode_report_us(feed_elapsed: Duration, backlog: u64, cushion: u64, frame_interval_ns: u64) -> u32 {
    let queued_ns = backlog.saturating_sub(cushion).saturating_mul(frame_interval_ns);
    let decode_us = u64::try_from(feed_elapsed.as_micros())
        .unwrap_or(u64::MAX)
        .saturating_add(queued_ns / 1_000);
    u32::try_from(decode_us).unwrap_or(u32::MAX)
}

/// The depth [`decode_report_us`] treats as present lead rather than backlog: the rolling minimum
/// of recent poll samples, clamped into [`STANDING_CUSHION_FRAMES`]..=[`CUSHION_MAX_FRAMES`].
/// Empty (no poll yet) yields the floor, never 0 — a session's first windows are exactly when the
/// buffer is still filling and a raw fold does its damage.
fn cushion_frames(samples: impl Iterator<Item = u64>) -> u64 {
    samples
        .min()
        .unwrap_or(0)
        .clamp(STANDING_CUSHION_FRAMES, CUSHION_MAX_FRAMES)
}

/// What the pump knows about a frame that the sink can't work out for itself.
pub struct FrameFlags {
    /// This frame can restart decoding on its own (IDR, or an LTR recovery anchor).
    pub reanchor: bool,
    /// Loss was detected at or before this frame — a sequence gap, or a frame the
    /// transport dropped.
    pub loss: bool,
    /// Host frame index, for logs only.
    pub index: u64,
}

/// Outcome of one [`NdlSink::submit`].
pub enum SinkResult {
    /// Fed to the decoder. `decode_us` is the latency figure for the host's ABR
    /// controller, present only when the sink was built with `report_decode_latency`.
    Presented { decode_us: Option<u32> },
    /// Skipped — still frozen, waiting for a re-anchor.
    Held,
    /// Skipped or failed, and the throttle allows asking the host for a keyframe now.
    NeedKeyframe,
}

/// The video-decode backend this session runs on: one of the two NDL `DirectMedia` generations
/// (`device::ndl_generation` picks), or SMP where the user selected it
/// (`Settings::video_backend`, offered on webOS 3.5-4.x only). Everything above this type is
/// backend-blind.
///
/// V2 is Arc'd because the audio-offload path shares the handle with `ndl_audio_pump`; NDL
/// unloads process-globally, so unload waits for both threads (`Arc::drop`).
pub enum VideoPlayer {
    /// webOS 5+: PTS-driven feed, render-buffer query, flush, HDR metadata.
    V2(Arc<NdlVideo>),
    /// webOS 3.5-4.x: H.264 SDR, feed-order presentation, none of the above (see
    /// `platform::webos::ndl::v1`).
    V1(NdlV1Video),
    /// SMP: HEVC + HDR + `pauseAtDecodeTime` on a legacy TV (see
    /// `platform::webos::smp`).
    Smp(SmpVideo),
}

impl VideoPlayer {
    /// Feed access unit; return (result, `feed_duration`) for ABR latency reporting.
    /// `pts_ns` must already be in NDL's PTS clock domain (see [`Self::pace_base_ns`]).
    fn play(&self, au: &[u8], pts_ns: u64) -> (anyhow::Result<()>, Duration) {
        let t = Instant::now();
        let result = match self {
            Self::V2(ndl) => ndl.play(au, pts_ns),
            // v1's feed takes no timestamp at all — frames present as fed.
            Self::V1(ndl) => ndl.play(au),
            Self::Smp(sf) => sf.play(au, pts_ns),
        };
        (result, t.elapsed())
    }

    /// Unpaced PTS reference for `frame` in NDL's clock domain, smoothed by [`PtsPacer`]
    /// before it reaches [`Self::play`]. NDL has no PTS clock of its own (see
    /// `NdlVideo::elapsed_ns`), so the host PTS is mapped onto NDL's player clock
    /// ([`HostPtsAnchor`]) and the reference tracks host frame cadence instead of feed-time
    /// wall-clock, keeping delivery jitter out of the pacer's drift-clamp anchor.
    ///
    /// **V2 anchors whether or not pacing is on.** It used to fall back to raw player time with
    /// pacing off, which stamps video in *feed* time while offloaded audio is stamped in *host*
    /// time (`NdlVideo::play_audio`) — two clocks that agree only by luck, and stop agreeing the
    /// moment a receive-backlog flush jumps the client to live. Anchoring unconditionally is what
    /// makes the two planes share one mapping by construction. Pacing off now means only that
    /// [`PtsPacer`]'s smoothing is skipped.
    ///
    /// SMP is mapped the same way as V2 — its PTS domain is nanoseconds since *its own* load
    /// (`now - openTime`), not the host's clock, so an unmapped host PTS would sit an
    /// arbitrary epoch away and `pauseAtDecodeTime` would hold every frame. V1 has nothing to pace
    /// against at all (no PTS input, no player clock), so the host PTS returns untouched there and
    /// the pacer's output is discarded at the feed.
    fn pace_base_ns(&self, frame_pts_ns: u64, anchor: &mut HostPtsAnchor, pacing_on: bool) -> u64 {
        let player_clock_ns = match self {
            Self::V2(ndl) => ndl.elapsed_ns(),
            Self::Smp(sf) => sf.elapsed_ns(),
            Self::V1(_) => return frame_pts_ns,
        };
        if pacing_on || matches!(self, Self::V2(_)) {
            anchor.map(frame_pts_ns, player_clock_ns)
        } else {
            player_clock_ns
        }
    }

    /// Hand the audio plane the host-PTS → player-clock offset this frame was stamped with, so
    /// offloaded Opus lands in the same timeline as the picture (`NdlVideo::play_audio`). Only
    /// the first frame after an anchor reset is taken (see `NdlVideo::latch_pts_offset`) — this
    /// is called per frame so that re-latch happens promptly, not so the offset tracks.
    /// `base_ns` is the *unpaced* reference — the pacer's ±half-frame smoothing is video-only
    /// jitter compensation and must not be projected onto the audio timeline.
    ///
    /// V2 only, and only when audio is offloaded; no other backend feeds audio through NDL.
    fn latch_pts_offset(&self, frame_pts_ns: u64, base_ns: u64) {
        if let Some(ndl) = self.offloaded_ndl() {
            ndl.latch_pts_offset(base_ns as i64 - frame_pts_ns as i64);
        }
    }

    /// Decouple the audio plane from a timeline that no longer holds — the anchor it was
    /// derived from is being reset. Audio holds until the next fed frame republishes.
    fn clear_pts_offset(&self) {
        if let Some(ndl) = self.offloaded_ndl() {
            ndl.clear_pts_offset();
        }
    }

    /// The NDL handle the audio plane shares, or `None` on every backend/load that decodes Opus
    /// in software — the gate both offset calls above answer to.
    fn offloaded_ndl(&self) -> Option<&NdlVideo> {
        match self {
            Self::V2(ndl) => ndl.audio_offloaded().then(|| ndl.as_ref()),
            Self::V1(_) | Self::Smp(_) => None,
        }
    }

    /// Neither V1 nor SMP has a render-buffer to flush (SMP drains its own buffer and
    /// resets on an IDR) — the sink's freeze still holds frames.
    fn flush(&self) -> anyhow::Result<()> {
        match self {
            Self::V2(ndl) => ndl.flush(),
            Self::V1(_) | Self::Smp(_) => Ok(()),
        }
    }

    /// No-op on V1: SDR/BT.709 only, and no `SetHDRInfo` to call.
    pub fn set_color_info(&self, meta: Option<&quic::HdrMeta>, color: quic::ColorInfo) -> anyhow::Result<()> {
        match self {
            Self::V2(ndl) => ndl.set_color_info(meta, color),
            Self::Smp(sf) => sf.set_color_info(meta, color),
            Self::V1(_) => Ok(()),
        }
    }

    /// Shared NDL handle when audio-offloaded; None on a video-only load or any other backend.
    /// V2 only: V1's `NDL_DirectAudio*` has no Opus source type, and SMP loads `needAudio: false`.
    pub fn ndl_audio_handle(&self) -> Option<Arc<NdlVideo>> {
        match self {
            Self::V2(ndl) => ndl.audio_offloaded().then(|| ndl.clone()),
            Self::V1(_) | Self::Smp(_) => None,
        }
    }

    /// NDL render-buffer backlog. `None` if the query fails, or on a backend with no such query —
    /// the sink treats both as "no queue to add".
    fn render_buffer_length(&self) -> Option<i32> {
        match self {
            Self::V2(ndl) => ndl.render_buffer_length(),
            Self::V1(_) | Self::Smp(_) => None,
        }
    }

    /// For the stats overlay and the session log line.
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::V2(_) => "NDL v2",
            Self::V1(_) => "NDL v1",
            Self::Smp(_) => "SMP",
        }
    }
}

/// Everything the sink needs to know up front.
pub struct SinkConfig {
    /// The host's frame cadence. Drives both the pacing grid and the backlog→latency fold.
    pub stream_hz: u32,
    /// Whether the host asked for decode-latency reports (its ABR controller).
    pub report_decode_latency: bool,
    /// Host-minus-client clock skew, live (`NativeClient::clock_offset_shared`). Read per
    /// published frame, never cached — the handshake re-syncs it mid-stream.
    pub clock_offset: Arc<AtomicI64>,
    /// Where [`video_e2e_ns`] is published for the audio plane
    /// (`NativeClient::video_e2e_shared`). `0` = nothing on the glass yet.
    pub video_e2e: Arc<AtomicU64>,
}

/// Minimum spacing between [`SinkResult::NeedKeyframe`] results: the request travels on its own
/// QUIC control stream, so a tight interval costs nothing but the request itself.
const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The NDL implementation: [`VideoPlayer`] + [`PtsPacer`] + [`StreamStats`].
pub struct NdlSink {
    player: VideoPlayer,
    stats: Arc<StreamStats>,
    cfg: SinkConfig,
    /// The panel's actual drain cadence, reconciled against the stream rate — the same interval
    /// the pacer runs on. NDL drains at panel cadence whether or not pacing is enabled, so this,
    /// not the stream rate, is what converts a render-queue depth into time — for BOTH consumers of
    /// that conversion ([`video_e2e_ns`] and [`NdlSink::decode_us`]), so one depth cannot mean two
    /// different latencies.
    frame_interval_ns: u64,
    /// Always instantiated — the Blue button can flip pacing on mid-stream, so the state
    /// must exist even when it starts off.
    pacer: PtsPacer,
    /// NDL host-PTS→player-clock mapping, reset in lockstep with the pacer.
    host_anchor: HostPtsAnchor,
    /// Previous-frame pacing state, so an off→on flip can re-anchor cleanly.
    pacing_was_on: bool,
    /// Last polled depth, `None` if that query failed — which must not read as an empty queue.
    backlog_cached: Option<u64>,
    /// Recent poll depths, newest last — their minimum is the cushion the decode figure excludes
    /// (see [`STANDING_CUSHION_FRAMES`]). Bounded at [`CUSHION_MIN_POLLS`].
    backlog_recent: std::collections::VecDeque<u64>,
    /// Cached [`Self::refresh_cushion`] result — read per presented frame, written per poll.
    cushion_frames: u64,
    last_backlog_poll: Option<Instant>,
    last_keyframe_request: Option<Instant>,
    /// Freeze-until-reanchor: while holding, frames are skipped rather than fed — the
    /// punch-through plane keeps the last good picture. Resumes on IDR / LTR-RFI recovery
    /// anchor, or after [`HOLD_GIVE_UP`]. `Some` for exactly as long as the hold lasts (see
    /// [`Self::holding`]), and not reset on cascading gaps so the give-up deadline can't be
    /// pushed out indefinitely.
    hold_started: Option<Instant>,
}

impl NdlSink {
    pub fn new(player: VideoPlayer, stats: Arc<StreamStats>, cfg: SinkConfig) -> Self {
        let stream_hz = cfg.stream_hz.max(1);
        let pacing_was_on = stats.pacing_enabled.load(Ordering::Relaxed);
        let frame_interval_ns = reconciled_pace_interval_ns(stream_hz);
        Self {
            player,
            stats,
            frame_interval_ns,
            pacer: PtsPacer::new(frame_interval_ns),
            host_anchor: HostPtsAnchor::new(),
            pacing_was_on,
            cfg,
            backlog_cached: None,
            backlog_recent: std::collections::VecDeque::with_capacity(CUSHION_MIN_POLLS),
            cushion_frames: STANDING_CUSHION_FRAMES,
            last_backlog_poll: None,
            last_keyframe_request: None,
            hold_started: None,
        }
    }

    pub fn set_color_info(&self, meta: Option<&quic::HdrMeta>, color: quic::ColorInfo) -> anyhow::Result<()> {
        self.player.set_color_info(meta, color)
    }

    /// True when the throttle allows a keyframe request now; stamps it as sent.
    fn take_keyframe_slot(&mut self) -> bool {
        let ready = self
            .last_keyframe_request
            .is_none_or(|t| t.elapsed() >= KEYFRAME_REQUEST_MIN_INTERVAL);
        if ready {
            self.last_keyframe_request = Some(Instant::now());
        }
        ready
    }

    /// Drop everything derived from a mapping that no longer holds: the pacer's accumulator, the
    /// host anchor, and the audio plane's copy of that anchor. The three move in lockstep or the
    /// planes end up on timelines that disagree.
    fn reset_timeline(&mut self) {
        self.pacer.reset();
        self.host_anchor.reset();
        self.player.clear_pts_offset();
    }

    fn begin_hold(&mut self) {
        self.stats.holding.store(true, Ordering::Relaxed);
        self.hold_started.get_or_insert_with(Instant::now);
    }

    /// The decode figure reported to the host's ABR controller. NDL's `play` is
    /// decode-AND-present in one opaque call, so `feed_elapsed` alone is *submission*
    /// time — a decoder quietly falling behind buffers frames internally and the feed
    /// stays fast, which left the controller's decode-rise signal (`abr::DECODE_RISE_US`,
    /// built precisely for "the decoder saturates before the link does") effectively
    /// blind on this client. The render-buffer backlog IS that standing decode queue, so
    /// it's folded in as queue-above-cushion × the drain interval (see [`decode_report_us`]).
    /// Polled on a cadence rather than every frame — three samples per 750 ms ABR report window is
    /// plenty, and assuming an NDL query is cheap enough for per-frame use is exactly the mistake
    /// docs/NOTES.md warns against; between polls the cached depth is reused.
    fn decode_us(&self, feed_elapsed: Duration, backlog: u64) -> u32 {
        decode_report_us(feed_elapsed, backlog, self.cushion_frames, self.frame_interval_ns)
    }

    /// NDL's render-queue depth, refreshed on [`BACKLOG_POLL`]'s cadence and cached between polls.
    /// `None` is a failed query, not an empty queue.
    ///
    /// Called unconditionally, because it has TWO consumers: the ABR decode figure
    /// ([`Self::decode_us`]) and the A/V sync loop's video reference ([`video_e2e_ns`]). Polling it
    /// only when the host asks for decode latency pins the depth at `0` against hosts that never
    /// do, silently zeroing the queue term in the video reference.
    fn poll_backlog(&mut self) -> Option<u64> {
        if self.last_backlog_poll.is_none_or(|t| t.elapsed() >= BACKLOG_POLL) {
            self.last_backlog_poll = Some(Instant::now());
            self.backlog_cached = self.player.render_buffer_length().and_then(|b| u64::try_from(b).ok());
            // A freeze flushes NDL, so a depth polled while held is an emptied buffer, not this
            // mode's settled lead. Learning from it would clamp the cushion back to the floor for
            // [`CUSHION_MIN_POLLS`] after every loss event — and at 60Hz one unaccounted frame is
            // 16.6ms, straight back over the ABR's decode-rise threshold. The cached depth itself
            // still updates: A/V sync wants the truth, only the learned cushion is held steady.
            if let Some(depth) = self.backlog_cached.filter(|_| !self.holding()) {
                if self.backlog_recent.len() == CUSHION_MIN_POLLS {
                    self.backlog_recent.pop_front();
                }
                self.backlog_recent.push_back(depth);
                self.cushion_frames = cushion_frames(self.backlog_recent.iter().copied());
            }
        }
        self.backlog_cached
    }

    /// Publish this frame's video end-to-end figure for the audio plane's sync loop.
    ///
    /// Nothing is published when [`video_e2e_ns`] cannot believe its own arithmetic — the cell
    /// keeps its previous value and `AvSync` simply makes no observation, which is the same inert
    /// path as a session where no frame has been presented yet.
    fn publish_video_e2e(&self, submit_realtime_ns: i128, frame_pts_ns: u64, backlog: u64) {
        let e2e = video_e2e_ns(
            submit_realtime_ns,
            self.cfg.clock_offset.load(Ordering::Relaxed),
            frame_pts_ns,
            backlog,
            self.frame_interval_ns,
        );
        if let Some(ns) = e2e {
            self.cfg.video_e2e.store(ns, Ordering::Relaxed);
        }
    }

    /// Whether a freeze-until-reanchor hold is currently active (stats/logging).
    pub fn holding(&self) -> bool {
        self.hold_started.is_some()
    }

    /// Decoder backlog depth for the heartbeat/overlay, or `None` if NDL can't report one.
    ///
    /// Polls ([`Self::poll_backlog`]) rather than querying NDL directly, so the depth the log prints
    /// is the one the decode figure was computed from — a second, unsynchronized reading can
    /// disagree with it — and every `NDL_DirectVideoGetRenderBufferLength` stays on one cadence.
    pub fn poll_backlog_depth(&mut self) -> Option<i32> {
        self.poll_backlog().map(|d| i32::try_from(d).unwrap_or(i32::MAX))
    }

    /// Present one access unit, or decide not to. `pts_ns` is the host's capture-clock
    /// PTS; the sink maps and paces it into the decoder's own clock domain.
    pub fn submit(&mut self, au: &[u8], pts_ns: u64, flags: FrameFlags) -> SinkResult {
        if flags.loss && !self.holding() {
            self.begin_hold();
            tracing::warn!("loss (frame {}) — freezing", flags.index);
            let _ = self.player.flush();
        }
        let mut need_keyframe = self.holding() && self.take_keyframe_slot();

        let gave_up = self.hold_started.is_some_and(|t| t.elapsed() >= HOLD_GIVE_UP);
        if self.holding() && !flags.reanchor && !gave_up {
            return if need_keyframe {
                SinkResult::NeedKeyframe
            } else {
                SinkResult::Held
            };
        }
        if self.holding() {
            tracing::info!(
                "resuming after {:.0}ms (frame {}, reanchor={}, gave_up={gave_up})",
                self.hold_started.map_or(0.0, |t| t.elapsed().as_secs_f32() * 1000.0),
                flags.index,
                flags.reanchor,
            );
            // The real timeline just jumped (freeze then reanchor/give-up) — nothing about
            // the pre-hold accumulator is worth continuing.
            self.reset_timeline();
        }
        self.stats.holding.store(false, Ordering::Relaxed);
        self.hold_started = None;

        // Live pacing toggle (Blue button): re-anchor on the off→on edge so the pacer picks
        // up from the current frame rather than a stale grid.
        let pacing_on = self.stats.pacing_enabled.load(Ordering::Relaxed);
        if pacing_on && !self.pacing_was_on {
            self.reset_timeline();
        }
        self.pacing_was_on = pacing_on;

        let base_ns = self.player.pace_base_ns(pts_ns, &mut self.host_anchor, pacing_on);
        let paced_ns = if pacing_on {
            let paced = self.pacer.next(base_ns);
            self.stats
                .pacing_delta_ns
                .store(paced as i64 - base_ns as i64, Ordering::Relaxed);
            paced
        } else {
            self.stats.pacing_delta_ns.store(0, Ordering::Relaxed);
            base_ns
        };

        // The submit instant, on CLOCK_REALTIME — the same basis the host stamps `pts_ns` with and
        // the skew handshake compares, so the two are directly subtractable. Taken BEFORE the feed
        // call: `play` blocks for its submission time, and the frame enters NDL's pipeline behind
        // exactly the frames the backlog below counts.
        let submit_realtime_ns = punktfunk_core::client::now_realtime_ns();
        let (play_result, feed_elapsed) = self.player.play(au, paced_ns);
        self.stats.feed_us.store(
            u32::try_from(feed_elapsed.as_micros()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        if feed_elapsed >= FEED_BACKPRESSURE_WARN {
            tracing::warn!(
                "NDL slow: {:.1}ms (frame {}, pts {:.2}ms)",
                feed_elapsed.as_secs_f32() * 1000.0,
                flags.index,
                paced_ns as f64 / 1_000_000.0,
            );
        }

        // A failed query counts as no queue for both consumers below: neither the ABR figure nor the
        // A/V reference has a better guess, and both already treat 0 as "nothing to add".
        let backlog = self.poll_backlog().unwrap_or(0);
        if play_result.is_ok() {
            // Only a frame NDL actually accepted is on its way to the glass. A failed feed is
            // followed by a flush and a hold below, where the reference would be meaningless.
            self.publish_video_e2e(submit_realtime_ns, pts_ns, backlog);
            // Same reason, and it also doubles as the audio plane's start gate. `play_audio` has
            // no `ensure_loaded` guard of its own, so latching off a REJECTED frame turns the
            // audio thread loose on a pipeline NDL hasn't loaded yet — which costs the session
            // its audio outright.
            self.player.latch_pts_offset(pts_ns, base_ns);
        }
        let decode_us =
            (self.cfg.report_decode_latency && play_result.is_ok()).then(|| self.decode_us(feed_elapsed, backlog));

        if let Err(e) = play_result {
            tracing::warn!(
                "NDL error (frame {}, pts {:.2}ms): {e:#}",
                flags.index,
                paced_ns as f64 / 1_000_000.0,
            );
            // A frame refused because the pipeline hasn't finished loading is NOT a decode
            // error, and gets neither loss response.
            //
            // No flush: against a not-yet-loaded pipeline it silently kills the audio plane for
            // the session (video recovers, audio never does — observed on CX), and nothing is
            // queued in NDL to discard anyway.
            //
            // No hold: freeze-until-reanchor is mid-stream recovery, and at frame 0 there is no
            // last-good picture to freeze on. Worse, holding short-circuits `submit` before
            // `play`, the only caller of the feed-anyway escape, so the hold outlives its own
            // cause — release then needs the host's reanchor or `HOLD_GIVE_UP`, both evaluated
            // only when a frame arrives, and a static desktop sends none. Request a keyframe and
            // let the next frame retry.
            let not_loaded = e.downcast_ref::<NotLoadedYet>().is_some();
            if self.take_keyframe_slot() {
                if !not_loaded {
                    let _ = self.player.flush();
                    self.begin_hold();
                }
                need_keyframe = true;
            }
        }

        if need_keyframe {
            SinkResult::NeedKeyframe
        } else {
            SinkResult::Presented { decode_us }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{cushion_frames, decode_report_us, video_e2e_ns, CUSHION_MAX_FRAMES, STANDING_CUSHION_FRAMES};

    /// 60 Hz, the cadence the pacing grid reconciles to on most panels.
    const HZ60_NS: u64 = 16_666_666;
    /// A frame submitted 30 ms after the host captured it, with clocks aligned and nothing queued.
    const SUBMIT: i128 = 2_000_000_000;
    const PTS: u64 = 1_970_000_000;

    fn base() -> Option<u64> {
        video_e2e_ns(SUBMIT, 0, PTS, 0, HZ60_NS)
    }

    #[test]
    fn e2e_is_the_glass_instant_minus_the_capture_instant() {
        assert_eq!(base(), Some(30_000_000));
    }

    /// The term that makes this better than a bare submit stamp: frames already queued in NDL must
    /// drain before this one is seen, so each adds one panel interval. Deleting the backlog term
    /// collapses this onto the base case.
    #[test]
    fn the_render_queue_adds_its_own_drain_time() {
        assert_eq!(video_e2e_ns(SUBMIT, 0, PTS, 3, HZ60_NS), Some(30_000_000 + 3 * HZ60_NS));
        // And a deeper queue is strictly later — the ratchet the sync loop exists to cancel.
        let shallow = video_e2e_ns(SUBMIT, 0, PTS, 1, HZ60_NS).unwrap();
        let deep = video_e2e_ns(SUBMIT, 0, PTS, 6, HZ60_NS).unwrap();
        assert!(deep > shallow, "{deep} !> {shallow}");
    }

    /// The glass instant is expressed in the HOST clock, so the skew term shifts it directly. A
    /// host running ahead of the client makes the same frame land later in host terms.
    #[test]
    fn clock_skew_moves_the_glass_instant() {
        assert_eq!(video_e2e_ns(SUBMIT, 7_000_000, PTS, 0, HZ60_NS), Some(37_000_000));
        assert_eq!(video_e2e_ns(SUBMIT, -7_000_000, PTS, 0, HZ60_NS), Some(23_000_000));
    }

    /// A frame cannot reach the glass before it was captured. A wall-clock step or an unsettled
    /// skew estimate produces exactly this, and steering a playback ring by it would empty or
    /// overfill the ring outright — so it is rejected, not clamped.
    #[test]
    fn a_frame_landing_before_its_own_capture_is_rejected() {
        assert_eq!(video_e2e_ns(1_000_000_000, 0, 2_000_000_000, 0, HZ60_NS), None);
    }

    /// This runs on the video thread for every presented frame, so the failure mode it guards is a
    /// **panic**: plain `+` here aborts the stream on overflow in a debug build. Proven by planting
    /// exactly that — swapping the `checked_add`s for `+` fails this test with an overflow panic.
    ///
    /// What it deliberately does NOT claim: that `checked_add` beats `wrapping_add`. With real
    /// inputs the i128 accumulator cannot overflow at all (a realtime stamp is ~1.7e18 ns and the
    /// whole pipeline term is bounded by `u64::MAX` ~1.8e19, against an i128 range of ~1.7e38), and
    /// a wrapped value would be caught by the `u64::try_from` below anyway. The `checked_add`s are
    /// belt-and-braces against garbage arguments, and this gate is about the panic.
    #[test]
    fn implausible_inputs_return_none_rather_than_panicking() {
        assert_eq!(video_e2e_ns(i128::MAX, i64::MAX, 0, u64::MAX, u64::MAX), None);
        assert_eq!(video_e2e_ns(i128::MIN, i64::MIN, u64::MAX, 0, 0), None);
        // A saturating queue term must not wrap into a small, believable-looking number.
        assert_eq!(video_e2e_ns(SUBMIT, 0, PTS, u64::MAX, HZ60_NS), None);
    }

    /// 120 Hz — the mode the ABR collapse was observed at (CX, 2026-08-10).
    const HZ120_NS: u64 = 8_333_333;
    const FEED: Duration = Duration::from_micros(400);

    /// The regression the cushion exists for: folded in raw, a standing 2-frame lead reported
    /// ~16.8 ms of "decode latency" every window and walked the rate to the floor. At the settled
    /// depth the report must be feed time and nothing else.
    #[test]
    fn a_standing_present_cushion_reports_no_decode_latency() {
        for depth in 0..=STANDING_CUSHION_FRAMES {
            assert_eq!(
                decode_report_us(FEED, depth, STANDING_CUSHION_FRAMES, HZ120_NS),
                400,
                "depth {depth} should read as lead, not queue"
            );
        }
    }

    /// The signal the fold exists for, still intact: a queue GROWING past the cushion is the
    /// decoder falling behind, and each extra frame is one drain interval of real latency.
    #[test]
    fn queue_above_the_cushion_is_reported() {
        let two_over = decode_report_us(FEED, STANDING_CUSHION_FRAMES + 2, STANDING_CUSHION_FRAMES, HZ120_NS);
        assert_eq!(two_over, 400 + 2 * 8_333);
        // And deeper is strictly worse — monotonic, or the controller can't act on it.
        let one_over = decode_report_us(FEED, STANDING_CUSHION_FRAMES + 1, STANDING_CUSHION_FRAMES, HZ120_NS);
        assert!(two_over > one_over, "{two_over} !> {one_over}");
    }

    /// Runs on the video thread per presented frame, so garbage must saturate rather than panic or
    /// wrap into a believable figure (`u32::MAX` µs is ~71 min — no threshold mistakes it for calm).
    #[test]
    fn an_implausible_queue_saturates_instead_of_wrapping() {
        assert_eq!(decode_report_us(FEED, u64::MAX, 0, u64::MAX), u32::MAX);
        assert_eq!(decode_report_us(Duration::MAX, 0, 0, HZ120_NS), u32::MAX);
    }

    /// No samples yet is exactly when a raw fold does its damage — the buffer is still filling — so
    /// assume the cushion rather than an empty queue.
    #[test]
    fn the_cushion_never_starts_at_zero() {
        assert_eq!(cushion_frames(std::iter::empty()), STANDING_CUSHION_FRAMES);
        assert_eq!(cushion_frames([0, 0, 1].into_iter()), STANDING_CUSHION_FRAMES);
    }

    /// A mode that settles deeper than the floor teaches its own lead — one frame is 16.6 ms at
    /// 60 Hz, so a fixed floor of two would put such a mode straight back over the threshold.
    #[test]
    fn a_deeper_settled_queue_raises_the_cushion() {
        assert_eq!(cushion_frames([4, 3, 5, 3].into_iter()), 3);
    }

    /// …but only so far. A decoder that stays overloaded past the sample window would otherwise
    /// teach the minimum its own backlog and mute the signal — the same baseline-absorption trap
    /// this fix exists to escape, just faster.
    #[test]
    fn a_sustained_overload_cannot_teach_the_cushion_away() {
        assert_eq!(cushion_frames([40, 38, 44].into_iter()), CUSHION_MAX_FRAMES);
    }
}
