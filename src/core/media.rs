//! The media pipeline's vocabulary: what a decode backend must offer, and what a stage above it
//! is allowed to assume.
//!
//! Three backends decode video here (NDL v2, NDL v1, SMP) and three routes carry audio, and the
//! pipeline in `session` is written against these traits rather than against any of them. In
//! `core` for the layering reason every shared vocabulary is: `platform::webos` implements it and
//! `session` consumes it, so it can live in neither.
//!
//! **The traits describe capability, not policy.** A sink says what it can take
//! ([`VideoSinkCaps`]) and does it; anchoring, freeze-until-reanchor, backlog metering and
//! concealment are the stages' business, above this seam and identical on every backend.
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::quic;

/// A feed refused because the pipeline hasn't finished loading — distinct from a decode error
/// because the response differs: a decode error is answered with a flush, and flushing a decoder
/// that has not finished loading takes the session's audio out for good (see `session::stage`).
#[derive(Debug)]
pub struct NotReady;

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decode pipeline not loaded yet — holding")
    }
}

impl std::error::Error for NotReady {}

/// [`SessionClock::offset_ns`] before the video plane has published a mapping. Not 0 — a genuine
/// offset of 0 ns is possible, and "unset" must be distinguishable from it.
const NO_OFFSET: i64 = i64::MIN;

/// The one mapping between the host's capture clock and the sink's own, shared by every plane in
/// a session.
///
/// **Both planes must be stamped in one time base.** The decoder runs its own A/V synchronisation
/// against the stamps it is fed, and regulating a video plane on host-capture cadence against an
/// audio plane on arrival wall-clock is what froze the picture on webOS 10.3 (docs/NOTES.md §
/// "NDL's audio plane"). Before this existed the mapping lived in the video sink's own atomics and
/// was pushed into the audio plane frame by frame; one object both planes read is the same
/// guarantee with one owner.
///
/// **Latched, not republished.** The mapping is stable only while the video stage's anchor is, so
/// the first value after each [`Self::clear`] wins and later ones are ignored. Re-deriving it per
/// frame lets any jump in the video timeline — a receive-backlog flush jumping to live drops
/// frames, so host PTS leaps forward while the sink clock does not — drag the audio stamp
/// *backwards* by the size of the jump, which NDL takes as a rewind and answers by muting the rest
/// of the session.
#[derive(Debug)]
pub struct SessionClock {
    offset_ns: AtomicI64,
    /// Bumped on every latch, so a reader can tell "the same mapping" from "a fresh one" without
    /// being called back on the video thread. A plane with per-latch work of its own (NDL derives
    /// its stamp skew) does it on its own thread, the first time it sees a new epoch.
    epoch: AtomicU64,
}

impl Default for SessionClock {
    fn default() -> Self {
        Self {
            offset_ns: AtomicI64::new(NO_OFFSET),
            epoch: AtomicU64::new(0),
        }
    }
}

impl SessionClock {
    /// Publish `offset_ns` if nothing is latched. Called per fed frame; only the first after each
    /// [`Self::clear`] takes.
    pub fn latch(&self, offset_ns: i64) {
        // The steady state is "already latched", and this runs per fed frame — keep the exclusive
        // access off the video thread's hot path once the offset is set.
        if self.offset_ns.load(Ordering::Relaxed) != NO_OFFSET {
            return;
        }
        if self
            .offset_ns
            .compare_exchange(NO_OFFSET, offset_ns, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.epoch.fetch_add(1, Ordering::Release);
        }
    }

    /// Decouple: the two timelines no longer agree (the video stage reset its anchor after a
    /// freeze-until-reanchor hold). Readers hold until the next latch.
    pub fn clear(&self) {
        self.offset_ns.store(NO_OFFSET, Ordering::Relaxed);
    }

    /// Which mapping is current — see [`Self::epoch`]. Pair it with [`Self::map_host_ns`]: a
    /// reader that sees a new epoch is on a fresh timeline.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// `host_pts_ns` in the sink's clock domain, or `None` while nothing is latched — audio ahead
    /// of the first video frame has no timeline to join yet.
    pub fn map_host_ns(&self, host_pts_ns: u64) -> Option<u64> {
        match self.offset_ns.load(Ordering::Relaxed) {
            NO_OFFSET => None,
            offset => Some((host_pts_ns as i64).saturating_add(offset).max(0) as u64),
        }
    }
}

/// What a video backend can be asked to do. Every `false` here is a stage behaviour that must
/// switch off, not a call that may fail — the alternative is per-backend `match`es scattered
/// across the pipeline, which is what this replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSinkCaps {
    /// The feed takes a presentation timestamp. `false` on NDL v1, which presents in feed order.
    pub pts: bool,
    /// Access units may be fed in pieces as they arrive (slice-progressive delivery). Needs a
    /// timestamp that can be repeated across the pieces, so it implies [`Self::pts`].
    pub partial_au: bool,
    /// The decoder holds a render queue that can be dropped on the floor.
    pub flush: bool,
    /// The queue's depth can be read back ([`VideoSink::queue_depth`]).
    pub render_queue: bool,
}

impl VideoSinkCaps {
    /// Nothing beyond a feed: the narrowest backend shape (NDL v1).
    pub const FEED_ONLY: Self = Self {
        pts: false,
        partial_au: false,
        flush: false,
        render_queue: false,
    };
}

/// A monotonic clock a sink presents against. NDL and SMP each have their own, unrelated to the
/// host's capture clock and to wall-clock — mapping between them is `session::timeline`'s job, and
/// this is the only thing it needs from a backend.
pub trait MediaClock: Send + Sync {
    /// Nanoseconds since the decoder loaded.
    fn now_ns(&self) -> u64;
}

/// One loaded video decoder.
pub trait VideoSink: Send {
    /// For the log line and the stats overlay.
    fn name(&self) -> &'static str;

    fn caps(&self) -> VideoSinkCaps;

    /// Hand one access unit (or one piece of one) to the decoder. `pts_ns` is in this sink's own
    /// clock domain and ignored by a sink whose caps say it takes no timestamp.
    fn feed(&self, au: &[u8], pts_ns: u64) -> Result<()>;

    /// Drop whatever is queued. A no-op where [`VideoSinkCaps::flush`] is false.
    fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// Frames queued for presentation, or `None` where the backend can't say — which is not the
    /// same answer as an empty queue, and the stages treat it differently.
    fn queue_depth(&self) -> Option<u32> {
        None
    }

    /// Negotiated colorimetry, plus HDR mastering metadata where the session applies it.
    fn set_color(&self, _meta: Option<&quic::HdrMeta>, _color: quic::ColorInfo) -> Result<()> {
        Ok(())
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        None
    }

    /// The audio plane this load produced, where the backend has one and the load was accepted.
    /// It is an [`AudioSink`] too, so a route that rides it needs nothing else.
    fn audio_plane(&self) -> Option<Arc<dyn AudioPlane>> {
        None
    }
}

/// What an audio sink takes. Declared by the sink; the stage above produces exactly this and
/// nothing converts afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    /// The wire's own Opus, handed over undecoded — the sink decodes.
    Opus { channels: u8 },
    /// Interleaved S16LE. `interleave` permutes punktfunk's channel order into the sink's, or is
    /// `None` where the two agree; it is applied while the samples are written, so an order that
    /// differs costs nothing extra.
    PcmS16 {
        channels: u8,
        sample_rate: u32,
        interleave: Option<&'static [usize]>,
    },
    /// Interleaved f32 in punktfunk's own channel order — what libopus decodes to, so this is the
    /// format that costs no conversion at all.
    PcmF32 { channels: u8, sample_rate: u32 },
}

impl AudioFormat {
    /// Channels this sink puts on a speaker. Nothing is ever folded into it: a session asks the
    /// host for a layout the selected route carries (`model::AudioRoutePref::max_channels`), so a
    /// mismatch here is a bug, not a case to mix down.
    pub fn channels(self) -> u8 {
        match self {
            Self::Opus { channels } | Self::PcmS16 { channels, .. } | Self::PcmF32 { channels, .. } => channels,
        }
    }
}

/// One packet on its way to a sink, in whatever shape that sink declared.
pub enum Samples<'a> {
    Opus(&'a [u8]),
    S16(&'a [u8]),
    F32(&'a [f32]),
}

/// Somewhere a session's audio can go. Three exist here — the TV's SDL device, NDL's PCM plane
/// and NDL's Opus plane — and `session::audio`'s stage is written against this rather than against
/// any of them, so adding a fourth is one implementation and no pipeline change.
pub trait AudioSink: Send + Sync {
    /// For the log line and the stats overlay.
    fn name(&self) -> &'static str;

    /// What this sink takes. The stage produces it; nothing converts on the way in.
    fn format(&self) -> AudioFormat;

    /// Feed one packet, stamped in the host's capture clock. A sink that paces on a timeline maps
    /// it through the session's [`SessionClock`]; one that just plays what it is given ignores it.
    fn feed(&self, samples: Samples<'_>, host_pts_ns: u64) -> Result<()>;

    /// Queue depth in ms where the sink knows one, for the stats overlay.
    fn depth_ms(&self) -> Option<i64> {
        None
    }
}

/// A hardware audio plane belonging to a video load — NDL's, in practice.
///
/// It is part of the *video* sink's world because the picture depends on it: NDL paces the
/// picture against a fed plane, so a plane starved of packets is a video stutter, not an audio
/// fault (docs/NOTES.md § "NDL's audio plane").
pub trait AudioPlane: AudioSink {
    /// Hand the plane the session's shared timeline, once, before anything is fed. Every stamp it
    /// hands the hardware is mapped through this — see [`SessionClock`].
    fn attach_clock(&self, clock: Arc<SessionClock>);

    /// How far the plane's stamps run ahead of its clock, in ms — the depth NDL paces on.
    fn lead_ms(&self) -> i64;

    /// Keep the plane fed until `stop`, so the picture stays paced. Blocks; the caller gives it a
    /// thread. `yields_to_real` leaves the plane to whatever pump is feeding it and fills in only
    /// once that stops.
    fn run_keepalive(&self, stop: &AtomicBool, yields_to_real: bool);
}
