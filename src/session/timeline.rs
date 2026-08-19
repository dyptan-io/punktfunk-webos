//! Timeline plumbing for the video pump: the panel-reconciled frame interval and the
//! host-PTS → player-clock anchor NDL needs (it has no PTS clock of its own).

use crate::platform::webos::sdl_webos;

/// Returns panel refresh in Hz, or `None` on query failure/implausible values — including
/// an SDL that has no such query (`platform::webos::sdl_webos`), where the caller falls back
/// to the stream's own rate.
fn panel_refresh_hz() -> Option<u32> {
    let fns = sdl_webos::fns().ok()?;
    let mut rate: std::os::raw::c_int = 0;
    // SAFETY: single out-param, no aliasing; read-only panel query.
    let ok = unsafe { (fns.get_refresh_rate)(&mut rate) };
    (ok != 0 && (20..=240).contains(&rate)).then_some(rate as u32)
}

/// Frame interval (ns): panel cadence if within ±2 Hz of stream, else stream rate.
pub fn reconciled_frame_interval_ns(stream_hz: u32) -> u64 {
    let hz = match panel_refresh_hz() {
        Some(panel_hz) if stream_hz.abs_diff(panel_hz) <= 2 => {
            tracing::info!("frame interval anchored to panel {panel_hz}Hz (stream {stream_hz}Hz)");
            panel_hz
        }
        Some(panel_hz) => {
            tracing::info!("frame interval on stream {stream_hz}Hz (panel {panel_hz}Hz differs by >2Hz)");
            stream_hz
        }
        None => stream_hz,
    };
    1_000_000_000 / u64::from(hz.max(1))
}

/// Maps the host's capture-clock PTS onto NDL's own player clock, anchored once at the
/// first frame of a run: `base = player_anchor + (host_pts - host_anchor)`. Keeps the
/// video and offloaded audio on one shared mapping (see
/// [`crate::session::sink::VideoPlayer::pts_base_ns`]). Same anchoring as SS4S's
/// `ndl_player.c::SS4S_NDL_webOS5_NextVideoPts`. Reset after a freeze-until-reanchor hold,
/// where the timeline jumps.
#[derive(Default)]
pub struct HostPtsAnchor {
    /// `(host_pts_ns, player_clock_ns)` of the frame the current run anchored on.
    anchor: Option<(u64, u64)>,
}

impl HostPtsAnchor {
    pub fn reset(&mut self) {
        self.anchor = None;
    }

    /// Base reference for `host_pts_ns`. First call anchors on `player_clock_ns` and
    /// returns it verbatim; later calls project the host PTS delta forward, floored at 0
    /// (a host PTS going backwards vs. the anchor would otherwise underflow).
    pub fn map(&mut self, host_pts_ns: u64, player_clock_ns: u64) -> u64 {
        match self.anchor {
            None => {
                self.anchor = Some((host_pts_ns, player_clock_ns));
                player_clock_ns
            }
            Some((host0, player0)) => {
                let delta = host_pts_ns as i64 - host0 as i64;
                (player0 as i64 + delta).max(0) as u64
            }
        }
    }
}
