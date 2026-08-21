//! The threads that drain the transport: video into the sink, audio into NDL or the
//! software playback ring.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::client::{AudioPacket, NativeClient};
use punktfunk_core::packet::{FLAG_SOF, USER_FLAG_RECOVERY_ANCHOR};
use punktfunk_core::PunktfunkError;

use crate::platform::webos::audio::{AudioFeed, PcmFeed};
use crate::platform::webos::device::boost_current_thread;
use crate::platform::webos::ndl::NdlVideo;
use crate::session::join::{join_with_timeout, SHUTDOWN_JOIN_TIMEOUT};
use crate::session::priority::{boost_hot_threads, spawn_vendor_decode_thread_renicer};
use crate::session::sink::{FrameFlags, NdlSink, SinkResult};
use crate::session::StreamStats;

/// Longest a `next_frame` call parks before the loop re-checks `stop`.
const FRAME_WAIT: Duration = Duration::from_millis(500);
/// Cadence of the pump's liveness check: refreshes the overlay's backlog figure, and is the
/// only place a "nothing is arriving" line can come from.
const HEARTBEAT: Duration = Duration::from_secs(2);
/// How often the heartbeat's detail line reaches the log. The line is a trend ("still
/// draining, still not holding"), and one every couple of seconds buried the rest of the log
/// saying nothing new.
const VIDEO_LOG_INTERVAL: Duration = Duration::from_secs(15);

/// A stamp that fires once per `interval` and re-arms itself.
struct Tick {
    interval: Duration,
    last: Instant,
}

impl Tick {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: Instant::now(),
        }
    }

    fn due(&mut self) -> bool {
        let ready = self.last.elapsed() >= self.interval;
        if ready {
            self.last = Instant::now();
        }
        ready
    }
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
/// With `frame_parts` off — the default, and every session on a backend that isn't NDL v2 — every
/// delivery carries `part: None` and this is a pass-through. With it on, AU prefixes arrive while
/// the rest is still on the wire, and the decoder gets a frame's first bytes without waiting for
/// its last datagram. That wait is a real slice of a frame period at high bitrate and it is pure
/// latency: none of it is decode work.
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
    fn step(&mut self, frame: &punktfunk_core::session::Frame) -> PartStep {
        let Some(part) = frame.part else {
            // Whole-AU delivery: `frame_parts` is off, or this is an aged-out chunk-aligned
            // partial, which core hands over as one buffer either way.
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
                tracing::warn!("frame parts: AU {} starts over an unfinished one", frame.frame_index);
            }
            self.open = (!part.last).then_some((frame.frame_index, len));
            return PartStep::Feed {
                partial: !part.last,
                lost_parts,
            };
        }
        match self.open {
            Some((index, next)) if index == frame.frame_index && next == part.offset => {
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
                        frame.frame_index,
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

/// Drives the video thread: transport → [`NdlSink`], plus the counters and the loss/HDR
/// side-channels that ride the same loop.
struct VideoPump {
    client: Arc<NativeClient>,
    sink: NdlSink,
    stats: Arc<StreamStats>,
    /// Whether the host's per-content HDR metadata is worth draining — false on every session
    /// where nothing would apply it (see `connect`'s `is_hdr`).
    is_hdr: bool,
    /// Core's cumulative drop count as of the last frame, to edge-detect new drops.
    last_dropped_seen: u64,
    heartbeat: Tick,
    video_log: Tick,
    /// Slice-progressive reassembly state — a pass-through unless the session opted into
    /// `frame_parts` (see [`AuParts`]).
    parts: AuParts,
}

impl VideoPump {
    fn new(client: Arc<NativeClient>, sink: NdlSink, stats: Arc<StreamStats>, is_hdr: bool) -> Self {
        let last_dropped_seen = client.frames_dropped();
        Self {
            client,
            sink,
            stats,
            is_hdr,
            last_dropped_seen,
            heartbeat: Tick::new(HEARTBEAT),
            video_log: Tick::new(VIDEO_LOG_INTERVAL),
            parts: AuParts::default(),
        }
    }

    fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            match self.client.next_frame(FRAME_WAIT) {
                Ok(frame) => self.on_frame(&frame),
                Err(PunktfunkError::NoFrame) => {
                    if self.heartbeat.due() {
                        // INFO for the same reason as the main heartbeat — and this arm is the
                        // one that says "nothing is arriving at all", which is a different fault
                        // from "arriving but not presenting".
                        tracing::info!("video: {} frames (idle)", self.frames());
                    }
                }
                // A teardown the user asked for reaches both pumps as `Closed`, so it is not an
                // error in either — the audio pump already logged it at INFO.
                Err(PunktfunkError::Closed) => {
                    tracing::info!("video pump ending: session closed");
                    break;
                }
                Err(e) => {
                    tracing::error!("video pump: {e:#}");
                    break;
                }
            }
            self.forward_hdr_meta();
        }
    }

    /// Frames taken off the transport this session — the counter the overlay reads.
    fn frames(&self) -> u64 {
        self.stats.frames.load(Ordering::Relaxed)
    }

    fn on_frame(&mut self, frame: &punktfunk_core::session::Frame) {
        self.stats.bytes.fetch_add(frame.data.len() as u64, Ordering::Relaxed);
        self.heartbeat();

        // A piece of an AU whose predecessor went missing has nowhere to go: the decoder holds a
        // truncated input and only a re-anchor clears it. `Discard` says so; `Feed` carries whether
        // the pieces so far leave this AU decodable.
        let PartStep::Feed { partial, lost_parts } = self.parts.step(frame) else {
            return;
        };
        // Only a completed AU is a frame — a session feeding pieces would otherwise count one
        // picture several times, and the overlay reads this figure as pictures per second.
        if !partial {
            self.stats.frames.fetch_add(1, Ordering::Relaxed);
        }

        let loss = self.note_loss(frame) || lost_parts;
        let flags = FrameFlags {
            reanchor: frame.flags & u32::from(FLAG_SOF) != 0 || frame.flags & USER_FLAG_RECOVERY_ANCHOR != 0,
            loss,
            index: u64::from(frame.frame_index),
            partial,
        };
        match self.sink.submit(&frame.data, frame.pts_ns, flags) {
            SinkResult::Presented { decode_us } => {
                if let Some(us) = decode_us {
                    self.client.report_decode_us(us);
                }
            }
            // Either the piece never reached the decoder (a hold swallows it, an error refused
            // it) or it did and a keyframe has just been asked for regardless — on both paths the
            // AU cannot be completed. Forgetting it costs the rest of one AU; keeping it would
            // eventually feed a frame with a hole in it.
            SinkResult::Held => self.parts.drop_open(),
            SinkResult::NeedKeyframe => {
                self.parts.drop_open();
                if let Err(e) = self.client.request_keyframe() {
                    tracing::warn!("request_keyframe: {e:#}");
                }
            }
        }
    }

    /// Refreshes the overlay's backlog figure, and on a slower cadence logs the pump's state.
    fn heartbeat(&mut self) {
        if !self.heartbeat.due() {
            return;
        }
        let backlog = self.sink.poll_backlog_depth();
        self.stats
            .render_backlog
            .store(backlog.unwrap_or(-1), Ordering::Relaxed);
        // `backlog` separates "the decoder is behind" from "frames are arriving late" —
        // indistinguishable before this, since play() decodes and presents in one opaque call.
        // Logged on its own slower cadence: the overlay wants a fresh depth, the log does not.
        //
        // DEBUG, so it costs a telemetry listener or `TELEMETRY_LEVEL=debug` to see — the
        // on-device file sink is INFO-only (`logger::resolved_level`).
        if self.video_log.due() {
            tracing::debug!(
                "video: {} frames, holding={}, dropped={}, backlog={}, pts_trim={}ms",
                self.frames(),
                self.sink.holding(),
                self.client.frames_dropped(),
                backlog.map_or_else(|| "n/a".to_string(), |b| b.to_string()),
                self.sink.pts_trimmed_ms(),
            );
        }
    }

    /// Whether loss reaches this frame — a sequence gap, or a frame the transport gave up on.
    fn note_loss(&mut self, frame: &punktfunk_core::session::Frame) -> bool {
        // From core v0.28 this returns the gap WIDTH (0 = contiguous) where it used to return a
        // bare "was there a gap" bool; `> 0` is the same predicate. Keep the width for the log
        // line — how many frames the hole swallowed is the number worth having when reading a
        // freeze report, not merely that one existed.
        let gap_width = self.client.note_frame_index(frame.frame_index);
        let dropped_now = self.client.frames_dropped();
        let dropped = dropped_now > self.last_dropped_seen;
        self.last_dropped_seen = dropped_now;
        let lost = gap_width > 0 || dropped;
        if lost && !self.sink.holding() {
            // Logged alongside the freeze the sink reports next: a sequence hole and a frame the
            // transport itself gave up on point at different faults.
            tracing::warn!("loss: gap={gap_width} dropped={dropped} (frame {})", frame.frame_index);
        }
        lost
    }

    /// Hands the decoder any per-content HDR mastering metadata the host has sent.
    fn forward_hdr_meta(&mut self) {
        if !self.is_hdr {
            return;
        }
        // Freshly *received* is not the same as changed: the host re-sends unchanged mastering
        // metadata (three identical packets inside 10 ms on a CX), so the on-change filter has to
        // run against the last value applied. The player does that.
        let Ok(meta) = self.client.next_hdr_meta(Duration::ZERO) else {
            return;
        };
        tracing::info!(
            "HDR metadata received: primaries={:?} white={:?} max_dml={} min_dml={} max_cll={} max_fall={}",
            meta.display_primaries,
            meta.white_point,
            meta.max_display_mastering_luminance,
            meta.min_display_mastering_luminance,
            meta.max_cll,
            meta.max_fall,
        );
        if let Err(e) = self.sink.set_color_info(Some(&meta), self.client.color) {
            tracing::warn!("NDL set_color_info: {e:#}");
        }
    }
}

/// The video thread's body: boost the threads that carry the stream, then pump until `stop`.
// The thread body owns everything it is handed — the `Arc`s die with it, which is what keeps
// the client and the stats alive for exactly as long as the pump runs.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn video_pump(
    client: Arc<NativeClient>,
    sink: NdlSink,
    stop: Arc<AtomicBool>,
    stats: Arc<StreamStats>,
    is_hdr: bool,
) {
    client.register_hot_thread();
    boost_hot_threads(&client);
    spawn_vendor_decode_thread_renicer();
    VideoPump::new(client, sink, stats, is_hdr).run(&stop);
}

/// How long an audio drain parks on an empty plane before re-checking `stop`.
const AUDIO_WAIT: Duration = Duration::from_millis(100);

/// The shared body of both audio threads: pull packets, hand each to `play`, exit on `stop` or
/// a closed plane.
///
/// A thread of its own on either path, not a drain bolted onto another loop. Bolted onto the
/// video pump (where the offloaded path first lived) audio only drained after a `next_frame`
/// call that blocks up to [`FRAME_WAIT`], so a video drought — an encoder stall on the host, a
/// loss hold — chopped audio into ≤500 ms stalls *with packets already waiting*, and in normal
/// flow packets drained in per-video-frame clumps that all took the same drain-time PTS. Bolted
/// onto the main loop (where the software path lived, forced by `sdl2::audio::AudioQueue` being
/// `!Send`) it sat behind the UI's software rasterizer on a 2-3 core panel, and `docs/NOTES.md`
/// already named the 500 ms stats-overlay raster as an underrun source because of it. Core's
/// `next_audio` docs ask for exactly this thread ("packets arrive every 5 ms"), and its pull
/// methods are one-thread-per-plane safe by contract.
fn audio_drain(client: &NativeClient, stop: &AtomicBool, what: &str, mut play: impl FnMut(&AudioPacket)) {
    // Same boost the video pump requests for itself — 5 ms packets are the most
    // latency-sensitive cadence in the session.
    boost_current_thread();
    while !stop.load(Ordering::Relaxed) {
        match client.next_audio(AUDIO_WAIT) {
            Ok(packet) => play(&packet),
            Err(PunktfunkError::NoFrame) => {}
            Err(e) => {
                tracing::info!("{what} ending: {e:#}");
                break;
            }
        }
    }
}

/// Drains raw Opus packets straight into NDL, for the offloaded path.
///
/// Teardown safety: this thread holds one of the two `Arc<NdlVideo>` owners, so the
/// process-global NDL unload in `NdlVideo::drop` cannot run until this thread has
/// exited — `NDL_DirectAudioPlay` can never race the unload, whichever thread
/// `Connected::shutdown` happens to join first.
/// A gap starves NDL's pacing clock, but `run_clock_plane` watches the same plane and fills in —
/// see its `yields_to_real`.
pub(super) fn ndl_audio_pump(client: &NativeClient, ndl: &NdlVideo, stop: &AtomicBool) {
    audio_drain(client, stop, "audio pump", |packet| {
        if let Err(e) = ndl.play_audio(&packet.data, packet.pts_ns) {
            tracing::warn!("NDL audio error (seq {}): {e:#}", packet.seq);
        }
    });
}

/// Decodes Opus here and feeds the PCM to NDL's own sink, for the `NdlAudioConfig::Pcm` path.
///
/// The middle ground between the two older routes, and the shortest audio leg of the three: the
/// software decoder still runs (so concealment, 5.1 layouts and every core fix stay in play) but
/// the samples go to the same hardware clock as the picture instead of through the SDL ring and
/// device buffer. Same teardown safety as [`ndl_audio_pump`] — the thread holds an `Arc<NdlVideo>`,
/// so no unload can race the feed.
pub(super) fn ndl_pcm_audio_pump(client: &NativeClient, ndl: &NdlVideo, stop: &AtomicBool, feed: &mut PcmFeed) {
    audio_drain(client, stop, "audio pcm", |packet| {
        let (pcm, lead_ms) = match feed.decode(packet.seq, &packet.data) {
            Ok(decoded) => decoded,
            Err(e) => {
                tracing::warn!("audio decode error (seq {}): {e:#}", packet.seq);
                return;
            }
        };
        // Concealment sits in the hole BEFORE this packet, so the buffer starts that much earlier
        // than the packet's own stamp. `play_audio`'s ceiling floors anything that lands under the
        // audio already fed, which is exactly the right answer for a hole being filled.
        let pts_ns = packet.pts_ns.saturating_sub(lead_ms as u64 * 1_000_000);
        if let Err(e) = ndl.play_audio(pcm, pts_ns) {
            tracing::warn!("NDL audio error (seq {}): {e:#}", packet.seq);
        }
    });
}

/// Spawns the software decode/feed thread (`audio_feed_pump`) and returns its handle.
pub fn spawn_audio_feed(
    client: Arc<NativeClient>,
    mut feed: AudioFeed,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("punktfunk-webos-audio".into())
        .spawn(move || audio_feed_pump(&client, &mut feed, &stop))
        .context("spawn audio feed thread")
}

/// Joins the audio feed thread, bounded by the same timeout every other teardown join uses — a
/// thread wedged in an Opus decode must not hold the whole app on the way back to the menu.
/// Software Opus → SDL2, not NDL, so a wedge here needs no `ndl::poison()`.
pub fn join_audio_feed(handle: std::thread::JoinHandle<()>) -> bool {
    join_with_timeout(handle, SHUTDOWN_JOIN_TIMEOUT, "audio-feed", || ())
}

/// Pulls Opus packets off the transport, decodes them, and hands the PCM to the playback ring.
fn audio_feed_pump(client: &NativeClient, feed: &mut AudioFeed, stop: &AtomicBool) {
    let mut packets: u32 = 0;
    audio_drain(client, stop, "audio feed", |packet| {
        match feed.play(packet.seq, packet.pts_ns, &packet.data) {
            Ok(peak) => {
                packets = packets.wrapping_add(1);
                // ~15s, matching the video heartbeat (packets are 5ms each).
                if packets % 3_000 == 0 {
                    tracing::debug!("audio peak: {peak:.4}");
                }
            }
            // Underruns and drift sheds are reported by the ring itself, which is the only
            // side that knows the depth — see `platform::webos::audio`'s callback.
            Err(e) => tracing::warn!("audio error (seq {}): {e:#}", packet.seq),
        }
    });
}
