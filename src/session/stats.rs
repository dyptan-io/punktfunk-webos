//! Live session counters and process metrics the stats overlay reads.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64};

use punktfunk_core::quic;

/// Live video-pump counters for stats overlay (read at ~2Hz); relaxed atomics written per frame.
#[derive(Default)]
pub struct StreamStats {
    pub frames: AtomicU64,
    /// Bytes received; deltas give measured bitrate.
    pub bytes: AtomicU64,
    /// Freeze-until-reanchor hold active.
    pub holding: AtomicBool,
    /// Most recent decoder feed duration (µs).
    pub feed_us: AtomicU32,
    /// NDL render-buffer backlog or -1 if unavailable.
    pub render_backlog: AtomicI32,
}

/// Short display name for a resolved wire codec id (the stats overlay's header).
pub fn codec_name(codec: u8) -> &'static str {
    match codec {
        c if c == quic::CODEC_HEVC => "HEVC",
        c if c == quic::CODEC_H264 => "H264",
        c if c == quic::CODEC_AV1 => "AV1",
        _ => "?",
    }
}
