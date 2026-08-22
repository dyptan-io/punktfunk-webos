//! The audio stage: transport packets → whatever the session's [`AudioSink`] takes.
//!
//! One implementation covers all three routes. What differs between them is the sink's declared
//! [`AudioFormat`], and the stage produces exactly that: raw Opus goes through untouched where the
//! TV decodes it, and everywhere else libopus decodes here — with the same concealment, in the
//! same channel order the sink asked for, written once into a reused buffer.
//!
//! **Nothing is mixed down.** A layout the selected route cannot carry is not requested from the
//! host in the first place (`core::model::AudioRoutePref::max_channels`), so a width mismatch here
//! is a bug and is reported as one rather than folded away.
use std::sync::Arc;

use anyhow::{bail, Result};
use punktfunk_core::audio::{layout_for, AudioGapTracker};

use crate::core::media::{AudioFormat, AudioSink, Samples};

/// 48 kHz, 5 ms frames — punktfunk's fixed audio framing (see punktfunk-core's audio.rs docs and
/// its `multistream_layout_roundtrips_with_channel_identity` test, the canonical reference for
/// both ends of this wire format).
pub const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 240;
/// Duration of one packet, in ms — the framing above, as the concealment arithmetic needs it.
const FRAME_MS: i64 = 5;
/// Max channels punktfunk ever negotiates (7.1) — sizes the scratch decode buffer.
const MAX_CHANNELS: usize = 8;

/// Decodes (or forwards) one session's audio into its sink.
pub struct AudioStage {
    sink: Arc<dyn AudioSink>,
    /// The sink's declared format, fixed at load time — read once here rather than per packet.
    format: AudioFormat,
    /// `None` on the Opus route, where the TV decodes and this stage only forwards.
    decoder: Option<opus::MSDecoder>,
    /// Negotiated channel count — the decode width, and what libopus sizes a frame by.
    channels: usize,
    /// Detects packets lost on the wire so they can be concealed rather than skipped.
    gaps: AudioGapTracker,
    /// Reused across packets: concealment frames first, then the packet itself. One of the two is
    /// filled per session, whichever shape the sink declared.
    s16: Vec<i16>,
    f32: Vec<f32>,
}

impl AudioStage {
    /// `channels` is host-resolved — the decoder MUST be built from what the handshake settled on,
    /// never from what was requested.
    pub fn new(sink: Arc<dyn AudioSink>, channels: u8) -> Result<Self> {
        let format = sink.format();
        if format.channels() != channels {
            // Not a fold: see the module docs. A route is only ever selected for a layout it
            // carries, so this is the caps wiring being wrong, and it must be loud.
            bail!(
                "{} takes {} channel(s), the session negotiated {channels}",
                sink.name(),
                format.channels(),
            );
        }
        let layout = layout_for(channels, false);
        let decoder = match format {
            AudioFormat::Opus { .. } => None,
            _ => Some(
                opus::MSDecoder::new(SAMPLE_RATE, layout.streams, layout.coupled, layout.mapping)
                    .map_err(|e| anyhow::anyhow!("opus MSDecoder::new: {e}"))?,
            ),
        };
        Ok(Self {
            sink,
            format,
            decoder,
            channels: layout.channels as usize,
            gaps: AudioGapTracker::new(),
            // One packet plus the concealment burst that can precede it, so steady state never
            // reallocates. Only the sink's own sample type is worth reserving.
            s16: Vec::with_capacity(match format {
                AudioFormat::PcmS16 { .. } => SAMPLES_PER_FRAME * MAX_CHANNELS * 2,
                _ => 0,
            }),
            f32: Vec::with_capacity(match format {
                AudioFormat::PcmF32 { .. } => SAMPLES_PER_FRAME * MAX_CHANNELS * 2,
                _ => 0,
            }),
        })
    }

    pub fn sink_name(&self) -> &'static str {
        self.sink.name()
    }

    /// One packet, concealment included, into the sink.
    ///
    /// Concealment sits in the hole BEFORE the packet, so the buffer starts that many
    /// milliseconds earlier than the packet's own stamp and is fed at `pts - lead`.
    pub fn play(&mut self, seq: u32, pts_ns: u64, payload: &[u8]) -> Result<()> {
        let Some(decoder) = self.decoder.as_mut() else {
            // The TV decodes: concealment, layout and framing are all its business from here.
            return self.sink.feed(Samples::Opus(payload), pts_ns);
        };
        let missing = self.gaps.missing_before(seq);
        self.s16.clear();
        self.f32.clear();
        // A concealment frame gets one frame's worth of buffer, not the whole scratch: with no
        // packet to describe it libopus takes `out.len() / channels` as the frame size and
        // rejects an illegal one. 5.1 gives 1920/6 = 320.
        let cap = |i: u32| {
            if i < missing {
                SAMPLES_PER_FRAME * self.channels
            } else {
                SAMPLES_PER_FRAME * MAX_CHANNELS
            }
        };
        // Concealment frames first (libopus PLC — decode with empty input interpolates a frame;
        // the alternative is a hard gap, i.e. a click), then the packet itself. The scratch lives
        // outside the loop: libopus overwrites what it uses, so re-zeroing it per frame is waste.
        match self.format {
            // Decoded straight to the sink's own sample type: libopus writes S16 as happily as
            // f32, so a PCM plane costs no conversion pass and no second buffer.
            AudioFormat::PcmS16 { interleave, .. } => {
                let mut pcm = [0i16; SAMPLES_PER_FRAME * MAX_CHANNELS];
                for i in 0..=missing {
                    let input: &[u8] = if i < missing { &[] } else { payload };
                    let frames = decoder
                        .decode(input, &mut pcm[..cap(i)], false)
                        .map_err(|e| anyhow::anyhow!("opus decode: {e}"))?;
                    let decoded = &pcm[..frames * self.channels];
                    match interleave {
                        // Written in the sink's channel order as it goes, so an order that differs
                        // from punktfunk's costs nothing extra.
                        Some(order) => {
                            for frame in decoded.chunks_exact(self.channels) {
                                self.s16.extend(order.iter().map(|&c| frame[c]));
                            }
                        }
                        None => self.s16.extend_from_slice(decoded),
                    }
                }
            }
            AudioFormat::PcmF32 { .. } => {
                let mut pcm = [0f32; SAMPLES_PER_FRAME * MAX_CHANNELS];
                for i in 0..=missing {
                    let input: &[u8] = if i < missing { &[] } else { payload };
                    let frames = decoder
                        .decode_float(input, &mut pcm[..cap(i)], false)
                        .map_err(|e| anyhow::anyhow!("opus decode: {e}"))?;
                    self.f32.extend_from_slice(&pcm[..frames * self.channels]);
                }
            }
            AudioFormat::Opus { .. } => unreachable!("decoder is None on the Opus route"),
        }
        let pts_ns = pts_ns.saturating_sub((i64::from(missing) * FRAME_MS) as u64 * 1_000_000);
        match self.format {
            AudioFormat::PcmF32 { .. } => self.sink.feed(Samples::F32(&self.f32), pts_ns),
            AudioFormat::PcmS16 { .. } => self.sink.feed(Samples::S16(as_le_bytes(&self.s16)), pts_ns),
            AudioFormat::Opus { .. } => unreachable!("decoder is None on the Opus route"),
        }
    }

    /// The sink's own queue depth in ms, where it knows one — NDL's plane lead, or the SDL ring's
    /// fill. The one figure that says which side of a late-audio report to look at.
    pub fn depth_ms(&self) -> Option<i64> {
        self.sink.depth_ms()
    }

    /// Peak sample of the last decoded buffer — a diagnostic that separates "the host is sending
    /// silence" from "the speaker is not working". `None` on the Opus route, which decodes nothing.
    pub fn peak(&self) -> Option<f32> {
        match self.format {
            AudioFormat::PcmF32 { .. } => Some(self.f32.iter().fold(0f32, |m, &s| m.max(s.abs()))),
            _ => None,
        }
    }
}

/// The S16 buffer as the bytes a PCM sink takes. Free on every target this ships to — S16LE is
/// exactly the in-memory representation of `i16` on a little-endian machine — and the compile-time
/// assertion is what keeps it that way.
fn as_le_bytes(samples: &[i16]) -> &[u8] {
    const {
        assert!(
            cfg!(target_endian = "little"),
            "S16LE output assumes a little-endian target"
        );
    }
    // SAFETY: `i16` has no padding and no invalid bit patterns, so any `[i16]` is a valid `[u8]`
    // of twice the length; the borrow keeps the source alive for the result.
    unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), std::mem::size_of_val(samples)) }
}
