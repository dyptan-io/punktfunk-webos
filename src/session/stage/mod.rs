//! The single place that talks to the video decoder.
//!
//! Everything between "an access unit arrived" and "NDL has been fed" lives here: host-PTS
//! anchoring on the refresh-rate-reconciled frame interval, backpressure metering,
//! freeze-until-reanchor, and keyframe-request throttling. The video pump keeps only the
//! parts that are wire-shaped — pulling frames, and *how* a keyframe is asked for, which it
//! answers to [`SinkResult::NeedKeyframe`] with `NativeClient::request_keyframe`.
//!
//! Two pieces sit in submodules because they are self-contained and independently testable:
//! [`metrics`] (the ABR decode figure and the A/V video reference) and [`parts`] (slice-progressive
//! reassembly).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use punktfunk_core::quic;

use crate::core::media::{AudioPlane, NotReady, SessionClock, VideoSink, VideoSinkCaps};
use crate::session::timeline::{ms, reconciled_frame_interval_ns, Pacing, PacingHealth};
use crate::session::StreamStats;

mod metrics;
mod parts;

use metrics::{cushion_frames, decode_report_us, video_e2e_ns, CUSHION_MIN_POLLS, STANDING_CUSHION_FRAMES};
use parts::{AuParts, PartStep};

/// Freeze duration after which we resume even without a clean re-anchor.
const HOLD_GIVE_UP: Duration = Duration::from_secs(2);
/// Feed calls slower than this suggest decoder backpressure rather than network loss.
const FEED_BACKPRESSURE_WARN: Duration = Duration::from_millis(20);
/// How often the sink refreshes NDL's render-buffer depth for the decode-latency signal —
/// three samples per 750 ms ABR report window; see [`VideoStage::decode_us`].
const BACKLOG_POLL: Duration = Duration::from_millis(250);
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
    /// Stamp frames from the fixed anchor rather than the cadence loop
    /// (`Settings::direct_playback`, Experimental). **The one gate on every latency-adding measure
    /// on this path**, from the other side: the default pays a jitter-sized cushion of at most one
    /// frame interval for a cadence that does not beat against the panel, and this gives that back
    /// at the price of the judder — see `session::timeline::Pacing`.
    pub direct_playback: bool,
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
    /// NDL host-PTS→player-clock mapping — the cadence loop, or the anchor under
    /// `Settings::direct_playback`. One object, so nothing in this file branches on which.
    pacing: Pacing,
    /// The stamp the access unit currently being fed was given, while it is still open. Every piece
    /// of one AU must carry the SAME timestamp (NDL finds AU boundaries by start code and has no
    /// boundary flag of its own), and — the reason this is a field rather than a recomputation — a
    /// picture must be mapped exactly ONCE: slice-progressive delivery repeats an AU's host PTS
    /// across its pieces at increasing arrival times, so re-mapping per piece teaches the mapping
    /// the AU's TAIL arrival and inflates the measured jitter by the AU's own transmission time.
    /// `None` on the whole-AU path and between AUs.
    au_base_ns: Option<u64>,
    /// Last polled depth, `None` if that query failed — which must not read as an empty queue.
    backlog_cached: Option<u64>,
    /// Recent poll depths, newest last — their minimum is the cushion the decode figure excludes
    /// (see [`STANDING_CUSHION_FRAMES`]). Bounded at [`CUSHION_MIN_POLLS`].
    backlog_recent: std::collections::VecDeque<u64>,
    /// Cached [`Self::refresh_cushion`] result — read per presented frame, written per poll.
    cushion_frames: u64,
    last_backlog_poll: Option<Instant>,
    last_keyframe_request: Option<Instant>,
    /// Submit time accumulated across the pieces of the AU currently being fed, so the overlay's
    /// `feed_us` stays a figure per PICTURE. Stored per-piece it reported only the last slice,
    /// which on a slice-progressive session is a fraction of the AU's real submission cost.
    au_feed_us: u32,
    /// Pieces handed to the decoder this session. Only meaningful against `stats.frames` (completed
    /// AUs): the ratio is the one number that says whether slice-progressive delivery is doing
    /// anything at all on this mode, which cannot be read any other way — core only emits early
    /// parts for an AU spanning more than one FEC block, so at small AU sizes the feature is inert.
    parts_fed: u64,
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
            // The SOURCE's nominal interval, NOT `frame_interval_ns`: that one is reconciled
            // onto the panel because it converts a render-queue depth into time, and this one
            // is the cushion's ceiling, so it has to describe the cadence the HOST produces.
            // Core says so with a test of its own
            // (`the_cadence_interval_comes_from_the_stream_mode_not_the_panel`): a 120 fps
            // stream on a 60 Hz panel would otherwise license twice the hold the source's own
            // cadence can justify.
            pacing: Pacing::new(1_000_000_000 / u64::from(stream_hz), cfg.direct_playback),
            au_base_ns: None,
            cfg,
            backlog_cached: None,
            backlog_recent: std::collections::VecDeque::with_capacity(CUSHION_MIN_POLLS),
            cushion_frames: STANDING_CUSHION_FRAMES,
            last_backlog_poll: None,
            last_keyframe_request: None,
            hold_started: None,
            au_feed_us: 0,
            parts_fed: 0,
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
        self.pacing.reset();
        self.au_base_ns = None;
        self.clock.clear();
    }

    /// This frame's stamp in the sink's own clock domain.
    ///
    /// A sink with a clock has no PTS clock of its own (NDL counts from its load),
    /// so the host's capture PTS is mapped onto it ([`HostPtsAnchor`]) — which is also what keeps
    /// video and any audio plane in ONE timeline. A sink without one presents in feed order and
    /// the stamp is discarded at the feed, so the host PTS passes through untouched.
    /// This AU's stamp: computed on the piece that opens it and repeated for the rest — see
    /// [`Self::au_base_ns`]. `partial` is whether this piece leaves the AU open.
    fn au_stamp_ns(&mut self, frame_pts_ns: u64, partial: bool) -> u64 {
        let base = match self.au_base_ns {
            Some(open) => open,
            None => self.pts_base_ns(frame_pts_ns),
        };
        self.au_base_ns = partial.then_some(base);
        base
    }

    fn pts_base_ns(&mut self, frame_pts_ns: u64) -> u64 {
        match self.sink.clock() {
            Some(clock) => {
                let now = clock.now_ns();
                self.pacing.map(frame_pts_ns, now)
            }
            None => frame_pts_ns,
        }
    }

    fn begin_hold(&mut self) {
        self.stats.holding.store(true, Ordering::Relaxed);
        self.hold_started.get_or_insert_with(Instant::now);
    }

    /// The decode figure reported to the host's ABR controller. NDL's `play` is
    /// decode-AND-present in one opaque call, so `submit_us` (the whole AU's feed time) is
    /// *submission* time alone — a decoder quietly falling behind buffers frames internally and the
    /// feed stays fast, which left the controller's decode-rise signal (`abr::DECODE_RISE_US`,
    /// built precisely for "the decoder saturates before the link does") effectively
    /// blind on this client. The render-buffer backlog IS that standing decode queue, so
    /// it's folded in as queue-above-cushion × the drain interval (see [`decode_report_us`]).
    /// Polled on a cadence rather than every frame — three samples per 750 ms ABR report window is
    /// plenty, and assuming an NDL query is cheap enough for per-frame use is exactly the mistake
    /// docs/NOTES.md warns against; between polls the cached depth is reused.
    fn decode_us(&self, submit_us: u32, backlog: u64) -> u32 {
        decode_report_us(submit_us, backlog, self.cushion_frames, self.frame_interval_ns)
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

    /// What the live mapping has to say for itself — see [`PacingHealth`]. The whole point of
    /// publishing it on both mappings is that `late_stamps` makes them comparable.
    pub fn pacing_health(&self) -> PacingHealth {
        self.pacing.health()
    }

    /// Which mapping is stamping this session (`paced` / `direct`).
    pub fn pacing_label(&self) -> &'static str {
        self.pacing.label()
    }

    /// Audio-plane queue depth in ms, or `None` on a session with no plane — see
    /// `NdlVideo::audio_plane_lead_ms`. Here because it is a *video* symptom: the plane's depth is
    /// what NDL paces the picture on, so it belongs next to the backlog in the video heartbeat.
    pub fn audio_plane_lead_ms(&self) -> Option<i64> {
        self.audio_plane.as_deref().map(AudioPlane::lead_ms)
    }

    /// Pieces fed this session — see [`Self::parts_fed`]. Read against the completed-AU count.
    pub fn parts_fed(&self) -> u64 {
        self.parts_fed
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
            // Same reason as the `drop_open` below: the AU this was accumulating submission time
            // for is abandoned, so its cost must not be charged to whatever AU comes next. Its
            // stamp goes with it — the next AU is a new picture and maps itself.
            self.au_feed_us = 0;
            self.au_base_ns = None;
            return SinkResult::Held;
        };
        // Only a completed AU is a frame — a session feeding pieces would otherwise count one
        // picture several times, and the overlay reads this figure as pictures per second.
        if partial {
            self.parts_fed += 1;
        } else {
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
            // The AU this was accumulating for will never complete, so it must not be added to
            // whatever AU comes next — nor may its stamp be repeated onto one.
            self.au_feed_us = 0;
            self.au_base_ns = None;
        }
        result
    }

    fn feed(&mut self, au: &[u8], pts_ns: u64, flags: FrameFlags) -> SinkResult {
        let request_keyframe = match self.gate(&flags) {
            HoldGate::Skip(result) => return result,
            HoldGate::Feed { request_keyframe } => request_keyframe,
        };

        let base_ns = self.au_stamp_ns(pts_ns, flags.partial);
        // The submit instant, on CLOCK_REALTIME — the same basis the host stamps `pts_ns` with and
        // the skew handshake compares, so the two are directly subtractable. Taken BEFORE the feed
        // call: `play` blocks for its submission time, and the frame enters NDL's pipeline behind
        // exactly the frames the backlog below counts.
        let submit_realtime_ns = punktfunk_core::client::now_realtime_ns();
        let feed_start = Instant::now();
        let play_result = self.sink.feed(au, base_ns);
        let feed_elapsed = feed_start.elapsed();
        self.au_feed_us = self
            .au_feed_us
            .saturating_add(u32::try_from(feed_elapsed.as_micros()).unwrap_or(u32::MAX));
        // Read before the reset below, because BOTH consumers are per PICTURE: the overlay's
        // figure and the ABR submission term. Taking the last slice's `feed_elapsed` for the
        // latter reports a fraction of the AU's real cost on a slice-progressive session.
        let au_feed_us = self.au_feed_us;
        if !flags.partial {
            self.stats.feed_us.store(au_feed_us, Ordering::Relaxed);
            self.au_feed_us = 0;
        }
        if feed_elapsed >= FEED_BACKPRESSURE_WARN {
            tracing::warn!(
                "NDL slow: {:.1}ms (frame {}, pts {:.2}ms)",
                feed_elapsed.as_secs_f32() * 1000.0,
                flags.index,
                ms(base_ns),
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
                if self.pacing.ready_for_audio() {
                    self.clock.latch(base_ns as i64 - pts_ns as i64);
                }
                let decode_us = self
                    .cfg
                    .report_decode_latency
                    .then(|| self.decode_us(au_feed_us, backlog));
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
            ms(base_ns),
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
