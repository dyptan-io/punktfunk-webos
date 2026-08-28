//! Video end-to-end estimation from NDL's render-queue depth.
//!
//! Free functions rather than [`VideoStage`](super::VideoStage) methods purely so they are
//! testable: the stage owns a live NDL handle and cannot be built off-device.

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
