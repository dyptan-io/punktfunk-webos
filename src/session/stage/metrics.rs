//! The two latency figures the video stage derives from one render-queue depth, and the
//! self-calibrating cushion both are read against.
//!
//! Free functions rather than [`VideoStage`](super::VideoStage) methods purely so they are
//! testable: the stage owns a live NDL handle and cannot be built off-device.

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
/// `submit_us` is the whole access unit's feed time, not the last slice's: on a slice-progressive
/// session a picture is submitted in pieces, and the controller is comparing this against a
/// per-picture budget.
///
/// Split out as a free function purely so it is testable — [`VideoStage`] owns a live NDL handle and
/// cannot be built off-device.
pub(super) fn decode_report_us(submit_us: u32, backlog: u64, cushion: u64, frame_interval_ns: u64) -> u32 {
    let queued_ns = backlog.saturating_sub(cushion).saturating_mul(frame_interval_ns);
    let decode_us = u64::from(submit_us).saturating_add(queued_ns / 1_000);
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
