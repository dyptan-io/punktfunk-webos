//! A ring between the audio stage and a hardware audio plane, drained on the plane's own cadence.
//!
//! **Why this exists.** NDL paces the picture against the audio plane's queue depth, so whatever
//! decides that depth decides how smooth the video is. Feeding the plane as packets arrive makes
//! the depth a function of network jitter — which is exactly the intermittent stutter the PCM
//! route showed, while the silent metronome (a fixed 20 ms tick topping the plane up to a constant
//! lead) and the Opus route (whose decoder is a queue of the TV's own behind the plane) both stay
//! smooth.
//!
//! So this route stops feeding on arrival. The pump decodes into a ring; a feeder thread runs on
//! the metronome's cadence and tops the plane up to [`AudioPlane::target_lead_ms`] with real
//! samples, padding the plane's own silence when the ring is dry. The plane then sees the same
//! metronomic depth it sees on the software route, and the jitter lands in the ring instead.
//!
//! **What it costs.** One prime's worth of buffer ([`PRIME_MS`]) and a lip-sync offset that is
//! constant rather than mapped: stamps come off the plane's clock, not the host timeline. Drift
//! between the host's capture rate and the TV's playback clock is absorbed the same way the SDL
//! device's ring absorbs it — pad when dry, drop the oldest when it piles up.
//!
//! Structured like `platform::webos::audio`'s SDL ring, and for the same reason: the two halves
//! belong on different threads, so [`PacedPlane::new`] hands back a sink for the stage and a
//! [`PlaneFeeder`] for the thread that drives it.
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use crate::core::media::{AudioFormat, AudioPlane, AudioSink, Samples};
use crate::session::audio::SAMPLE_RATE;

/// Feeder cadence — the metronome's, because the plane is being held at the same depth the
/// metronome holds it at.
const TICK: Duration = Duration::from_millis(20);

/// Depth the ring accumulates before the first real sample is fed, and again after it runs dry.
/// The jitter budget this route trades latency for; sized like the SDL ring's own prime, which is
/// the same job on the same network.
const PRIME_MS: i64 = 20;

/// Ring ceiling. Above this the host is producing faster than the TV plays (or a stall has just
/// unblocked) and the oldest audio is dropped, so the offset can't creep upward for the rest of
/// the session.
const MAX_QUEUE_MS: i64 = 120;

/// Chunks in flight between the audio thread and the feeder. 5 ms each, so this is ~1.6 s of
/// slack — the queue is bounded by [`MAX_QUEUE_MS`] long before the channel is.
const CHUNK_QUEUE: usize = 320;

/// ~10 s of ticks.
const LOG_EVERY_TICKS: u64 = 500;

/// The feed half, as the audio stage sees it: an [`AudioSink`] that queues instead of playing.
///
/// Every format question is delegated to the plane, so the stage decodes into exactly the sample
/// type and channel order the hardware declared and this adds no conversion of its own.
pub struct PacedPlane {
    plane: Arc<dyn AudioPlane>,
    chunks: SyncSender<Vec<u8>>,
    /// Drained chunk `Vec`s coming back from the feeder for reuse, so steady state stops
    /// allocating (~200 chunks/s otherwise). Behind a `Mutex` only because [`AudioSink`] is
    /// shared: one thread feeds it, and the lock is never contended.
    recycle: Mutex<Receiver<Vec<u8>>>,
    /// Ring depth in ms, kept by both halves — the producer adds, the feeder subtracts. The
    /// feeder's prime and overrun rules read it, and it is half of what the overlay shows.
    queued_ms: Arc<AtomicI64>,
    bytes_per_ms: usize,
}

/// The drain half, owned by the feeder thread: pop, top the plane up, pad when dry.
pub struct PlaneFeeder {
    plane: Arc<dyn AudioPlane>,
    chunks: Receiver<Vec<u8>>,
    recycle: SyncSender<Vec<u8>>,
    queued_ms: Arc<AtomicI64>,
    bytes_per_ms: usize,
    /// One tick's worth of chunks, concatenated so the whole burst reaches the plane under one
    /// backend lock. Reused across ticks.
    burst: Vec<u8>,
    /// Ms of silence padded, and ms of real audio dropped to the ceiling — the two numbers that
    /// say whether the ring is sized right for this network.
    padded_ms: u64,
    dropped_ms: u64,
}

impl PacedPlane {
    /// `channels` is host-resolved, and must be what the plane's load declared — the stage checks
    /// that too, but the ring's ms↔bytes arithmetic depends on it just as hard.
    pub fn new(plane: Arc<dyn AudioPlane>, channels: u8) -> (Arc<Self>, PlaneFeeder) {
        let (chunks_tx, chunks_rx) = std::sync::mpsc::sync_channel(CHUNK_QUEUE);
        let (recycle_tx, recycle_rx) = std::sync::mpsc::sync_channel(CHUNK_QUEUE);
        let bytes_per_ms = (SAMPLE_RATE as usize / 1000) * usize::from(channels.max(1)) * 2;
        let queued_ms = Arc::new(AtomicI64::new(0));
        (
            Arc::new(Self {
                plane: plane.clone(),
                chunks: chunks_tx,
                recycle: Mutex::new(recycle_rx),
                queued_ms: queued_ms.clone(),
                bytes_per_ms,
            }),
            PlaneFeeder {
                plane,
                chunks: chunks_rx,
                recycle: recycle_tx,
                queued_ms,
                bytes_per_ms,
                burst: Vec::with_capacity(bytes_per_ms * MAX_QUEUE_MS as usize),
                padded_ms: 0,
                dropped_ms: 0,
            },
        )
    }
}

impl AudioSink for PacedPlane {
    fn name(&self) -> &'static str {
        self.plane.name()
    }

    fn format(&self) -> AudioFormat {
        self.plane.format()
    }

    /// Queue one decoded packet. The stamp is dropped on purpose: this route's stamps come off the
    /// plane's own clock at the far end of the ring (see the module docs), so a host PTS here
    /// would only describe an arrival time nothing downstream uses.
    ///
    /// A full channel drops the packet rather than blocking — this runs on the audio thread, and
    /// blocking it back-pressures into the transport.
    fn feed(&self, samples: Samples<'_>, _host_pts_ns: u64) -> Result<()> {
        let Samples::S16(pcm) = samples else {
            anyhow::bail!("a paced plane takes S16 samples only");
        };
        let mut buf = self
            .recycle
            .lock()
            .map_or_else(|_| Vec::new(), |rx| rx.try_recv().unwrap_or_default());
        buf.clear();
        buf.extend_from_slice(pcm);
        let ms = (buf.len() / self.bytes_per_ms.max(1)) as i64;
        if self.chunks.try_send(buf).is_ok() {
            self.queued_ms.fetch_add(ms, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Ring plus plane: what the session is actually holding, which is the figure the late-audio
    /// question is asked about.
    fn depth_ms(&self) -> Option<i64> {
        Some(self.queued_ms.load(Ordering::Relaxed) + self.plane.lead_ms())
    }
}

impl PlaneFeeder {
    /// Hold the plane at its target depth until `stop`. Blocks, so the caller gives it a thread.
    ///
    /// This replaces the metronome on a paced route rather than running beside it: two feeders
    /// topping up the same plane would race for the ceiling, and the silence one would win
    /// whenever the ring was momentarily behind.
    pub fn run(&mut self, stop: &std::sync::atomic::AtomicBool) {
        let target = self.plane.target_lead_ms();
        let mut primed = false;
        let mut ticks: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            ticks += 1;
            primed |= self.queued_ms.load(Ordering::Relaxed) >= PRIME_MS;
            if primed {
                self.drop_overrun();
                // Dry before the plane is full means an underrun: re-prime, so the next dry spell
                // is one gap rather than a tick-by-tick stutter — the SDL ring's rule, for the
                // same reason.
                primed = match self.top_up(target) {
                    Ok(full) => full,
                    Err(e) => {
                        tracing::warn!("paced plane feed stopping: {e:#}");
                        return;
                    }
                };
            }
            // Whatever the real samples left short — including the whole target while priming —
            // is silence, because a plane below its depth is a picture that stutters.
            if self.plane.lead_ms() < target {
                self.padded_ms += (target - self.plane.lead_ms()).max(0) as u64;
                if let Err(e) = self.plane.fill_silence() {
                    tracing::warn!("paced plane silence stopping: {e:#}");
                    return;
                }
            }
            if ticks % LOG_EVERY_TICKS == 0 {
                tracing::debug!(
                    ring_ms = self.queued_ms.load(Ordering::Relaxed),
                    lead_ms = self.plane.lead_ms(),
                    padded_ms = self.padded_ms,
                    dropped_ms = self.dropped_ms,
                    "paced audio plane",
                );
            }
            std::thread::sleep(TICK);
        }
        tracing::info!(
            "paced plane ending: {}ms padded, {}ms dropped",
            self.padded_ms,
            self.dropped_ms,
        );
    }

    /// Feed real samples until the plane reaches `target`. `Ok(true)` when it got there, `Ok(false)`
    /// on an empty ring — the caller's cue to re-prime and let silence cover the rest.
    fn top_up(&mut self, target: i64) -> Result<bool> {
        self.burst.clear();
        let mut span_ms = 0;
        let mut full = true;
        while self.plane.lead_ms() + span_ms < target {
            let Some(chunk_ms) = self.pop(|s, chunk| s.burst.extend_from_slice(chunk)) else {
                full = false;
                break;
            };
            span_ms += chunk_ms;
        }
        if span_ms > 0 {
            // One call, one lock: the plane packetizes the burst itself (`NdlVideo::burst_pcm`).
            self.plane.feed_paced(Samples::S16(&self.burst), span_ms)?;
        }
        Ok(full)
    }

    /// Discard the oldest audio while the ring is over [`MAX_QUEUE_MS`]. A ring that only ever
    /// grows is a lip-sync offset that only ever grows with it.
    fn drop_overrun(&mut self) {
        while self.queued_ms.load(Ordering::Relaxed) > MAX_QUEUE_MS {
            match self.pop(|_, _| {}) {
                Some(ms) => self.dropped_ms += ms as u64,
                None => break,
            }
        }
    }

    /// Take the next chunk, hand it to `use_it`, return it to the pool and report its duration.
    /// `None` on an empty ring.
    fn pop(&mut self, use_it: impl FnOnce(&mut Self, &[u8])) -> Option<i64> {
        let mut chunk = self.chunks.try_recv().ok()?;
        let ms = (chunk.len() / self.bytes_per_ms.max(1)) as i64;
        use_it(self, &chunk);
        self.queued_ms.fetch_sub(ms, Ordering::Relaxed);
        chunk.clear();
        let _ = self.recycle.try_send(chunk);
        Some(ms)
    }
}
