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
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::quic;

/// A feed refused because the pipeline hasn't finished loading — distinct from a decode error
/// because the response differs: a decode error is answered with a flush, and flushing a decoder
/// that has not finished loading takes the session's audio out for good (see `session::sink`).
#[derive(Debug)]
pub struct NotReady;

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decode pipeline not loaded yet — holding")
    }
}

impl std::error::Error for NotReady {}

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
    fn audio_plane(&self) -> Option<Arc<dyn AudioPlane>> {
        None
    }
}

/// A hardware audio plane belonging to a video load — NDL's, in practice.
///
/// It is part of the *video* sink's world because the picture depends on it: NDL paces the
/// picture against a fed plane, so a plane starved of packets is a video stutter, not an audio
/// fault (docs/NOTES.md § "NDL's audio plane").
pub trait AudioPlane: Send + Sync {
    /// Publish the host-PTS → sink-clock mapping the video plane is feeding on, so audio stamps
    /// land in the same timeline as the picture. `base_ns` is the frame's mapped stamp.
    fn latch_pts_offset(&self, offset_ns: i64, base_ns: u64);

    /// Drop a mapping that no longer holds; audio holds until the next latch.
    fn clear_pts_offset(&self);

    /// How far the plane's stamps run ahead of its clock, in ms — the depth NDL paces on.
    fn lead_ms(&self) -> i64;

    /// Feed one buffer in the plane's own format, stamped in the host's capture clock.
    fn feed(&self, buf: &[u8], host_pts_ns: u64) -> Result<()>;

    /// Keep the plane fed until `stop`, so the picture stays paced. Blocks; the caller gives it a
    /// thread. `yields_to_real` leaves the plane to whatever pump is feeding it and fills in only
    /// once that stops.
    fn run_keepalive(&self, stop: &AtomicBool, yields_to_real: bool);
}
