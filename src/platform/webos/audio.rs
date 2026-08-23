//! The TV's own audio device, and the sink wrapper the pipeline drives it through.
//!
//! One of two routes (`core::model::AudioRoutePref`) and the longer of them: software Opus
//! decode (`session::audio`) into a ring, drained by SDL's audio callback, through whatever
//! `PulseAudio` adds behind it. It is also the only one whose pacing behaviour is proven on this
//! hardware, hence the default — the offload route is shorter, but the TV's own decoder is behind
//! the plane and some sets accept its load and then play nothing.
//!
//! What used to live here — a `JitterPolicy` ring with an adaptive target, crossfaded drift sheds
//! and an `AvSync` estimator — was ~35 ms of floor on top of the device quantum, and it is gone.
//! What is left is a prime-then-serve ring; the decode half moved up into the pipeline's audio
//! stage, which serves both routes.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::core::media::{AudioFormat, AudioSink, Samples};
use crate::session::audio::SAMPLE_RATE;

/// Device buffer: 512 frames (10.67 ms). Deliberately not smaller — a smaller quantum on a 2-3
/// core TV `SoC` buys more wakeups and more missed callbacks, not less latency (docs/NOTES.md).
const DEVICE_BUFFER_FRAMES: u16 = 512;

/// Chunks in flight between the decode thread and the callback. 5 ms each.
const CHUNK_QUEUE: usize = 64;

/// Depth the ring primes to before the first sample plays, and the floor it tries to hold. A fixed
/// number, not an adaptive target: the adaptive one belonged to a path with an A/V estimator
/// behind it, and this one only has to not crackle.
const PRIME_MS: usize = 25;

/// Ring ceiling: above this the oldest audio is dropped back to [`PRIME_MS`].
///
/// Nothing else bounds this ring — the callback pops exactly one quantum per wake, so anything
/// the producer runs ahead by STAYS ahead. A host stall that unblocks into a burst therefore used
/// to buy the session a permanent lip-sync debt of however long the stall was, with the picture
/// still perfectly paced (the clock plane drives that, not this). One audible trim beats carrying
/// it to the end of the session.
const MAX_MS: usize = 90;

/// How often an over-ceiling ring may be trimmed. Slow drift is the other way the ring grows
/// (host capture clock vs. this device's), and a trim per callback would turn that into a
/// continuous rasp instead of one skip every few seconds.
const TRIM_INTERVAL: Duration = Duration::from_secs(2);

/// The playback device. Holding it alive is the whole job — the ring drains on SDL's own
/// audio thread from construction until this is dropped. See the module docs for when it is used
/// at all.
pub struct AudioPlayer {
    device: sdl2::audio::AudioDevice<RingCallback>,
}

impl AudioPlayer {
    /// Opens the device and returns it alongside the [`SdlAudioSink`] that fills it. `channels` is
    /// host-resolved (the pipeline MUST build its decoder from this, never from its own request).
    /// `buffer_ms` is where the ring publishes its depth for the stats overlay.
    ///
    /// The two halves are returned separately because they belong on different threads: the device
    /// stays wherever SDL was initialised, and the sink moves to the audio thread.
    pub fn new(
        sdl_audio: &sdl2::AudioSubsystem,
        channels: u8,
        buffer_ms: Arc<AtomicU32>,
    ) -> Result<(Self, SdlAudioSink)> {
        let spec = sdl2::audio::AudioSpecDesired {
            freq: Some(SAMPLE_RATE as i32),
            channels: Some(channels),
            samples: Some(DEVICE_BUFFER_FRAMES),
        };
        let depth_cell = buffer_ms.clone();
        let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(CHUNK_QUEUE);
        let (recycle_tx, recycle_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(CHUNK_QUEUE);
        let device = sdl_audio
            .open_playback(None, &spec, |obtained| RingCallback {
                rx: pcm_rx,
                recycle: recycle_tx,
                ring: VecDeque::new(),
                // Denominated in interleaved samples, so it is built from what the device actually
                // negotiated rather than what was asked for.
                prime_samples: PRIME_MS * (SAMPLE_RATE as usize / 1000) * obtained.channels as usize,
                per_ms: (SAMPLE_RATE as usize / 1000) * obtained.channels as usize,
                primed: false,
                buffer_ms,
                underruns: 0,
                dropped_ms: 0,
                last_trim: None,
                callbacks: 0,
            })
            .map_err(|e| anyhow::anyhow!("SDL open_playback: {e}"))?;
        let obtained = *device.spec();
        // The device quantum is the other half of this route's latency, and SDL is free to
        // negotiate something other than what was asked for — a larger one also silently RAISES
        // the effective prime, which is `max(PRIME_MS, one callback)`. Unlogged, there was no way
        // to tell from a session log where the software route's buffering actually went.
        tracing::info!(
            "SDL audio device: {}ch @{}Hz, {} frame(s) per callback ({:.1}ms), format {:?}",
            obtained.channels,
            obtained.freq,
            obtained.samples,
            f64::from(obtained.samples) * 1000.0 / f64::from(obtained.freq.max(1)),
            obtained.format,
        );
        if obtained.channels != channels || obtained.freq != SAMPLE_RATE as i32 {
            // Not fatal — SDL converts — but it means the decoder and the device disagree about
            // the frame shape, which is worth seeing in a log before it is heard.
            tracing::warn!(
                "audio device negotiated {}ch @{}Hz, stream is {}ch @{}Hz",
                obtained.channels,
                obtained.freq,
                channels,
                SAMPLE_RATE,
            );
        }
        device.resume();
        Ok((
            Self { device },
            SdlAudioSink {
                pcm_tx,
                recycle_rx: std::sync::Mutex::new(recycle_rx),
                channels,
                buffer_ms: depth_cell,
            },
        ))
    }

    /// The device's actually-negotiated spec — may differ from what was requested if
    /// the device doesn't support it exactly.
    pub fn spec(&self) -> &sdl2::audio::AudioSpec {
        self.device.spec()
    }
}

/// The feed half of the device, as the pipeline sees it: hand it f32 samples in punktfunk's own
/// channel order and they reach SDL's callback with no conversion anywhere.
///
/// Timestamps are ignored — this device has no timeline to land on, it plays what it is given in
/// the order it arrives. That is the whole difference between this route and the two that ride
/// NDL's plane, and the reason the pipeline stamps every route the same way regardless.
pub struct SdlAudioSink {
    pcm_tx: SyncSender<Vec<f32>>,
    /// Drained chunk `Vec`s coming back from the callback for reuse, so steady-state playback
    /// stops allocating (~200 chunks/s otherwise). Behind a `Mutex` only because `AudioSink` is
    /// shared: one thread feeds it, and the lock is never contended.
    recycle_rx: std::sync::Mutex<Receiver<Vec<f32>>>,
    channels: u8,
    /// Where the ring publishes its depth, so [`AudioSink::depth_ms`] can read it back.
    buffer_ms: Arc<AtomicU32>,
}

impl AudioSink for SdlAudioSink {
    fn name(&self) -> &'static str {
        "SDL device"
    }

    fn format(&self) -> AudioFormat {
        AudioFormat::PcmF32 {
            channels: self.channels,
            sample_rate: SAMPLE_RATE,
        }
    }

    /// Hands one decoded chunk to the ring, reusing a pooled `Vec` where one is available.
    ///
    /// A full channel drops the chunk rather than blocking: this runs on the audio thread, and
    /// blocking it would back-pressure into the transport.
    fn feed(&self, samples: Samples<'_>, _host_pts_ns: u64) -> Result<()> {
        let Samples::F32(pcm) = samples else {
            anyhow::bail!("SDL device takes f32 samples only");
        };
        let mut buf = self
            .recycle_rx
            .lock()
            .map_or_else(|_| Vec::new(), |rx| rx.try_recv().unwrap_or_default());
        buf.clear();
        buf.extend_from_slice(pcm);
        let _ = self.pcm_tx.try_send(buf);
        Ok(())
    }

    fn depth_ms(&self) -> Option<i64> {
        Some(i64::from(self.buffer_ms.load(Ordering::Relaxed)))
    }
}

/// The playback half, owned by SDL's audio thread: drain the channel into a ring, prime once, then
/// serve. No adaptive target and no drift shed — see the module docs for why the machinery that
/// had those is gone.
struct RingCallback {
    rx: Receiver<Vec<f32>>,
    recycle: SyncSender<Vec<f32>>,
    ring: VecDeque<f32>,
    /// Interleaved samples the ring must hold before the first one plays, and again before it
    /// resumes after running dry.
    prime_samples: usize,
    /// Interleaved samples per millisecond, for the depth the overlay reads.
    per_ms: usize,
    primed: bool,
    buffer_ms: Arc<AtomicU32>,
    underruns: u64,
    /// Audio discarded to the ceiling — the counter that distinguishes "the host burst once" from
    /// "the clocks are drifting", which look identical from the depth alone.
    dropped_ms: u64,
    /// When the ring was last trimmed, so [`TRIM_INTERVAL`] can space the drift case out.
    last_trim: Option<Instant>,
    callbacks: u64,
}

impl sdl2::audio::AudioCallback for RingCallback {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        while let Ok(mut chunk) = self.rx.try_recv() {
            self.ring.extend(chunk.iter().copied());
            // Return the drained Vec to the pool; a full/closed pool just drops it.
            chunk.clear();
            let _ = self.recycle.try_send(chunk);
        }
        self.trim_overrun();
        self.buffer_ms
            .store((self.ring.len() / self.per_ms.max(1)) as u32, Ordering::Relaxed);

        // Prime, then hold: serving from a ring shallower than one callback only produces a
        // sequence of half-empty callbacks, i.e. continuous crackle instead of one gap.
        if !self.primed {
            let target = self.prime_samples.max(out.len());
            if self.ring.len() < target {
                out.fill(0.0);
                return;
            }
            // Serve from EXACTLY the prime depth, not from whatever crossed it. The ring is only
            // inspected once per callback, so the depth at this edge overshoots the target by up
            // to one callback plus one 5 ms chunk — and nothing regulates it back down afterwards
            // ([`MAX_MS`] is a 90 ms ceiling, not a target), so that overshoot is standing latency
            // for the rest of the session. Dropping it here is free: not one sample has played yet
            // on the first prime, and a re-prime follows an underrun, which is already the gap.
            let excess = self.ring.len() - target;
            if excess > 0 {
                self.ring.drain(..excess);
                tracing::debug!(
                    excess_ms = excess / self.per_ms.max(1),
                    depth_ms = target / self.per_ms.max(1),
                    "audio ring primed — dropped the overshoot before the first sample",
                );
            }
            self.primed = true;
        }

        let mut ran_short = false;
        for slot in out.iter_mut() {
            *slot = self.ring.pop_front().unwrap_or_else(|| {
                ran_short = true;
                0.0
            });
        }
        if ran_short {
            self.underruns += 1;
            self.primed = false;
        }
        self.callbacks += 1;
        // ~10 s at this device quantum — the depth this route's lip sync is made of, and the
        // only place it is observable.
        if self.callbacks % 1_000 == 0 {
            tracing::debug!(
                buffer_ms = self.ring.len() / self.per_ms.max(1),
                underruns = self.underruns,
                dropped_ms = self.dropped_ms,
                "audio playback (SDL device)"
            );
        }
    }
}

impl RingCallback {
    /// Drop the oldest audio back to [`PRIME_MS`] when the ring is over [`MAX_MS`] — see the
    /// constant for why nothing else would.
    ///
    /// Dropped from the FRONT: the newest audio is the one that belongs with the picture on
    /// screen, and the ring's depth IS this route's lip-sync offset.
    fn trim_overrun(&mut self) {
        let per_ms = self.per_ms.max(1);
        if self.ring.len() <= MAX_MS * per_ms {
            return;
        }
        if self.last_trim.is_some_and(|t| t.elapsed() < TRIM_INTERVAL) {
            return;
        }
        self.last_trim = Some(Instant::now());
        let keep = PRIME_MS * per_ms;
        let drop = self.ring.len() - keep;
        self.ring.drain(..drop);
        self.dropped_ms += (drop / per_ms) as u64;
        tracing::debug!(
            dropped_ms = drop / per_ms,
            total_ms = self.dropped_ms,
            "audio ring over {MAX_MS}ms — trimmed to the prime depth",
        );
    }
}
