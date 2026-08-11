//! NDL `DirectMedia` **v1** video (`NDL_DirectVideoOpen/SetCallback/SetArea/
//! PlayWithCallback/Close`) — the API webOS 3.5-4.x ships in `libNDL_directmedia.so.1`, where
//! the v2 surface [`super::v2`] uses does not exist. Ported from ss4s `modules/webos/ndl/webos4`.
//!
//! What it cannot do:
//! - **H.264 only, SDR, BT.709** — ss4s's `GetCapabilities` advertises that and its `OpenVideo`
//!   rejects the rest. HEVC and HDR are refused *before* the handshake (`core::caps` →
//!   `session::connect`); [`NdlV1Video::load`] refuses again as defence in depth.
//! - **No PTS input** — `PlayWithCallback` takes `(data, size, userdata)` and frames present as
//!   fed, so `session`'s pacing and A/V-offset machinery is inert here.
//! - **No render-buffer query, no flush, no HDR call.** The sink tolerates all three.
//! - **Fixed 1920x1080 plane**, placed once via `SetArea` ([`fit_video`], ported verbatim) since
//!   v1 has no native punch-through sizing. Video is an underlay the UI composites over, so the
//!   app never repositions it.
//!
//! Audio stays on software Opus → SDL: v1's `NDL_DirectAudio*` is PCM/AAC/AC3 with no Opus.
//!
//! **The M3/KADP patch is deliberately not ported.** ss4s's `m3_kadp_fix.c` `mprotect`s a vendor
//! code page RWX and NOPs two bytes out of a `MStar` codec-type whitelist so *non-H.264* types
//! pass. We only ever feed H.264 here, and unverifiable patching of vendor code is what
//! `docs/NOTES.md` argues against.
use std::ffi::{c_ulonglong, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Result};

use super::{arm_load, ensure_init, ensure_not_poisoned, ffi, NdlCodec, FRAME_FED, PLAYING};
use crate::platform::webos::device;

/// The v1 video plane's fixed size, regardless of panel resolution (see the module docs).
const PLANE_WIDTH: i32 = 1920;
const PLANE_HEIGHT: i32 = 1080;

/// Letterbox/pillarbox the stream's aspect into the fixed plane — ss4s `FitVideo`, verbatim.
/// Called once at open; never again from the app.
fn fit_video(fns: &ffi::V1, width: i32, height: i32) {
    let (x, y, w, h) = if width <= 0 || height <= 0 {
        (0, 0, PLANE_WIDTH, PLANE_HEIGHT)
    } else {
        let h = PLANE_WIDTH * height / width;
        if h <= PLANE_HEIGHT {
            (0, (PLANE_HEIGHT - h) / 2, PLANE_WIDTH, h)
        } else {
            let w = PLANE_WIDTH * PLANE_HEIGHT / h;
            ((PLANE_WIDTH - w) / 2, 0, w, PLANE_HEIGHT)
        }
    };
    // SAFETY: plain integer arguments.
    let ret = unsafe { (fns.video_set_area)(x, y, w, h) };
    if ret != 0 {
        tracing::warn!("NDL v1 SetArea({x},{y},{w},{h}) failed: ret={ret}");
    }
    tracing::info!("NDL v1 plane: {w}x{h}+{x}+{y} for a {width}x{height} stream");
}

/// v1's frame-done callback, echoing back the feed's `userdata`. Only a "pipeline is alive"
/// signal: with no PTS there is nothing to time the frame index against.
extern "C" fn on_frame(userdata: c_ulonglong) {
    if PLAYING.bump_first() {
        tracing::info!("NDL v1 pipeline confirmed a frame through (frame {userdata})");
    }
}

/// One open v1 video decode session. Process-global like v2's — NDL has no context handle — so
/// dropping this closes *the* video plane.
pub struct NdlV1Video {
    fns: &'static ffi::V1,
    open_instant: Instant,
    /// Feed counter, handed to NDL as each frame's `userdata` and echoed back by [`on_frame`].
    frames_fed: AtomicU64,
    /// Serializes `NDL_Direct*` calls (singleton C API not documented as thread-safe).
    ffi: Mutex<()>,
}

impl NdlV1Video {
    /// Open the v1 video plane for a `width`x`height` H.264 stream.
    pub fn load(app_id: &str, width: i32, height: i32, codec: NdlCodec) -> Result<Self> {
        ensure_not_poisoned()?;
        // Defence in depth: `session::connect` never advertises HEVC here, so H.265 means its
        // guard was bypassed — and feeding HEVC to this decoder is a black screen, not an error.
        if codec != NdlCodec::H264 {
            bail!("NDL v1 decodes H.264 only (asked for {codec:?}) — see platform::webos::ndl::v1");
        }
        if device::jail_config_broken() {
            bail!(
                "this TV's jailer config leaves /dev/rtkmem unreadable (machine {}), so NDL v1 \
                 cannot access the decoder",
                device::machine_name().unwrap_or_else(|| "unknown".into()),
            );
        }
        let fns = ffi::v1()?;
        ensure_init(app_id)?;
        let mut info = ffi::V1VideoInfo { width, height };
        // Re-arm before opening, so the reveal gate can't be satisfied by a previous session.
        arm_load();
        // SAFETY: `info` is valid for the duration of this call; NDL copies what it needs.
        let ret = unsafe { (fns.video_open)(&mut info) };
        if ret != 0 {
            bail!("NDL_DirectVideoOpen failed: ret={ret}");
        }
        // SAFETY: `on_frame` is an `extern "C"` fn with no captured state.
        let ret = unsafe { (fns.video_set_callback)(Some(on_frame)) };
        if ret != 0 {
            tracing::warn!("NDL v1 SetCallback failed: ret={ret} — presence signal unavailable");
        }
        fit_video(fns, width, height);
        Ok(Self {
            fns,
            open_instant: Instant::now(),
            frames_fed: AtomicU64::new(0),
            ffi: Mutex::new(()),
        })
    }

    /// Feed one access unit. No PTS: v1 presents frames as they are fed (see the module docs),
    /// so the caller's paced timestamp has nowhere to go.
    pub fn play(&self, au: &[u8]) -> Result<()> {
        let frame = self.frames_fed.fetch_add(1, Ordering::Relaxed);
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: NDL reads `size` bytes synchronously and does not retain the pointer.
        let ret = unsafe { (self.fns.video_play_with_callback)(au.as_ptr() as *mut c_void, au.len(), frame) };
        if ret != 0 {
            bail!("NDL_DirectVideoPlayWithCallback failed: ret={ret}");
        }
        // Same reveal gate as v2, and not left to `on_frame` alone: a model that never delivers
        // the callback would hold the menu up until `runtime::stream`'s reveal timeout.
        if FRAME_FED.bump_first() {
            tracing::info!("NDL v1 first frame fed {:?} after open", self.open_instant.elapsed());
        }
        Ok(())
    }
}

impl Drop for NdlV1Video {
    fn drop(&mut self) {
        // Re-arm so the reveal gate stops reporting the session being torn down here.
        arm_load();
        let _ffi = self.ffi.lock();
        // SAFETY: best-effort teardown; error ignored (Drop can't propagate a Result).
        let ret = unsafe { (self.fns.video_close)() };
        if ret != 0 {
            tracing::warn!("NDL_DirectVideoClose failed: ret={ret}");
        }
    }
}
