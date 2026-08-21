//! Audio decode for the session, plus the SDL device that exists only as a fallback.
//!
//! **The normal path does not come through SDL at all.** [`PcmFeed`] decodes punktfunk's Opus and
//! hands interleaved S16LE straight to NDL's audio plane (`session::pump::ndl_pcm_audio_pump`),
//! which puts the sound on the same hardware clock as the picture and removes the whole local
//! buffering stack from the latency budget. What used to live here — a `JitterPolicy` ring with an
//! adaptive target, crossfaded drift sheds, an `AvSync` estimator and a chunk recycler — was ~35 ms
//! of floor (25 ms ring + a 512-frame device quantum) on top of whatever `PulseAudio` adds behind
//! SDL, and it is gone with it. NDL owns the depth now; nothing here can steer it, which is the
//! trade.
//!
//! [`AudioPlayer`] is what remains: a plain SDL device with a dumb prime-then-serve ring, used
//! ONLY when the session has no NDL audio plane to ride — an audio-enabled load the set refused,
//! NDL v1, or SMP (`session::connect::AudioRoute::Software`). It exists so such a session has
//! sound at all, not to be good; if a report says audio is late, the first question is which route
//! the log named.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::audio::{layout_for, AudioGapTracker};

/// 48 kHz, 5 ms frames — punktfunk's fixed audio framing (see punktfunk-core's
/// audio.rs doc comments and its `multistream_layout_roundtrips_with_channel_identity`
/// test, the canonical reference for both ends of this wire format).
const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 240;
/// Duration of one packet, in ms — the framing above, as the concealment arithmetic needs it.
const FRAME_MS: i64 = 5;
/// Max channels punktfunk ever negotiates (7.1) — sizes the scratch decode buffer.
const MAX_CHANNELS: usize = 8;
/// Widest layout NDL's audio plane has a mode for (`"6-channel"`). A 7.1 stream is folded to this
/// in [`PcmFeed::fold_into`] rather than refused.
const PLANE_MAX_CHANNELS: usize = 6;

/// Device buffer for the fallback device: 512 frames (10.67 ms). Deliberately not smaller — a
/// smaller quantum on a 2-3 core TV `SoC` buys more wakeups and more missed callbacks, not less
/// latency (docs/NOTES.md).
const DEVICE_BUFFER_FRAMES: u16 = 512;

/// Chunks in flight between the decode thread and the fallback callback. 5 ms each.
const CHUNK_QUEUE: usize = 64;

/// Depth the fallback ring primes to before the first sample plays, and the floor it tries to hold.
/// A fixed number, not an adaptive target: the adaptive one belonged to a path that was the primary
/// route, and this one only has to not crackle.
const FALLBACK_PRIME_MS: usize = 25;

/// Interleave order NDL's `"6-channel"` PCM mode expects, as indices into punktfunk's own 5.1
/// order (`FL FR FC LFE BL BR`) — i.e. emit `FL FR BL BR FC LFE`.
///
/// ⚠ **Inferred, not verified.** ss4s only accepts an Opus 5.1 stream whose multistream mapping is
/// `{0,1,4,5,2,3}` for NDL passthrough (`IsOpusPassthroughSupported`), which says NDL wants the
/// centre and LFE pair *last*. Since we interleave the samples ourselves, the same permutation is
/// applied here. If 5.1 comes out with dialogue in the surrounds, [`NDL_51_ORDER`] is the first
/// thing to try as an identity `[0,1,2,3,4,5]` — the failure is audible, not silent.
const NDL_51_ORDER: [usize; PLANE_MAX_CHANNELS] = [0, 1, 4, 5, 2, 3];

/// Decodes punktfunk's Opus into the interleaved S16LE that NDL's audio plane takes.
///
/// Same decoder and the same libopus PLC concealment the SDL path used — none of that moved to the
/// TV; only the sink did. What it adds is the width fold: the plane's widest mode is 6 channels, so
/// a 7.1 session is folded here (sides into rears) rather than being refused, and a TV whose Sound
/// Out is stereo never gets offered more than stereo in the first place
/// (`ndl::audio_plane_max_channels`).
pub struct PcmFeed {
    decoder: opus::MSDecoder,
    /// Negotiated channel count — the decode width, and what libopus sizes a frame by.
    channels: usize,
    /// Channels handed to NDL: [`Self::channels`], folded to at most [`PLANE_MAX_CHANNELS`].
    plane_channels: usize,
    /// Detects packets lost on the wire so they can be concealed rather than skipped.
    gaps: AudioGapTracker,
    /// Reused across packets: concealment frames first, then the packet itself.
    out: Vec<u8>,
}

impl PcmFeed {
    pub fn new(channels: u8) -> Result<Self> {
        let layout = layout_for(channels, false);
        let decoder = opus::MSDecoder::new(SAMPLE_RATE, layout.streams, layout.coupled, layout.mapping)
            .map_err(|e| anyhow::anyhow!("opus MSDecoder::new: {e}"))?;
        let channels = layout.channels as usize;
        let plane_channels = channels.min(PLANE_MAX_CHANNELS);
        if plane_channels != channels {
            tracing::info!("audio: folding {channels} channels into NDL's {plane_channels}-channel plane");
        }
        Ok(Self {
            decoder,
            channels,
            plane_channels,
            gaps: AudioGapTracker::new(),
            // One packet plus the concealment burst that can precede it, so steady state never
            // reallocates.
            out: Vec::with_capacity(SAMPLES_PER_FRAME * MAX_CHANNELS * 2 * 4),
        })
    }

    /// Channels the plane was loaded for — what `session::connect` must have asked NDL for.
    pub fn plane_channels(&self) -> u8 {
        self.plane_channels as u8
    }

    /// Decodes one packet, prefixed by a concealment frame for each packet lost immediately
    /// before it, into interleaved S16LE.
    ///
    /// Returns the bytes and how many **milliseconds before the packet's own PTS the buffer
    /// starts** — the concealment sits in the hole that precedes the packet, so the caller stamps
    /// at `pts - lead`. Zero on the (overwhelmingly common) contiguous path.
    pub fn decode(&mut self, seq: u32, opus_payload: &[u8]) -> Result<(&[u8], i64)> {
        self.out.clear();
        let missing = self.gaps.missing_before(seq);
        for _ in 0..missing {
            let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
            // One frame's worth, not the whole scratch buffer — libopus derives the frame size
            // from `out.len() / channels` when there is no packet to describe it, and rejects a
            // length that isn't legal. 5.1 gives 1920/6 = 320.
            let out = &mut pcm[..SAMPLES_PER_FRAME * self.channels];
            let frames = self
                .decoder
                .decode_float(&[], out, false)
                .map_err(|e| anyhow::anyhow!("opus PLC decode: {e}"))?;
            let decoded = frames * self.channels;
            Self::fold_into(&mut self.out, &pcm[..decoded], self.channels, self.plane_channels);
        }
        let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
        let frames = self
            .decoder
            .decode_float(opus_payload, &mut pcm, false)
            .map_err(|e| anyhow::anyhow!("opus decode_float: {e}"))?;
        let decoded = frames * self.channels;
        Self::fold_into(&mut self.out, &pcm[..decoded], self.channels, self.plane_channels);
        Ok((&self.out, i64::from(missing) * FRAME_MS))
    }

    /// Interleaved f32 → interleaved S16LE at the plane's width, permuted for NDL where 5.1 is
    /// involved (see [`NDL_51_ORDER`]).
    ///
    /// The only fold that exists is 7.1 → 5.1: each side channel is summed into the rear on its
    /// own side at −3 dB, which is the conventional collapse and cannot clip a legal input. Every
    /// other width passes through, because a TV that can't take more than stereo is never offered
    /// more than stereo.
    fn fold_into(out: &mut Vec<u8>, pcm: &[f32], channels: usize, plane_channels: usize) {
        const SIDE_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
        for frame in pcm.chunks_exact(channels) {
            if plane_channels < PLANE_MAX_CHANNELS {
                // Stereo (or mono): nothing to permute and nothing to fold.
                for &s in frame {
                    push_s16le(out, s);
                }
                continue;
            }
            // 5.1 in punktfunk's order, with 7.1's sides folded in where they exist.
            let mut fold = [0f32; PLANE_MAX_CHANNELS];
            fold.copy_from_slice(&frame[..PLANE_MAX_CHANNELS]);
            if channels == 8 {
                fold[4] += frame[6] * SIDE_GAIN;
                fold[5] += frame[7] * SIDE_GAIN;
            }
            for &i in &NDL_51_ORDER {
                push_s16le(out, fold[i]);
            }
        }
    }
}

/// One f32 sample as S16LE, clamped. NDL's PCM plane takes no float format, and a sample outside
/// [-1, 1] (libopus can produce one, and the 7.1 fold above can approach it) would wrap rather
/// than clip if cast directly.
fn push_s16le(out: &mut Vec<u8>, sample: f32) {
    let s = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
    out.extend_from_slice(&s.to_le_bytes());
}

/// The fallback playback device. Holding it alive is the whole job — the ring drains on SDL's own
/// audio thread from construction until this is dropped. See the module docs for when it is used
/// at all.
pub struct AudioPlayer {
    device: sdl2::audio::AudioDevice<RingCallback>,
}

impl AudioPlayer {
    /// Opens the device and returns it alongside the [`AudioFeed`] that fills it. `channels` is
    /// host-resolved (the client MUST build its decoder from this, never from its own request).
    /// `buffer_ms` is where the ring publishes its depth for the stats overlay.
    ///
    /// The two halves are returned separately because they belong on different threads: the device
    /// stays wherever SDL was initialised, and the feed moves to the decode thread.
    pub fn new(sdl_audio: &sdl2::AudioSubsystem, channels: u8, buffer_ms: Arc<AtomicU32>) -> Result<(Self, AudioFeed)> {
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
        let device = sdl_audio
            .open_playback(None, &spec, |obtained| RingCallback {
                rx: pcm_rx,
                recycle: recycle_tx,
                ring: VecDeque::new(),
                // Denominated in interleaved samples, so it is built from what the device actually
                // negotiated rather than what was asked for.
                prime_samples: FALLBACK_PRIME_MS * (SAMPLE_RATE as usize / 1000) * obtained.channels as usize,
                per_ms: (SAMPLE_RATE as usize / 1000) * obtained.channels as usize,
                primed: false,
                buffer_ms,
                underruns: 0,
                callbacks: 0,
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
                gaps: AudioGapTracker::new(),
            },
        ))
    }

    /// The device's actually-negotiated spec — may differ from what was requested if
    /// the device doesn't support it exactly.
    pub fn spec(&self) -> &sdl2::audio::AudioSpec {
        self.device.spec()
    }
}

/// The fallback decode half: Opus → f32 PCM, loss concealment, hand-off to the ring. Lives on its
/// own thread (`session::pump`'s audio feed).
pub struct AudioFeed {
    pcm_tx: SyncSender<Vec<f32>>,
    /// Drained chunk `Vec`s coming back from the callback for reuse, so steady-state playback
    /// stops allocating (~200 chunks/s otherwise).
    recycle_rx: Receiver<Vec<f32>>,
    decoder: opus::MSDecoder,
    channels: usize,
    /// Detects packets lost on the wire so they can be concealed rather than skipped.
    gaps: AudioGapTracker,
}

impl AudioFeed {
    /// Decodes one Opus packet (concealing anything lost immediately before it) and hands the PCM
    /// to the ring. Returns the peak sample — a diagnostic that separates "the host is sending
    /// silence" from "the speaker is not working".
    pub fn play(&mut self, seq: u32, opus_payload: &[u8]) -> Result<f32> {
        // Conceal whatever went missing immediately before this packet. libopus PLC (decode with
        // empty input) interpolates a frame; the alternative is a hard gap, i.e. a click.
        for _ in 0..self.gaps.missing_before(seq) {
            let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
            // One frame, not the whole scratch buffer: with no packet to describe it, libopus
            // takes `out.len() / channels` as the frame size and rejects one that isn't legal.
            let out = &mut pcm[..SAMPLES_PER_FRAME * self.channels];
            let n = self
                .decoder
                .decode_float(&[], out, false)
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
    /// blocking it would back pressure into the transport.
    fn push(&mut self, pcm: &[f32]) {
        let mut buf = self.recycle_rx.try_recv().unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(pcm);
        let _ = self.pcm_tx.try_send(buf);
    }
}

/// The fallback playback half, owned by SDL's audio thread: drain the channel into a ring, prime
/// once, then serve. No adaptive target and no drift shed — see the module docs for why the
/// machinery that had those is gone.
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
                "audio playback (SDL fallback)"
            );
        }
    }
}
