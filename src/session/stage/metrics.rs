//! The two latency figures the video stage derives from one render-queue depth, and the
//! self-calibrating cushion both are read against.
//!
//! Free functions rather than [`VideoStage`](super::VideoStage) methods purely so they are
//! testable: the stage owns a live NDL handle and cannot be built off-device.

use std::time::Duration;

/// Render-buffer frames NDL holds as a *standing* present cushion, excluded from the ABR decode
/// figure (see [`VideoStage::decode_us`]). NDL presents off its own clock, so a healthy session
/// sits at 1-2 frames of depth indefinitely — lead, not backlog. Folded in raw it manufactures a
/// constant 8-17ms of apparent decode latency, crossing `abr`'s 15ms decode-rise threshold and
/// backing off bitrate for no real reason (observed on the CX 2026-08-10: 1440p120 driven from
/// 25 Mbps to the 5 Mbps floor with zero loss and flat OWD). Subtracting it keeps the real
/// signal — a queue that GROWS past this is genuine falling-behind — while dropping the constant.
///
/// Only a floor: the settled depth is NDL's business and one frame is 16.6ms at 60Hz, so a mode
/// idling at three frames would reproduce the bug against a fixed two. The excluded depth is the
/// rolling min over [`CUSHION_MIN_POLLS`], clamped into this..=[`CUSHION_MAX_FRAMES`] — this value
/// covers only the first seconds, before that minimum has seen a settled queue.
pub(super) const STANDING_CUSHION_FRAMES: u64 = 2;
/// Polls whose minimum depth is the self-calibrating cushion (see [`STANDING_CUSHION_FRAMES`]).
/// 20 × [`BACKLOG_POLL`] = 5s: long enough that `abr`'s two-window (1.5s) decode backoff still
/// fires on a queue that genuinely grows, short enough to follow a mode or content change.
pub(super) const CUSHION_MIN_POLLS: usize = 20;
/// Ceiling on the self-calibrated cushion. Without it, a decoder that stays overloaded for longer
/// than [`CUSHION_MIN_POLLS`] teaches the rolling minimum its own backlog and the signal goes
/// quiet — the same baseline-absorption trap this fix exists to escape, just faster. Lead is a
/// couple of frames by construction, so anything past this is queue, and queue must stay visible.
pub(super) const CUSHION_MAX_FRAMES: u64 = 4;

/// This frame's video end-to-end latency, in the shape [`punktfunk_core::audio::AvSync`] compares
/// against: the instant it reaches the glass expressed in the HOST capture clock, minus its host
/// PTS. Published for the audio plane, which steers its ring depth to land audio WITH the picture.
///
/// **Why an estimate at all.** NDL is submit-only — `NDL_DirectVideoPlay` returns nothing about
/// presentation and there is no glass callback — so the true stamp the Vulkan/Android/Apple
/// presenters publish does not exist here. This is the fallback core's own `video_e2e_ns` field
/// docs already sanction ("a TRUE on-glass stamp where `VK_KHR_present_wait` is available, and the
/// submit instant otherwise"), plus the one further term this platform *can* observe: NDL's
/// standing render queue. That middle term is the one that MOVES — a decoder falling behind
/// buffers deeper — and it is precisely the ratchet the sync loop exists to cancel.
///
/// **The estimate omits one term, and that is why nothing steers on it.** NDL's decode + panel
/// latency *after* the queue drains cannot be observed from the app at all, so it is simply absent
/// here — which biases this figure low, biases the measured A/V offset high, and would aim the
/// audio ring correspondingly shallow, i.e. **play audio early by the whole missing term**. At
/// 60 Hz a plausible 2–5 frame pipeline is 33–83 ms, far outside `AvSync`'s 10 ms deadband, so
/// acting on this would be a systematic error larger than the drift being corrected. Hence
/// measure-only: the offset reaches the stats overlay and no target reaches `JitterPolicy` (see
/// `platform::webos::audio`'s `observe_av`). Arming it means measuring that term on real hardware
/// first and folding it back in as a constant — `docs/NOTES.md` § "A/V sync" has the procedure.
///
/// `None` when the arithmetic would place the frame on the glass *before* its own capture — a
/// wall-clock step, a stale PTS, or a host clock that has not settled. The caller then publishes
/// nothing, core reads the untouched cell as "nothing presented yet", and the loop stays inert
/// rather than steering on a figure it cannot believe.
pub(super) fn video_e2e_ns(
    submit_realtime_ns: i128,
    clock_offset_ns: i64,
    frame_pts_ns: u64,
    backlog_frames: u64,
    frame_interval_ns: u64,
) -> Option<u64> {
    let pipeline_ns = backlog_frames.saturating_mul(frame_interval_ns);
    let glass_host_ns = submit_realtime_ns
        .checked_add(i128::from(pipeline_ns))?
        .checked_add(i128::from(clock_offset_ns))?;
    u64::try_from(glass_host_ns.checked_sub(i128::from(frame_pts_ns))?).ok()
}

/// The decode-stage latency reported to `punktfunk_core::abr`: submission time plus the part of
/// NDL's render queue that is genuinely backlog. See [`VideoStage::decode_us`] for why the queue
/// belongs in it at all, and [`STANDING_CUSHION_FRAMES`] for why `cushion` comes out first.
///
/// Split out as a free function purely so it is testable — [`VideoStage`] owns a live NDL handle and
/// cannot be built off-device.
pub(super) fn decode_report_us(feed_elapsed: Duration, backlog: u64, cushion: u64, frame_interval_ns: u64) -> u32 {
    let queued_ns = backlog.saturating_sub(cushion).saturating_mul(frame_interval_ns);
    let decode_us = u64::try_from(feed_elapsed.as_micros())
        .unwrap_or(u64::MAX)
        .saturating_add(queued_ns / 1_000);
    u32::try_from(decode_us).unwrap_or(u32::MAX)
}

/// The depth [`decode_report_us`] treats as present lead rather than backlog: the rolling minimum
/// of recent poll samples, clamped into [`STANDING_CUSHION_FRAMES`]..=[`CUSHION_MAX_FRAMES`].
/// Empty (no poll yet) yields the floor, never 0 — a session's first windows are exactly when the
/// buffer is still filling and a raw fold does its damage.
pub(super) fn cushion_frames(samples: impl Iterator<Item = u64>) -> u64 {
    samples
        .min()
        .unwrap_or(0)
        .clamp(STANDING_CUSHION_FRAMES, CUSHION_MAX_FRAMES)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{cushion_frames, decode_report_us, video_e2e_ns, CUSHION_MAX_FRAMES, STANDING_CUSHION_FRAMES};

    /// 60 Hz, the cadence the frame interval reconciles to on most panels.
    const HZ60_NS: u64 = 16_666_666;
    /// A frame submitted 30 ms after the host captured it, with clocks aligned and nothing queued.
    const SUBMIT: i128 = 2_000_000_000;
    const PTS: u64 = 1_970_000_000;

    fn base() -> Option<u64> {
        video_e2e_ns(SUBMIT, 0, PTS, 0, HZ60_NS)
    }

    #[test]
    fn e2e_is_the_glass_instant_minus_the_capture_instant() {
        assert_eq!(base(), Some(30_000_000));
    }

    /// The term that makes this better than a bare submit stamp: frames already queued in NDL must
    /// drain before this one is seen, so each adds one panel interval. Deleting the backlog term
    /// collapses this onto the base case.
    #[test]
    fn the_render_queue_adds_its_own_drain_time() {
        assert_eq!(video_e2e_ns(SUBMIT, 0, PTS, 3, HZ60_NS), Some(30_000_000 + 3 * HZ60_NS));
        // And a deeper queue is strictly later — the ratchet the sync loop exists to cancel.
        let shallow = video_e2e_ns(SUBMIT, 0, PTS, 1, HZ60_NS).unwrap();
        let deep = video_e2e_ns(SUBMIT, 0, PTS, 6, HZ60_NS).unwrap();
        assert!(deep > shallow, "{deep} !> {shallow}");
    }

    /// The glass instant is expressed in the HOST clock, so the skew term shifts it directly. A
    /// host running ahead of the client makes the same frame land later in host terms.
    #[test]
    fn clock_skew_moves_the_glass_instant() {
        assert_eq!(video_e2e_ns(SUBMIT, 7_000_000, PTS, 0, HZ60_NS), Some(37_000_000));
        assert_eq!(video_e2e_ns(SUBMIT, -7_000_000, PTS, 0, HZ60_NS), Some(23_000_000));
    }

    /// A frame cannot reach the glass before it was captured. A wall-clock step or an unsettled
    /// skew estimate produces exactly this, and steering a playback ring by it would empty or
    /// overfill the ring outright — so it is rejected, not clamped.
    #[test]
    fn a_frame_landing_before_its_own_capture_is_rejected() {
        assert_eq!(video_e2e_ns(1_000_000_000, 0, 2_000_000_000, 0, HZ60_NS), None);
    }

    /// This runs on the video thread for every presented frame, so the failure mode it guards is a
    /// **panic**: plain `+` here aborts the stream on overflow in a debug build. Proven by planting
    /// exactly that — swapping the `checked_add`s for `+` fails this test with an overflow panic.
    ///
    /// What it deliberately does NOT claim: that `checked_add` beats `wrapping_add`. With real
    /// inputs the i128 accumulator cannot overflow at all (a realtime stamp is ~1.7e18 ns and the
    /// whole pipeline term is bounded by `u64::MAX` ~1.8e19, against an i128 range of ~1.7e38), and
    /// a wrapped value would be caught by the `u64::try_from` below anyway. The `checked_add`s are
    /// belt-and-braces against garbage arguments, and this gate is about the panic.
    #[test]
    fn implausible_inputs_return_none_rather_than_panicking() {
        assert_eq!(video_e2e_ns(i128::MAX, i64::MAX, 0, u64::MAX, u64::MAX), None);
        assert_eq!(video_e2e_ns(i128::MIN, i64::MIN, u64::MAX, 0, 0), None);
        // A saturating queue term must not wrap into a small, believable-looking number.
        assert_eq!(video_e2e_ns(SUBMIT, 0, PTS, u64::MAX, HZ60_NS), None);
    }

    /// 120 Hz — the mode the ABR collapse was observed at (CX, 2026-08-10).
    const HZ120_NS: u64 = 8_333_333;
    const FEED: Duration = Duration::from_micros(400);

    /// The regression the cushion exists for: folded in raw, a standing 2-frame lead reported
    /// ~16.8 ms of "decode latency" every window and walked the rate to the floor. At the settled
    /// depth the report must be feed time and nothing else.
    #[test]
    fn a_standing_present_cushion_reports_no_decode_latency() {
        for depth in 0..=STANDING_CUSHION_FRAMES {
            assert_eq!(
                decode_report_us(FEED, depth, STANDING_CUSHION_FRAMES, HZ120_NS),
                400,
                "depth {depth} should read as lead, not queue"
            );
        }
    }

    /// The signal the fold exists for, still intact: a queue GROWING past the cushion is the
    /// decoder falling behind, and each extra frame is one drain interval of real latency.
    #[test]
    fn queue_above_the_cushion_is_reported() {
        let two_over = decode_report_us(FEED, STANDING_CUSHION_FRAMES + 2, STANDING_CUSHION_FRAMES, HZ120_NS);
        assert_eq!(two_over, 400 + 2 * 8_333);
        // And deeper is strictly worse — monotonic, or the controller can't act on it.
        let one_over = decode_report_us(FEED, STANDING_CUSHION_FRAMES + 1, STANDING_CUSHION_FRAMES, HZ120_NS);
        assert!(two_over > one_over, "{two_over} !> {one_over}");
    }

    /// Runs on the video thread per presented frame, so garbage must saturate rather than panic or
    /// wrap into a believable figure (`u32::MAX` µs is ~71 min — no threshold mistakes it for calm).
    #[test]
    fn an_implausible_queue_saturates_instead_of_wrapping() {
        assert_eq!(decode_report_us(FEED, u64::MAX, 0, u64::MAX), u32::MAX);
        assert_eq!(decode_report_us(Duration::MAX, 0, 0, HZ120_NS), u32::MAX);
    }

    /// No samples yet is exactly when a raw fold does its damage — the buffer is still filling — so
    /// assume the cushion rather than an empty queue.
    #[test]
    fn the_cushion_never_starts_at_zero() {
        assert_eq!(cushion_frames(std::iter::empty()), STANDING_CUSHION_FRAMES);
        assert_eq!(cushion_frames([0, 0, 1].into_iter()), STANDING_CUSHION_FRAMES);
    }

    /// A mode that settles deeper than the floor teaches its own lead — one frame is 16.6 ms at
    /// 60 Hz, so a fixed floor of two would put such a mode straight back over the threshold.
    #[test]
    fn a_deeper_settled_queue_raises_the_cushion() {
        assert_eq!(cushion_frames([4, 3, 5, 3].into_iter()), 3);
    }

    /// …but only so far. A decoder that stays overloaded past the sample window would otherwise
    /// teach the minimum its own backlog and mute the signal — the same baseline-absorption trap
    /// this fix exists to escape, just faster.
    #[test]
    fn a_sustained_overload_cannot_teach_the_cushion_away() {
        assert_eq!(cushion_frames([40, 38, 44].into_iter()), CUSHION_MAX_FRAMES);
    }
}
