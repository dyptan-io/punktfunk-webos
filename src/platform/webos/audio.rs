//! The TV's own audio device, and the sink wrapper the pipeline drives it through.
//!
//! One of three routes (`core::model::AudioRoutePref`) and the longest of them: software Opus
//! decode (`session::audio`) into a ring, drained by SDL's audio callback, through whatever
//! `PulseAudio` adds behind it. It is also the only one whose pacing behaviour is proven on this
//! hardware, hence the default.
//!
//! What used to live here — a `JitterPolicy` ring with an adaptive target, crossfaded drift sheds
//! and an `AvSync` estimator — was ~35 ms of floor on top of the device quantum, and it is gone.
//! What is left is a prime-then-serve ring; the decode half moved up into the pipeline's audio
//! stage, which now serves every route.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

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
                callbacks: 0,
            })
            .map_err(|e| anyhow::anyhow!("SDL open_playback: {e}"))?;
        let obtained = *device.spec();
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
        self.buffer_ms
            .store((self.ring.len() / self.per_ms.max(1)) as u32, Ordering::Relaxed);

        // Prime, then hold: serving from a ring shallower than one callback only produces a
        // sequence of half-empty callbacks, i.e. continuous crackle instead of one gap.
        if !self.primed {
            if self.ring.len() < self.prime_samples.max(out.len()) {
                out.fill(0.0);
                return;
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
        // ~10 s at this device quantum. The plane must not be invisible in a log: this route only
        // runs when the NDL plane was unavailable, which is already an unusual session.
        if self.callbacks % 1_000 == 0 {
            tracing::debug!(
                buffer_ms = self.ring.len() / self.per_ms.max(1),
                underruns = self.underruns,
                "audio playback (SDL device)"
            );
        }
    }
}
