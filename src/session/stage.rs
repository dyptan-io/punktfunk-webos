//! The single place that talks to the video decoder.
//!
//! Everything between "an access unit arrived" and "NDL has been fed" lives here: host-PTS
//! anchoring on the refresh-rate-reconciled frame interval, backpressure metering,
//! freeze-until-reanchor, and keyframe-request throttling. The video pump keeps only the
//! parts that are wire-shaped — pulling frames, and *how* a keyframe is asked for, which it
//! answers to [`SinkResult::NeedKeyframe`] with `NativeClient::request_keyframe`.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use punktfunk_core::quic;

use crate::core::media::{AudioPlane, NotReady, SessionClock, VideoSink, VideoSinkCaps};
use crate::session::timeline::{reconciled_frame_interval_ns, HostPtsAnchor};
use crate::session::StreamStats;

/// Freeze duration after which we resume even without a clean re-anchor.
const HOLD_GIVE_UP: Duration = Duration::from_secs(2);
/// Feed calls slower than this suggest decoder backpressure rather than network loss.
const FEED_BACKPRESSURE_WARN: Duration = Duration::from_millis(20);
/// How often the sink refreshes NDL's render-buffer depth for the decode-latency signal —
/// three samples per 750 ms ABR report window; see [`VideoStage::decode_us`].
const BACKLOG_POLL: Duration = Duration::from_millis(250);
/// Render-buffer frames NDL holds as a *standing* present cushion, excluded from the ABR decode
/// figure (see [`VideoStage::decode_us`]). NDL presents off its own clock, so a healthy session
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
/// NDL's render queue that is genuinely backlog. See [`VideoStage::decode_us`] for why the queue
/// belongs in it at all, and [`STANDING_CUSHION_FRAMES`] for why `cushion` comes out first.
///
/// Split out as a free function purely so it is testable — [`VideoStage`] owns a live NDL handle and
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

/// One delivery off the transport, as the pump sees it — the stage decides what it means.
pub struct WireFrame<'a> {
    pub data: &'a [u8],
    /// Host capture-clock PTS.
    pub pts_ns: u64,
    pub index: u32,
    /// Slice-progressive piece info, `None` for a whole-AU delivery
    /// (`punktfunk_core::session::FramePart`).
    pub part: Option<punktfunk_core::session::FramePart>,
    /// This frame can restart decoding on its own (IDR, or an LTR recovery anchor).
    pub reanchor: bool,
    /// Loss was detected at or before this frame — a sequence gap, or a frame the transport
    /// dropped.
    pub loss: bool,
}

/// What the stage worked out about one delivery before feeding it.
#[derive(Clone, Copy)]
struct FrameFlags {
    reanchor: bool,
    loss: bool,
    /// Host frame index, for logs only.
    index: u64,
    /// This feed is one PIECE of an access unit whose end has not arrived yet
    /// (slice-progressive delivery — `punktfunk_core::session::FramePart`, on for every NDL v2
    /// session). The decoder still takes the bytes; what a piece is NOT is a
    /// presentable frame, so the per-frame reference points hang off the piece that completes the
    /// AU instead. `false` for every feed on the whole-AU path.
    partial: bool,
}

/// What [`AuParts`] decided about one delivery.
enum PartStep {
    /// Hand the bytes to the sink. `partial` = this is not the AU's last piece; `lost_parts` = an
    /// earlier AU died mid-flight, so decoding cannot continue from where it stopped.
    Feed { partial: bool, lost_parts: bool },
    /// Nothing usable — a piece of an AU already abandoned. The decoder must not see it.
    Discard,
}

/// Slice-progressive reassembly bookkeeping (`punktfunk_core::session::FramePart`).
///
/// On every NDL v2 session, AU prefixes arrive while the rest is still on the wire and the decoder
/// gets a frame's first bytes without waiting for its last datagram — a real slice of a frame
/// period at high bitrate, and pure latency: none of that wait is decode work. On a backend that
/// can't take them (`Negotiated::clamp`) every delivery carries `part: None` and this is a
/// pass-through.
///
/// The contract enforced here is core's: parts arrive in order with no gaps, BUT the pre-decode
/// hand-off may drop entries (memory pressure, a jump-to-live clear), so an `offset` that isn't the
/// open AU's next expected byte means that AU is gone. There is no abort marker — a `first` part for
/// a new index while one is still open is how a death is signalled. Both cases abandon the AU and
/// report loss, which puts the sink into freeze-until-reanchor and asks the host for a keyframe:
/// the decoder is holding a truncated input, and nothing short of a fresh anchor clears it.
#[derive(Default)]
struct AuParts {
    /// `(frame_index, next expected byte offset)` of the AU currently being fed.
    open: Option<(u32, u32)>,
    /// An AU was abandoned and nothing has restarted decoding since — so whatever comes next is
    /// resuming against a decoder that still holds a truncated frame.
    abandoned: bool,
}

impl AuParts {
    fn step(&mut self, frame: &WireFrame<'_>, takes_parts: bool) -> PartStep {
        let Some(part) = frame.part.filter(|_| takes_parts) else {
            // Whole-AU delivery: parts weren't negotiated, this backend doesn't take them, or
            // this is an aged-out chunk-aligned partial — core hands all three over as one buffer.
            self.open = None;
            return PartStep::Feed {
                partial: false,
                lost_parts: std::mem::take(&mut self.abandoned),
            };
        };
        let len = frame.data.len() as u32;
        if part.first {
            let lost_parts = self.open.take().is_some() | std::mem::take(&mut self.abandoned);
            if lost_parts {
                tracing::warn!("frame parts: AU {} starts over an unfinished one", frame.index);
            }
            self.open = (!part.last).then_some((frame.index, len));
            return PartStep::Feed {
                partial: !part.last,
                lost_parts,
            };
        }
        match self.open {
            Some((index, next)) if index == frame.index && next == part.offset => {
                self.open = (!part.last).then_some((index, next + len));
                PartStep::Feed {
                    partial: !part.last,
                    lost_parts: false,
                }
            }
            // Either nothing is open (the AU was abandoned, or this part arrived without its head)
            // or the offset skipped — both mean the AU can never be completed.
            open => {
                if open.is_some() {
                    tracing::warn!(
                        "frame parts: AU {} broke at offset {} — abandoning",
                        frame.index,
                        part.offset,
                    );
                }
                self.drop_open();
                PartStep::Discard
            }
        }
    }

    /// Forget the AU in flight: its remaining parts are no longer feedable, and whatever restarts
    /// decoding has to be told the decoder holds a truncated input.
    fn drop_open(&mut self) {
        self.abandoned |= self.open.take().is_some();
    }
}

/// Outcome of one [`VideoStage::submit`].
pub enum SinkResult {
    /// Fed to the decoder. `decode_us` is the latency figure for the host's ABR
    /// controller, present only when the sink was built with `report_decode_latency`.
    Presented { decode_us: Option<u32> },
    /// Skipped — still frozen, waiting for a re-anchor.
    Held,
    /// Skipped or failed, and the throttle allows asking the host for a keyframe now.
    NeedKeyframe,
}

/// Everything the sink needs to know up front.
pub struct SinkConfig {
    /// The host's frame cadence. Drives both the frame-interval grid and the backlog→latency fold.
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

/// Everything between "an access unit arrived" and "the decoder has been fed", on any backend.
///
/// Backend-blind by construction: it holds a [`VideoSink`] and asks it what it can do
/// ([`VideoSinkCaps`](crate::core::media::VideoSinkCaps)) rather than which one it is.
pub struct VideoStage {
    sink: Box<dyn VideoSink>,
    /// What this backend can be asked to do — read instead of matching on which backend it is.
    caps: VideoSinkCaps,
    /// The audio plane this load produced, if any — kept for its depth reading; everything the
    /// stage publishes to it goes through [`Self::clock`].
    audio_plane: Option<std::sync::Arc<dyn AudioPlane>>,
    /// Slice-progressive reassembly state — a pass-through on a backend that doesn't take parts
    /// (see [`AuParts`]).
    parts: AuParts,
    /// The one host-PTS → sink-clock mapping this session's planes share. The stage owns it
    /// because the video plane is what derives it, frame by frame.
    clock: Arc<SessionClock>,
    stats: Arc<StreamStats>,
    cfg: SinkConfig,
    /// The panel's actual drain cadence, reconciled against the stream rate. NDL drains at panel
    /// cadence, so this, not the stream rate, is what converts a render-queue depth into time — for BOTH consumers of
    /// that conversion ([`video_e2e_ns`] and [`VideoStage::decode_us`]), so one depth cannot mean two
    /// different latencies.
    frame_interval_ns: u64,
    /// NDL host-PTS→player-clock mapping.
    host_anchor: HostPtsAnchor,
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

impl VideoStage {
    pub fn new(sink: Box<dyn VideoSink>, stats: Arc<StreamStats>, cfg: SinkConfig) -> Self {
        let stream_hz = cfg.stream_hz.max(1);
        let frame_interval_ns = reconciled_frame_interval_ns(stream_hz);
        let audio_plane = sink.audio_plane();
        let caps = sink.caps();
        let clock = Arc::new(SessionClock::default());
        if let Some(plane) = &audio_plane {
            plane.attach_clock(clock.clone());
        }
        Self {
            parts: AuParts::default(),
            clock,
            caps,
            sink,
            audio_plane,
            stats,
            frame_interval_ns,
            host_anchor: HostPtsAnchor::new(frame_interval_ns),
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
        self.sink.set_color(meta, color)
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

    /// Drop everything derived from a mapping that no longer holds: the host anchor and the audio
    /// plane's copy of it. The two move in lockstep or the planes end up on timelines that
    /// disagree.
    fn reset_timeline(&mut self) {
        self.host_anchor.reset();
        self.clock.clear();
    }

    /// This frame's stamp in the sink's own clock domain.
    ///
    /// A sink with a clock has no PTS clock of its own (NDL and SMP both count from their load),
    /// so the host's capture PTS is mapped onto it ([`HostPtsAnchor`]) — which is also what keeps
    /// video and any audio plane in ONE timeline. A sink without one presents in feed order and
    /// the stamp is discarded at the feed, so the host PTS passes through untouched.
    fn pts_base_ns(&mut self, frame_pts_ns: u64) -> u64 {
        match self.sink.clock() {
            Some(clock) => {
                let now = clock.now_ns();
                self.host_anchor.map(frame_pts_ns, now)
            }
            None => frame_pts_ns,
        }
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
        // A backend with no queue to read has no depth, and asking costs an FFI call per poll for
        // an answer that is always `None`.
        if !self.caps.render_queue {
            return None;
        }
        if self.last_backlog_poll.is_none_or(|t| t.elapsed() >= BACKLOG_POLL) {
            self.last_backlog_poll = Some(Instant::now());
            self.backlog_cached = self.sink.queue_depth().map(u64::from);
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

    /// Standing PTS lead this run has trimmed off NDL's stamps, in ms — the decoder-hold latency
    /// the session started with, and the only place it is observable (see [`HostPtsAnchor`]).
    pub fn pts_trimmed_ms(&self) -> u64 {
        self.host_anchor.trimmed_ns() / 1_000_000
    }

    /// Audio-plane queue depth in ms, or `None` on a session with no plane — see
    /// `NdlVideo::audio_plane_lead_ms`. Here because it is a *video* symptom: the plane's depth is
    /// what NDL paces the picture on, so it belongs next to the backlog in the video heartbeat.
    pub fn audio_plane_lead_ms(&self) -> Option<i64> {
        self.audio_plane.as_deref().map(AudioPlane::lead_ms)
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
    /// Present one delivery, or decide not to. The stage owns everything past the wire: how the
    /// pieces of an access unit fit together, whether the timeline holds, and what the decoder's
    /// clock domain calls this frame.
    pub fn submit(&mut self, frame: &WireFrame<'_>) -> SinkResult {
        // A piece of an AU whose predecessor went missing has nowhere to go: the decoder holds a
        // truncated input and only a re-anchor clears it. `Discard` says so; `Feed` carries whether
        // the pieces so far leave this AU decodable.
        let PartStep::Feed { partial, lost_parts } = self.parts.step(frame, self.caps.partial_au) else {
            return SinkResult::Held;
        };
        // Only a completed AU is a frame — a session feeding pieces would otherwise count one
        // picture several times, and the overlay reads this figure as pictures per second.
        if !partial {
            self.stats.frames.fetch_add(1, Ordering::Relaxed);
        }
        let flags = FrameFlags {
            reanchor: frame.reanchor,
            loss: frame.loss || lost_parts,
            index: u64::from(frame.index),
            partial,
        };
        let result = self.feed(frame.data, frame.pts_ns, flags);
        // Either the piece never reached the decoder (a hold swallows it, an error refused it) or
        // it did and a keyframe has just been asked for regardless — on both paths the AU cannot
        // be completed. Forgetting it costs the rest of one AU; keeping it would eventually feed a
        // frame with a hole in it.
        if !matches!(result, SinkResult::Presented { .. }) {
            self.parts.drop_open();
        }
        result
    }

    fn feed(&mut self, au: &[u8], pts_ns: u64, flags: FrameFlags) -> SinkResult {
        let request_keyframe = match self.gate(&flags) {
            HoldGate::Skip(result) => return result,
            HoldGate::Feed { request_keyframe } => request_keyframe,
        };

        let base_ns = self.pts_base_ns(pts_ns);
        // The submit instant, on CLOCK_REALTIME — the same basis the host stamps `pts_ns` with and
        // the skew handshake compares, so the two are directly subtractable. Taken BEFORE the feed
        // call: `play` blocks for its submission time, and the frame enters NDL's pipeline behind
        // exactly the frames the backlog below counts.
        let submit_realtime_ns = punktfunk_core::client::now_realtime_ns();
        let feed_start = Instant::now();
        let play_result = self.sink.feed(au, base_ns);
        let feed_elapsed = feed_start.elapsed();
        self.stats.feed_us.store(
            u32::try_from(feed_elapsed.as_micros()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        if feed_elapsed >= FEED_BACKPRESSURE_WARN {
            tracing::warn!(
                "NDL slow: {:.1}ms (frame {}, pts {:.2}ms)",
                feed_elapsed.as_secs_f32() * 1000.0,
                flags.index,
                base_ns as f64 / 1_000_000.0,
            );
        }

        // A failed query counts as no queue for both consumers below: neither the ABR figure nor the
        // A/V reference has a better guess, and both already treat 0 as "nothing to add".
        let backlog = self.poll_backlog().unwrap_or(0);
        let (decode_us, failed_keyframe) = match play_result {
            Ok(()) if flags.partial => (None, false),
            Ok(()) => {
                // Only a frame NDL actually accepted is on its way to the glass. A failed feed is
                // followed by a flush and a hold, where the reference would be meaningless.
                self.publish_video_e2e(submit_realtime_ns, pts_ns, backlog);
                // Same reason, and it also doubles as the audio plane's start gate. `play_audio` has
                // no `ensure_loaded` guard of its own, so latching off a REJECTED frame turns the
                // audio thread loose on a pipeline NDL hasn't loaded yet — which costs the session
                // its audio outright.
                // Held back until the anchor's trim has settled: audio stamps ride this offset and
                // can only move forward, so latching onto a mapping still being pulled earlier
                // costs lip sync (see `HostPtsAnchor::ready_for_audio`).
                if self.host_anchor.ready_for_audio() {
                    self.clock.latch(base_ns as i64 - pts_ns as i64);
                }
                let decode_us = self
                    .cfg
                    .report_decode_latency
                    .then(|| self.decode_us(feed_elapsed, backlog));
                (decode_us, false)
            }
            Err(e) => (None, self.on_play_error(&e, &flags, base_ns)),
        };

        if request_keyframe || failed_keyframe {
            SinkResult::NeedKeyframe
        } else {
            SinkResult::Presented { decode_us }
        }
    }

    /// The freeze-until-reanchor gate, run before every feed: opens a hold on fresh loss, keeps
    /// frames out while one is up, and releases it on a re-anchor or [`HOLD_GIVE_UP`].
    fn gate(&mut self, flags: &FrameFlags) -> HoldGate {
        if flags.loss && !self.holding() {
            self.begin_hold();
            tracing::warn!("loss (frame {}) — freezing", flags.index);
            if self.caps.flush {
                let _ = self.sink.flush();
            }
        }
        let Some(started) = self.hold_started else {
            return HoldGate::Feed {
                request_keyframe: false,
            };
        };
        let request_keyframe = self.take_keyframe_slot();
        let gave_up = started.elapsed() >= HOLD_GIVE_UP;
        if !flags.reanchor && !gave_up {
            return HoldGate::Skip(if request_keyframe {
                SinkResult::NeedKeyframe
            } else {
                SinkResult::Held
            });
        }
        tracing::info!(
            "resuming after {:.0}ms (frame {}, reanchor={}, gave_up={gave_up})",
            started.elapsed().as_secs_f32() * 1000.0,
            flags.index,
            flags.reanchor,
        );
        // The real timeline just jumped (freeze then reanchor/give-up) — nothing about
        // the pre-hold accumulator is worth continuing.
        self.reset_timeline();
        self.stats.holding.store(false, Ordering::Relaxed);
        self.hold_started = None;
        HoldGate::Feed { request_keyframe }
    }

    /// Handles a refused feed; returns whether to ask the host for a keyframe.
    fn on_play_error(&mut self, e: &anyhow::Error, flags: &FrameFlags, base_ns: u64) -> bool {
        tracing::warn!(
            "{} error (frame {}, pts {:.2}ms): {e:#}",
            self.sink.name(),
            flags.index,
            base_ns as f64 / 1_000_000.0,
        );
        if !self.take_keyframe_slot() {
            return false;
        }
        // A frame refused because the pipeline hasn't finished loading is NOT a decode error, and
        // gets neither loss response.
        //
        // No flush: against a not-yet-loaded pipeline it silently kills the audio plane for the
        // session (video recovers, audio never does — observed on CX), and nothing is queued in
        // NDL to discard anyway.
        //
        // No hold: freeze-until-reanchor is mid-stream recovery, and at frame 0 there is no
        // last-good picture to freeze on. Worse, holding short-circuits `submit` before `play`,
        // the only caller of the feed-anyway escape, so the hold outlives its own cause — release
        // then needs the host's reanchor or `HOLD_GIVE_UP`, both evaluated only when a frame
        // arrives, and a static desktop sends none. Request a keyframe and let the next frame
        // retry.
        if e.downcast_ref::<NotReady>().is_none() {
            if self.caps.flush {
                let _ = self.sink.flush();
            }
            self.begin_hold();
        }
        true
    }
}

/// What [`VideoStage::gate`] decided about this frame.
enum HoldGate {
    /// Feed it. `request_keyframe` is set when a hold released on this frame and the throttle
    /// allowed asking for one — the frame is still fed, but the request is what gets reported.
    Feed { request_keyframe: bool },
    /// Still frozen — skip it, and report this instead.
    Skip(SinkResult),
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{cushion_frames, decode_report_us, video_e2e_ns, CUSHION_MAX_FRAMES, STANDING_CUSHION_FRAMES};

    /// 60 Hz, the cadence the frame interval reconciles to on most panels.
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
