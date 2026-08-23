//! The audio stage: transport packets → whatever the session's [`AudioSink`] takes.
//!
//! One implementation covers both routes. What differs between them is the sink's declared
//! [`AudioFormat`], and the stage produces exactly that: raw Opus goes through untouched where the
//! TV decodes it, and libopus decodes here where the SDL device plays it — with concealment,
//! written once into a reused buffer.
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
    /// `None` on the Opus route, where the TV decodes and this stage only forwards.
    decoder: Option<opus::MSDecoder>,
    /// Negotiated channel count — the decode width, and what libopus sizes a frame by.
    channels: usize,
    /// Detects packets lost on the wire so they can be concealed rather than skipped.
    gaps: AudioGapTracker,
    /// Reused across packets: concealment frames first, then the packet itself.
    f32: Vec<f32>,
    /// libopus's own output buffer, one frame at the widest layout. A field rather than a local:
    /// as a local it is a 7.7 KB stack array zeroed on every packet, i.e. 200 pointless memsets a
    /// second on a soft-float `SoC`. libopus overwrites what it uses, so the stale contents of the
    /// tail are never read.
    pcm: Box<[f32; SAMPLES_PER_FRAME * MAX_CHANNELS]>,
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
            decoder,
            channels: layout.channels as usize,
            gaps: AudioGapTracker::new(),
            // One packet plus the concealment burst that can precede it, so steady state never
            // reallocates.
            f32: Vec::with_capacity(SAMPLES_PER_FRAME * MAX_CHANNELS * 2),
            pcm: Box::new([0.0; SAMPLES_PER_FRAME * MAX_CHANNELS]),
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
        // Destructured so the decode loop can hold `decoder`, `pcm` and `f32` at once.
        let Self {
            decoder,
            f32,
            pcm,
            channels,
            gaps,
            sink,
        } = self;
        let Some(decoder) = decoder.as_mut() else {
            // The TV decodes: concealment, layout and framing are all its business from here.
            return sink.feed(Samples::Opus(payload), pts_ns);
        };
        let channels = *channels;
        let missing = gaps.missing_before(seq);
        f32.clear();
        // A concealment frame gets one frame's worth of buffer, not the whole scratch: with no
        // packet to describe it libopus takes `out.len() / channels` as the frame size and
        // rejects an illegal one. 5.1 gives 1920/6 = 320.
        let cap = |i: u32| {
            if i < missing {
                SAMPLES_PER_FRAME * channels
            } else {
                SAMPLES_PER_FRAME * MAX_CHANNELS
            }
        };
        // Concealment frames first (libopus PLC — decode with empty input interpolates a frame;
        // the alternative is a hard gap, i.e. a click), then the packet itself. `f32` is both what
        // libopus produces and what the SDL device takes, so there is no conversion pass and no
        // second buffer.
        for i in 0..=missing {
            let input: &[u8] = if i < missing { &[] } else { payload };
            let frames = decoder
                .decode_float(input, &mut pcm[..cap(i)], false)
                .map_err(|e| anyhow::anyhow!("opus decode: {e}"))?;
            f32.extend_from_slice(&pcm[..frames * channels]);
        }
        let pts_ns = pts_ns.saturating_sub((i64::from(missing) * FRAME_MS) as u64 * 1_000_000);
        sink.feed(Samples::F32(f32), pts_ns)
    }

    /// The sink's own queue depth in ms, where it knows one — NDL's plane lead, or the SDL ring's
    /// fill. The one figure that says which side of a late-audio report to look at.
    pub fn depth_ms(&self) -> Option<i64> {
        self.sink.depth_ms()
    }

    /// Peak sample of the last decoded buffer — a diagnostic that separates "the host is sending
    /// silence" from "the speaker is not working". `None` on the Opus route, which decodes nothing.
    pub fn peak(&self) -> Option<f32> {
        self.decoder
            .is_some()
            .then(|| self.f32.iter().fold(0f32, |m, &s| m.max(s.abs())))
    }
}
