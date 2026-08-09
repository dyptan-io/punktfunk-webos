//! SDL2 audio-queue playback of punktfunk's Opus audio packets. Decode only (Opus →
//! PCM) happens here — NDL is video-only (see ndl.rs docs), so this is a completely
//! separate path from the video decode/punch-through plane.
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::audio::{layout_for, AudioGapTracker, AvSync, AvSyncObservation};

/// 48 kHz, 5 ms frames — punktfunk's fixed audio framing (see punktfunk-core's
/// audio.rs doc comments and its `multistream_layout_roundtrips_with_channel_identity`
/// test, the canonical reference for both ends of this wire format).
const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 240;
/// Max channels punktfunk ever negotiates (7.1) — sizes the scratch decode buffer.
const MAX_CHANNELS: usize = 8;

/// Device buffer: 512 frames keeps slack vs pump cadence, cuts latency by ~75ms.
/// Obtained spec logged at session start for on-device verification.
const DEVICE_BUFFER_FRAMES: u16 = 512;

/// Soft ceiling on SDL-queued audio: above this, drop packets to let queue drain.
/// WHY: full clear at `MAX_QUEUED_LAG_MS` was audible (100ms silence). Drops are ~5ms.
pub const SOFT_QUEUED_LAG_MS: u32 = 60;

/// Hard bound: backstop against burst between pump ticks. Clear costs one audible blip.
pub const MAX_QUEUED_LAG_MS: u32 = 100;

/// What [`AudioPlayer::play`] did with a packet — reported so the caller can log the
/// cases that are audible, and tell an over-full queue apart from a starved one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioEvent {
    Queued,
    /// The device buffer had run dry before this packet arrived: the main loop didn't
    /// feed it in time (a stall), and the gap was audible. Distinct from every
    /// queue-too-full case below — the remedy is the opposite direction.
    Underrun,
    /// Dropped above [`SOFT_QUEUED_LAG_MS`] to let the queue drain.
    Dropped,
    /// Cleared at [`MAX_QUEUED_LAG_MS`] — the loud one.
    Resnapped,
}

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

pub struct AudioPlayer {
    queue: sdl2::audio::AudioQueue<f32>,
    decoder: opus::MSDecoder,
    channels: usize,
    /// Interleaved samples per millisecond at the negotiated layout (48 × channels).
    per_ms: usize,
    /// Detects packets lost on the wire so they can be concealed rather than skipped —
    /// see [`AudioPlayer::play`].
    gaps: AudioGapTracker,
    /// A/V synchronisation, **measure-only for now** — see [`AudioPlayer::observe_av`].
    av: AvSync,
    cells: SyncCells,
}

impl AudioPlayer {
    /// `channels` is host-resolved (client MUST build decoder from this, not own request).
    pub fn new(sdl_audio: &sdl2::AudioSubsystem, channels: u8, cells: SyncCells) -> Result<Self> {
        let layout = layout_for(channels, false);
        let decoder = opus::MSDecoder::new(SAMPLE_RATE, layout.streams, layout.coupled, layout.mapping)
            .map_err(|e| anyhow::anyhow!("opus MSDecoder::new: {e}"))?;
        let spec = sdl2::audio::AudioSpecDesired {
            freq: Some(SAMPLE_RATE as i32),
            channels: Some(layout.channels),
            samples: Some(DEVICE_BUFFER_FRAMES),
        };
        let queue = sdl_audio
            .open_queue::<f32, _>(None, &spec)
            .map_err(|e| anyhow::anyhow!("SDL open_queue: {e}"))?;
        queue.resume();
        Ok(Self {
            queue,
            decoder,
            channels: layout.channels as usize,
            per_ms: (SAMPLE_RATE / 1000) as usize * layout.channels as usize,
            gaps: AudioGapTracker::new(),
            av: AvSync::new(layout.channels),
            cells,
        })
    }

    /// Fold one packet into the A/V offset measurement, and publish both the offset and the ring
    /// depth for the stats overlay.
    ///
    /// **This measures; it does not steer.** [`AvSync::desired_depth`] is deliberately never
    /// called, and no target is ever handed to a playback policy. The reason is the one term this
    /// platform cannot observe: NDL reports nothing about presentation, so
    /// `session::sink::video_e2e_ns` carries a *calibrated constant* for the decode+panel latency
    /// after its render queue drains. Until that constant has been read off real hardware it is
    /// `0`, which biases the offset high — and acting on a biased offset would place audio early
    /// by exactly the amount of the bias, a worse fault than the drift being corrected. Publishing
    /// it on the HUD first is what turns that constant from a guess into a measurement.
    ///
    /// `buffered_ahead` is the device queue as it stands BEFORE this packet is added: everything
    /// that must still play before it does.
    fn observe_av(&mut self, pts_ns: u64, buffered_ahead: usize) {
        // Published unconditionally — the ring's depth is worth seeing whether or not the loop is
        // acting, and it is what makes an "audio is late" report triageable at all.
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

    /// The device's actually-negotiated spec — may differ from what was requested if
    /// the device doesn't support it exactly.
    pub fn spec(&self) -> &sdl2::audio::AudioSpec {
        self.queue.spec()
    }

    /// Decodes Opus packet (with PLC for losses) and queues PCM.
    /// Returns peak sample (diagnostic for silent input vs speaker failure) + `AudioEvent`.
    pub fn play(&mut self, seq: u32, pts_ns: u64, opus_payload: &[u8]) -> Result<(f32, AudioEvent)> {
        let bytes_per_ms = SAMPLE_RATE / 1000 * self.channels as u32 * std::mem::size_of::<f32>() as u32;
        let queued = self.queue.size();

        // Measured against the queue as it stands NOW, before this packet joins it, and before the
        // shed branches below — a packet this call decides to drop still tells the truth about
        // where the ring sits relative to the picture.
        self.observe_av(pts_ns, queued as usize / std::mem::size_of::<f32>());

        // WHY: empty queue detects stall on feeding thread (opposite remedy from over-full).
        let underrun = queued == 0;

        if queued > bytes_per_ms * MAX_QUEUED_LAG_MS {
            self.queue.clear();
            self.queue_packet(opus_payload)?;
            return Ok((0.0, AudioEvent::Resnapped));
        }
        if queued > bytes_per_ms * SOFT_QUEUED_LAG_MS {
            // WHY: decode anyway (stateful codec); skip would corrupt state and follow-up.
            let _ = self.gaps.missing_before(seq);
            let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
            let _ = self.decoder.decode_float(opus_payload, &mut pcm, false);
            return Ok((0.0, AudioEvent::Dropped));
        }

        // Conceal whatever went missing immediately before this packet.
        for _ in 0..self.gaps.missing_before(seq) {
            let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
            let n = self
                .decoder
                .decode_float(&[], &mut pcm, false)
                .map_err(|e| anyhow::anyhow!("opus PLC decode: {e}"))?;
            self.queue
                .queue_audio(&pcm[..n * self.channels])
                .map_err(|e| anyhow::anyhow!("SDL queue_audio (PLC): {e}"))?;
        }

        let peak = self.queue_packet(opus_payload)?;
        Ok((
            peak,
            if underrun {
                AudioEvent::Underrun
            } else {
                AudioEvent::Queued
            },
        ))
    }

    /// Decodes one real packet into the device queue, returning its peak sample.
    fn queue_packet(&mut self, opus_payload: &[u8]) -> Result<f32> {
        let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
        let samples_per_channel = self
            .decoder
            .decode_float(opus_payload, &mut pcm, false)
            .map_err(|e| anyhow::anyhow!("opus decode_float: {e}"))?;
        let decoded = &pcm[..samples_per_channel * self.channels];
        let peak = decoded.iter().fold(0f32, |m, &s| m.max(s.abs()));
        self.queue
            .queue_audio(decoded)
            .map_err(|e| anyhow::anyhow!("SDL queue_audio: {e}"))?;
        Ok(peak)
    }
}
