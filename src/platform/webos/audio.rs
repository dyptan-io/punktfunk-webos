//! SDL2 callback playback of punktfunk's Opus audio packets. Decode only (Opus →
//! PCM) happens here — NDL is video-only (see ndl.rs docs), so this is a completely
//! separate path from the video decode/punch-through plane.
//!
//! **Two threads, one ring.** [`AudioFeed`] decodes on a dedicated thread and posts interleaved
//! f32 chunks down a bounded channel; [`RingCallback`] drains that channel into a ring it owns
//! outright and serves SDL's audio callback from it. Nothing locks, and nothing allocates in
//! steady state (drained chunk `Vec`s go back through a recycle channel for the feed to refill).
//! Same shape as `pf-client-core`'s `PipeWire` ring, for the same reasons.
//!
//! This replaced an `sdl2::audio::AudioQueue` pushed from the main loop. Two defects went with it:
//! the drain shared a thread with the UI's software rasterizer (`docs/NOTES.md` already named the
//! 500 ms stats-overlay raster as an underrun source on a 2-core panel), and `AudioQueue` offers
//! only `queue_audio`/`size`/`clear` — no partial drop — so the shared de-jitter policy's
//! crossfaded 5 ms shed was literally inexpressible against it. Its coarse corrections were a
//! whole-queue `clear()` (~100 ms of silence) and an uncrossfaded 5 ms discard.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::audio::{
    crossfade_drop, layout_for, AudioGapTracker, AudioSyncCell, AvSync, AvSyncObservation, JitterPolicy, JitterTuning,
};

/// 48 kHz, 5 ms frames — punktfunk's fixed audio framing (see punktfunk-core's
/// audio.rs doc comments and its `multistream_layout_roundtrips_with_channel_identity`
/// test, the canonical reference for both ends of this wire format).
const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 240;
/// Max channels punktfunk ever negotiates (7.1) — sizes the scratch decode buffer.
const MAX_CHANNELS: usize = 8;

/// Device buffer: 512 frames (10.67 ms) keeps slack vs callback cadence. Deliberately NOT lowered
/// further now that a real jitter policy owns the depth: a smaller quantum on a 2-3 core TV `SoC`
/// buys more wakeups and more missed callbacks, not less latency. [`WEBOS_TUNING`]'s base target is
/// sized to clear this quantum — see its note on the device floor.
const DEVICE_BUFFER_FRAMES: u16 = 512;

/// Chunks in flight between the decode thread and the audio callback. 64 × 5 ms = 320 ms of slack,
/// matching `pf-client-core`; the ring, not this channel, is what bounds latency.
const CHUNK_QUEUE: usize = 64;

/// The de-jitter preset for this platform. Not one of core's four (`PIPEWIRE`, `WASAPI`,
/// `COREAUDIO`, `AAUDIO`) — those are named for audio stacks, and this ring is shaped by the
/// device it runs on: a 2-3 core TV `SoC` whose UI rasterizes in software on the same silicon, over
/// a TV Wi-Fi radio `docs/NOTES.md` measures at a ~245 Mbps ceiling with 10-29 s black-hole stalls
/// on new flows.
///
/// Seeded from `JitterTuning::AAUDIO`, whose rationale is the closest match — we own the buffer,
/// and Wi-Fi power-save bunching arrives as underruns, i.e. crackle. Held locally rather than
/// upstreamed as a fifth preset until on-glass numbers justify specific values; the fields are
/// public and the struct is not `#[non_exhaustive]`, so this costs core nothing.
///
/// Two invariants checked by hand against these numbers, both of which the parent programme
/// shipped broken once:
///   * **The smooth shed fires strictly below the hard trim**, or drift correction is dead code and
///     every correction is the audible drop it was meant to replace. `shed_excess_ms()` is
///     `max(headroom/2, 2 × FRAME_MS)` = 20 ms, so the shed point is target+20 = 45 ms against a
///     trim point of `min(target+40, 120)` = 65 ms. 45 < 65.
///   * **The base target clears the device floor.** `effective_target` floors at `want + FRAME_MS`;
///     at [`DEVICE_BUFFER_FRAMES`] that is 10.67 + 5 = 15.7 ms, under the 25 ms base — so the ring
///     cannot oscillate prime → dropout → re-prime.
///
/// `deprime_ms` is a starvation window in MILLISECONDS of audio, not the count of consecutive short
/// reads it used to be. The count was the bug: a callback is not a unit of time, so the same number
/// meant a different fuse on every platform — 20 ms on iOS's 5 ms quantum against 44 ms on a Mac's
/// 11 ms, and upstream measured 120 audible gaps per 10 minutes at the short end versus 1-3 at the
/// long one. This device is not one of the badly bitten: at [`DEVICE_BUFFER_FRAMES`] the quantum is
/// 10.67 ms, so the old `5` was already a ~53 ms fuse. 60 ms is therefore close to what this ring
/// was doing, and is the value upstream picked for both Wi-Fi-transport presets — which is the
/// right company for a TV. A `MIN_DEPRIME_CALLBACKS` floor inside core keeps real hysteresis on a
/// large-quantum device, so this cannot de-prime on a single short read.
const WEBOS_TUNING: JitterTuning = JitterTuning {
    base_target_ms: 25,
    max_target_ms: 90,
    headroom_ms: 40,
    hard_cap_ms: 120,
    deprime_ms: 60,
};

/// The cells the A/V sync loop trades through, all owned by `NativeClient`: the video plane and
/// the clock handshake write the first two, this module writes the last two, and the stats overlay
/// reads them. They live there rather than here because neither plane owns the other — the video
/// sink cannot see the speaker, and the audio ring cannot see the glass.
pub struct SyncCells {
    /// Host-minus-client clock skew, live (re-synced mid-stream).
    pub clock_offset: Arc<AtomicI64>,
    /// The video plane's end-to-end latency in ns; `0` = nothing on the glass yet. Written by
    /// `session::sink`.
    pub video_e2e: Arc<AtomicU64>,
    /// Smoothed A/V offset in ms, positive = audio playing LATE. Published for the HUD.
    pub av_offset_ms: Arc<AtomicI64>,
    /// Decoded audio queued ahead of the speaker, in ms. Published for the HUD.
    pub buffer_ms: Arc<AtomicU32>,
}

/// The playback device. Holding it alive is the whole job — the ring drains on SDL's own audio
/// thread from construction until this is dropped.
pub struct AudioPlayer {
    device: sdl2::audio::AudioDevice<RingCallback>,
}

impl AudioPlayer {
    /// Opens the device and returns it alongside the [`AudioFeed`] that fills it. `channels` is
    /// host-resolved (the client MUST build its decoder from this, never from its own request).
    ///
    /// The two halves are returned separately because they belong on different threads: the device
    /// stays wherever SDL was initialised, and the feed moves to the decode thread.
    pub fn new(sdl_audio: &sdl2::AudioSubsystem, channels: u8, cells: SyncCells) -> Result<(Self, AudioFeed)> {
        let layout = layout_for(channels, false);
        let decoder = opus::MSDecoder::new(SAMPLE_RATE, layout.streams, layout.coupled, layout.mapping)
            .map_err(|e| anyhow::anyhow!("opus MSDecoder::new: {e}"))?;
        let spec = sdl2::audio::AudioSpecDesired {
            freq: Some(SAMPLE_RATE as i32),
            channels: Some(layout.channels),
            samples: Some(DEVICE_BUFFER_FRAMES),
        };
        let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(CHUNK_QUEUE);
        let (recycle_tx, recycle_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(CHUNK_QUEUE);
        let sync: Arc<AudioSyncCell> = Arc::default();
        let sync_cb = sync.clone();
        // Interleaved samples sitting in the chunk channel — decoded, not yet in the ring. Part of
        // what a packet queued now must wait behind, so it belongs in the A/V measurement; see
        // `AudioFeed::observe_av`. Balanced by construction: added once on a successful send,
        // subtracted once on receipt.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let in_flight_cb = in_flight.clone();
        let device = sdl_audio
            .open_playback(None, &spec, |obtained| {
                // Build the policy from what the device ACTUALLY negotiated, not what was asked
                // for: the policy's depths are denominated in interleaved samples, so a channel
                // count that differs here would silently scale every threshold.
                RingCallback {
                    rx: pcm_rx,
                    recycle: recycle_tx,
                    ring: VecDeque::new(),
                    policy: JitterPolicy::new(WEBOS_TUNING, obtained.channels),
                    sync: sync_cb,
                    in_flight: in_flight_cb,
                    underruns: 0,
                    sheds: 0,
                    callbacks: 0,
                }
            })
            .map_err(|e| anyhow::anyhow!("SDL open_playback: {e}"))?;
        let obtained = *device.spec();
        if obtained.channels != layout.channels || obtained.freq != SAMPLE_RATE as i32 {
            // Not fatal — SDL converts — but it means the decoder and the device disagree about
            // the frame shape, which is worth seeing in a log before it is heard.
            tracing::warn!(
                "audio device negotiated {}ch @{}Hz, stream is {}ch @{}Hz",
                obtained.channels,
                obtained.freq,
                layout.channels,
                SAMPLE_RATE,
            );
        }
        device.resume();
        let channels = layout.channels as usize;
        Ok((
            Self { device },
            AudioFeed {
                pcm_tx,
                recycle_rx,
                decoder,
                channels,
                per_ms: (SAMPLE_RATE / 1000) as usize * channels,
                gaps: AudioGapTracker::new(),
                av: AvSync::new(layout.channels),
                cells,
                sync,
                in_flight,
            },
        ))
    }

    /// The device's actually-negotiated spec — may differ from what was requested if
    /// the device doesn't support it exactly.
    pub fn spec(&self) -> &sdl2::audio::AudioSpec {
        self.device.spec()
    }
}

/// The decode half: Opus → PCM, loss concealment, the A/V measurement, and the hand-off to the
/// ring. Lives on its own thread (`session::audio_feed_pump`).
pub struct AudioFeed {
    pcm_tx: SyncSender<Vec<f32>>,
    /// Drained chunk `Vec`s coming back from the callback for reuse — the pool half of the chunk
    /// channel, so steady-state playback stops allocating (~200 chunks/s otherwise).
    recycle_rx: Receiver<Vec<f32>>,
    decoder: opus::MSDecoder,
    channels: usize,
    /// Interleaved samples per millisecond at the negotiated layout (48 × channels).
    per_ms: usize,
    /// Detects packets lost on the wire so they can be concealed rather than skipped.
    gaps: AudioGapTracker,
    /// A/V synchronisation, **measure-only for now** — see [`AudioFeed::observe_av`].
    av: AvSync,
    cells: SyncCells,
    /// Depth out of the callback.
    sync: Arc<AudioSyncCell>,
    /// Decoded samples handed off but not yet in the ring — see its construction site.
    in_flight: Arc<AtomicUsize>,
}

impl AudioFeed {
    /// Decodes one Opus packet (concealing anything lost immediately before it) and hands the PCM
    /// to the ring. Returns the peak sample — a diagnostic that separates "the host is sending
    /// silence" from "the speaker is not working".
    pub fn play(&mut self, seq: u32, pts_ns: u64, opus_payload: &[u8]) -> Result<f32> {
        self.observe_av(pts_ns, self.sync.depth(), self.in_flight.load(Ordering::Relaxed));

        // Conceal whatever went missing immediately before this packet. libopus PLC (decode with
        // empty input) interpolates a frame; the alternative is a hard gap, i.e. a click.
        for _ in 0..self.gaps.missing_before(seq) {
            let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
            let n = self
                .decoder
                .decode_float(&[], &mut pcm, false)
                .map_err(|e| anyhow::anyhow!("opus PLC decode: {e}"))?;
            self.push(&pcm[..n * self.channels]);
        }

        let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
        let samples_per_channel = self
            .decoder
            .decode_float(opus_payload, &mut pcm, false)
            .map_err(|e| anyhow::anyhow!("opus decode_float: {e}"))?;
        let decoded = &pcm[..samples_per_channel * self.channels];
        let peak = decoded.iter().fold(0f32, |m, &s| m.max(s.abs()));
        self.push(decoded);
        Ok(peak)
    }

    /// Hands one decoded chunk to the ring, reusing a pooled `Vec` where one is available.
    ///
    /// A full channel drops the chunk rather than blocking: this runs on the decode thread, and
    /// blocking it would back pressure into the transport. A wedged callback is already a fault
    /// the ring's own underrun path reports.
    fn push(&mut self, pcm: &[f32]) {
        let mut buf = self.recycle_rx.try_recv().unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(pcm);
        let n = buf.len();
        // Counted only on a successful send, so a dropped chunk cannot strand samples in the
        // in-flight tally forever.
        if self.pcm_tx.try_send(buf).is_ok() {
            self.in_flight.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Fold one packet into the A/V offset measurement, and publish both the offset and the ring
    /// depth for the stats overlay.
    ///
    /// **Measure-only, deliberately: nothing steers.** The offset and the ring depth are
    /// published — they are the instrument, and a latency report with no instrument behind it is
    /// what this whole programme exists to end — but no target is ever posted back to
    /// [`JitterPolicy`], so `AvSync::desired_depth` goes uncalled.
    ///
    /// The blocker is the video reference: `session::sink::video_e2e_ns` omits a term NDL never
    /// reports, which biases this offset high by that whole term — its docs carry the arithmetic
    /// and what arming the loop would take.
    ///
    /// **Two depths, and they are not interchangeable.** `ring_depth` is what the audio callback
    /// owns and the only thing [`JitterPolicy`] can actually move. `in_flight` is decoded PCM
    /// already handed off but still in the chunk channel — invisible to the policy, but just as
    /// real to a listener, since it must play before anything queued now does. The *measurement*
    /// uses the sum (that is the packet's true wait); the *correction* is expressed against the
    /// ring alone, because the policy interprets its target in ring samples. Handing the sum to
    /// `desired_depth` would inflate every target by the channel's contents.
    fn observe_av(&mut self, pts_ns: u64, ring_depth: usize, in_flight: usize) {
        let buffered_ahead = ring_depth + in_flight;
        // Published unconditionally — the depth is worth seeing whether or not the loop is acting,
        // and it is what makes an "audio is late" report triageable at all.
        self.cells
            .buffer_ms
            .store((buffered_ahead / self.per_ms.max(1)) as u32, Ordering::Relaxed);
        let video_e2e = self.cells.video_e2e.load(Ordering::Relaxed);
        self.av.observe(AvSyncObservation {
            pts_ns,
            now_local_ns: punktfunk_core::client::now_realtime_ns(),
            clock_offset_ns: self.cells.clock_offset.load(Ordering::Relaxed),
            buffered_ahead,
            // 0 = nothing on the glass yet; with no reference there is nothing to measure against.
            video_e2e_ns: (video_e2e > 0).then_some(video_e2e),
        });
        self.cells
            .av_offset_ms
            .store(i64::from(self.av.offset_ms()), Ordering::Relaxed);
    }
}

/// The playback half, owned by SDL's audio thread. Drains the chunk channel into a ring it owns
/// outright, then serves the callback from it under the shared de-jitter policy.
struct RingCallback {
    rx: Receiver<Vec<f32>>,
    recycle: SyncSender<Vec<f32>>,
    ring: VecDeque<f32>,
    /// Prime depth in MILLISECONDS, an underrun-driven adaptive floor, and a crossfaded 5 ms drift
    /// shed so latency returns to target instead of ratcheting. See [`WEBOS_TUNING`].
    policy: JitterPolicy,
    /// Depth out to the decode thread, target in.
    sync: Arc<AudioSyncCell>,
    /// Samples still in the chunk channel; decremented as they are drained into the ring.
    in_flight: Arc<AtomicUsize>,
    underruns: u64,
    sheds: u64,
    callbacks: u64,
}

impl sdl2::audio::AudioCallback for RingCallback {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        while let Ok(mut chunk) = self.rx.try_recv() {
            self.in_flight.fetch_sub(chunk.len(), Ordering::Relaxed);
            self.ring.extend(chunk.iter().copied());
            // Return the drained Vec to the pool; a full/closed pool just drops it.
            chunk.clear();
            let _ = self.recycle.try_send(chunk);
        }

        // `out` is interleaved samples for this callback — exactly the `want` the policy is
        // denominated in.
        let want = out.len();
        // Take whatever depth the decode thread's sync loop last asked for, and publish where the
        // ring actually is so it can measure the result. `None` (nothing armed, or inside the
        // deadband) reproduces the un-synced behaviour exactly.
        self.policy.set_sync_target(self.sync.target());
        self.sync.publish_depth(self.ring.len());
        let step = self.policy.step(self.ring.len(), want);
        if step.drop_front > 0 {
            self.sheds += 1;
            crossfade_drop(&mut self.ring, step.drop_front, step.crossfade);
        }

        let mut ran_short = false;
        for slot in out.iter_mut() {
            *slot = if step.silence {
                0.0
            } else {
                self.ring.pop_front().unwrap_or_else(|| {
                    ran_short = true;
                    0.0
                })
            };
        }
        // No-op while un-primed (the policy ignores it), so a deliberate priming silence is never
        // miscounted as an underrun.
        self.policy.note_read(ran_short);
        self.underruns += u64::from(ran_short);
        self.callbacks += 1;
        // ~10 s at this device quantum. The exact cadence does not matter; that the plane stops
        // being invisible does — before this, the only audio signal in a log was a per-packet
        // "underrun" line from the feed side, which said the queue was empty without saying how
        // deep it was supposed to be.
        if self.callbacks % 1_000 == 0 {
            tracing::debug!(
                buffer_ms = self.policy.avg_depth_ms(),
                target_ms = self.policy.target_ms(),
                underruns = self.underruns,
                drift_sheds = self.sheds,
                "audio playback"
            );
        }
    }
}
