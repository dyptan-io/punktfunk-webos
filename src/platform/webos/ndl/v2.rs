//! NDL `DirectMedia` **v2** (webOS 5+): `NDL_DirectMediaLoad` plus
//! `NDL_DirectVideoPlay(buffer, size, pts)`, a render-buffer query, a flush and HDR mastering
//! metadata. The path every currently-working TV takes.
//!
//! Never calls `NDL_DirectVideoSetArea` — stutters above 1080p, and v2 sizes its own
//! punch-through plane (v1 can't; see [`super::v1`]).
use std::ffi::{c_int, c_longlong, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use super::{arm_load, ensure_init, ensure_not_poisoned, ffi, settle_before_retry, wait_load_completed};
use super::{NdlCodec, FRAME_FED, LOAD_COMPLETED};

/// One silent stereo Opus frame (ss4s `opus_empty_frame_211`) fed once post-load to make
/// sure the NDL Opus decoder is ready before real audio arrives.
const OPUS_EMPTY_FRAME: [u8; 3] = [0xec, 0xff, 0xfe];

/// How long past `load()` [`NdlVideo::ensure_loaded`] holds frames while `LOADCOMPLETED` is
/// missing. Measured from the load, so it follows `LOAD_COMPLETE_TIMEOUT`.
const FEED_ANYWAY_AFTER: Duration = Duration::from_millis(1_000);

/// Opus audio config for NDL. Stereo only (no multistream/surround support).
#[derive(Clone, Copy)]
pub struct NdlAudioConfig {
    pub channels: i32,
    /// kHz, not Hz — NDL's own unit.
    pub sample_rate: f64,
}

impl NdlAudioConfig {
    fn to_union(self) -> ffi::AudioUnion {
        ffi::AudioOpusInfo {
            kind: 3, // NDL_AUDIO_TYPE_OPUS
            unknown1: 0,
            channels: self.channels as c_int,
            unknown2: 0,
            sample_rate: self.sample_rate,
            stream_header: std::ptr::null(),
        }
        .to_union()
    }
}

/// One loaded NDL v2 video decode session. Dropping unloads it (not `NDL_DirectMediaQuit`).
pub struct NdlVideo {
    fns: &'static ffi::V2,
    /// PTS in ms since load (NDL's local clock, not wall-clock or host capture clock).
    load_instant: Instant,
    audio_offloaded: bool,
    /// Serializes `NDL_Direct*` calls (singleton C API not documented as thread-safe).
    ffi: Mutex<()>,
    /// `false` while `LOADCOMPLETED` still hasn't been seen for this load — [`Self::play`]
    /// refuses to feed until it lands or [`FEED_ANYWAY_AFTER`] passes. Latched once, so the
    /// steady-state feed path costs one relaxed load.
    load_confirmed: AtomicBool,
}

impl NdlVideo {
    /// Load NDL video stream. Calls `NDL_DirectMediaInit` on first use.
    /// Audio request is a probe: fails silently on unsupported models, retries video-only.
    pub fn load(app_id: &str, width: i32, height: i32, codec: NdlCodec, audio: Option<NdlAudioConfig>) -> Result<Self> {
        ensure_not_poisoned()?;
        let fns = ffi::v2()?;
        ensure_init(app_id)?;
        let video = ffi::VideoInfo {
            width,
            height,
            kind: codec.ndl_type(),
            unknown1: 0,
        };
        if let Some(audio) = audio {
            match Self::try_load(fns, video, audio.to_union(), true) {
                Ok(loaded) => {
                    // Prime the Opus decoder with one silent frame (ss4s does this right after a
                    // successful audio-enabled load). Best-effort — a failure here doesn't
                    // invalidate the load, so it's logged but not propagated.
                    let mut frame = OPUS_EMPTY_FRAME;
                    // SAFETY: NDL reads `size` bytes synchronously, no pointer retained.
                    let prime =
                        unsafe { (fns.audio_play)(frame.as_mut_ptr() as *mut c_void, frame.len() as c_uint, 0) };
                    if prime != 0 {
                        tracing::warn!("NDL empty-Opus prime failed (ret={prime} error={})", ffi::last_error());
                    }
                    return Ok(loaded);
                }
                Err(e) => {
                    // Video-only fallback: audio offload is optimization, not critical. Unload
                    // first — a failed load may hold decoder resources (docs/NOTES.md).
                    tracing::warn!("NDL audio-enabled load failed ({e:#}) — retrying video-only");
                    // SAFETY: no arguments; best-effort cleanup of the rejected load.
                    let _ = unsafe { (fns.unload)() };
                    // The rejected load's callbacks are indistinguishable from the retry's, so let
                    // them land BEFORE arming below rather than racing them.
                    settle_before_retry();
                }
            }
        }
        Self::try_load(fns, video, ffi::AudioUnion::SILENT, false)
    }

    /// One `NDL_DirectMediaLoad` attempt, waited out to `LOADCOMPLETED`.
    fn try_load(
        fns: &'static ffi::V2,
        video: ffi::VideoInfo,
        audio: ffi::AudioUnion,
        audio_offloaded: bool,
    ) -> Result<Self> {
        let mut info = ffi::DataInfo { video, audio };
        arm_load();
        // SAFETY: `info` is valid for the duration of this call.
        let ret = unsafe { (fns.load)(&mut info, Some(super::on_load_state)) };
        if ret != 0 {
            bail!("NDL_DirectMediaLoad failed: ret={ret} error={}", ffi::last_error());
        }
        // `ret == 0` is "request accepted", not "pipeline ready" — the caller's first feed (and
        // the Opus prime, when there is one) still needs LOADCOMPLETED.
        let confirmed = wait_load_completed();
        Ok(Self {
            fns,
            load_instant: Instant::now(),
            audio_offloaded,
            ffi: Mutex::new(()),
            load_confirmed: AtomicBool::new(confirmed),
        })
    }

    /// Whether NDL accepted the Opus config and is decoding audio itself — if false the
    /// caller must run the software Opus path.
    pub fn audio_offloaded(&self) -> bool {
        self.audio_offloaded
    }

    /// Feed one Opus packet to NDL (only when `audio_offloaded`).
    ///
    /// ⚠ **The two planes are stamped in unrelated time bases, and NDL syncs them against each
    /// other.** This stamps arrival wall-clock (`load_instant.elapsed()`), while video is stamped
    /// with a host-PTS value mapped onto NDL's player clock by `HostPtsAnchor` and then smoothed by
    /// `PtsPacer` (`session::sink`). NDL's own A/V synchronisation therefore regulates against a
    /// fiction: the audio timeline drifts with delivery jitter while the video timeline tracks host
    /// capture cadence.
    ///
    /// Left as-is deliberately. Fixing it properly means both planes sharing ONE anchor, which
    /// means moving anchor ownership out of the sink and onto this handle behind a lock — a change
    /// to the *working* video hot path, made for a path that is off by default (see
    /// `NDL_AUDIO_OFFLOAD`) and that `docs/NOTES.md` records as freezing video outright on webOS
    /// 10.3. Anyone reviving audio offload must fix this first; the symptom would be audio that
    /// drifts against the picture over a session rather than sitting at a constant offset.
    pub fn play_audio(&self, packet: &[u8]) -> Result<()> {
        let pts_ms = self.load_instant.elapsed().as_millis() as c_longlong;
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: NDL reads `size` bytes synchronously and does not retain the pointer.
        let ret = unsafe { (self.fns.audio_play)(packet.as_ptr() as *mut c_void, packet.len() as c_uint, pts_ms) };
        if ret != 0 {
            bail!("NDL_DirectAudioPlay failed: ret={ret} error={}", ffi::last_error());
        }
        Ok(())
    }

    /// Nanoseconds since `load()` (NDL PTS domain). `video_pump`'s pacer clamps its accumulator around this.
    pub(crate) fn elapsed_ns(&self) -> u64 {
        self.load_instant.elapsed().as_nanos() as u64
    }

    /// `Err` while this load hasn't reported `LOADCOMPLETED`: the sink then flushes, holds and
    /// requests a keyframe — exactly what a late load needs, instead of frames into a decoder that
    /// isn't there. Bounded by [`FEED_ANYWAY_AFTER`], since a model that never delivers the
    /// callback must still stream.
    fn ensure_loaded(&self) -> Result<()> {
        if self.load_confirmed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let elapsed = self.load_instant.elapsed();
        if LOAD_COMPLETED.fired() {
            tracing::info!("NDL LOADCOMPLETED landed {elapsed:?} after load");
        } else if elapsed >= FEED_ANYWAY_AFTER {
            tracing::warn!("NDL: still no LOADCOMPLETED after {FEED_ANYWAY_AFTER:?} — feeding anyway");
        } else {
            bail!("NDL pipeline not loaded yet — holding");
        }
        self.load_confirmed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Feed one access unit at `pts_ns` (ns since `load()`), truncated to ms for NDL.
    /// Pass a paced value, not raw `elapsed_ns()`, to preserve inter-frame spacing.
    pub fn play(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.ensure_loaded()?;
        let pts_ms = (pts_ns / 1_000_000) as c_longlong;
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: NDL reads `size` bytes from `buffer` synchronously and does not
        // retain the pointer.
        let ret = unsafe { (self.fns.video_play)(au.as_ptr() as *mut c_void, au.len() as c_uint, pts_ms) };
        if ret != 0 {
            bail!("NDL_DirectVideoPlay failed: ret={ret} error={}", ffi::last_error());
        }
        if FRAME_FED.bump_first() {
            tracing::info!("NDL first frame fed {:?} after load", self.load_instant.elapsed());
        }
        Ok(())
    }

    /// Apply HDR mastering metadata. `meta` and `color` use the same SEI-standard
    /// units NDL expects (G/B/R order per ST.2086), so no conversion is needed.
    ///
    /// `meta: None` (an SDR stream) is a **no-op**: on this platform
    /// `NDL_DirectVideoSetHDRInfo` emits an HDR infoframe on *any* call — it ignores the
    /// SDR `transfer`/`primaries` triplet and flips the panel into HDR picture mode
    /// regardless (observed on OLED65CX with an H.264 SDR stream). So an SDR stream must
    /// not call it at all; its colorimetry rides the bitstream VUI instead. (This means
    /// NDL can't be used to correct a bitstream with missing/"unspecified" VUI colour
    /// info — the earlier reason this was called unconditionally — but forcing the panel
    /// into HDR for SDR content is the worse outcome.)
    pub fn set_color_info(
        &self,
        meta: Option<&punktfunk_core::quic::HdrMeta>,
        color: punktfunk_core::quic::ColorInfo,
    ) -> Result<()> {
        let Some(m) = meta else {
            return Ok(());
        };
        // G/B/R order (ST.2086 convention).
        let [g, b, r] = m.display_primaries;
        let info = ffi::HdrInfo {
            display_primaries_x0: c_int::from(g[0]),
            display_primaries_y0: c_int::from(g[1]),
            display_primaries_x1: c_int::from(b[0]),
            display_primaries_y1: c_int::from(b[1]),
            display_primaries_x2: c_int::from(r[0]),
            display_primaries_y2: c_int::from(r[1]),
            white_point_x: c_int::from(m.white_point[0]),
            white_point_y: c_int::from(m.white_point[1]),
            max_display_mastering_luminance: m.max_display_mastering_luminance as c_int,
            min_display_mastering_luminance: m.min_display_mastering_luminance as c_int,
            max_content_light_level: c_int::from(m.max_cll),
            max_pic_average_light_level: c_int::from(m.max_fall),
            transfer_characteristics: c_int::from(color.transfer),
            color_primaries: c_int::from(color.primaries),
            matrix_coeffs: c_int::from(color.matrix),
            reserved: [0; 32],
        };
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: passed by value; no pointers or aliasing.
        let ret = unsafe { (self.fns.set_hdr_info)(info) };
        if ret != 0 {
            bail!("NDL_DirectVideoSetHDRInfo failed: ret={ret} error={}", ffi::last_error());
        }
        Ok(())
    }

    /// Buffered-but-undisplayed frames in NDL (None if the query fails).
    /// Rising length = decoder behind; flat near-zero with stutter = upstream problem.
    pub fn render_buffer_length(&self) -> Option<i32> {
        let mut length: c_int = 0;
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: `length` is a valid, writable `c_int` for the duration of the call.
        let ret = unsafe { (self.fns.get_render_buffer_length)(&mut length) };
        (ret == 0).then_some(length)
    }

    pub fn flush(&self) -> Result<()> {
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: no arguments.
        let ret = unsafe { (self.fns.flush_render_buffer)() };
        if ret != 0 {
            bail!(
                "NDL_DirectVideoFlushRenderBuffer failed: ret={ret} error={}",
                ffi::last_error()
            );
        }
        Ok(())
    }
}

impl Drop for NdlVideo {
    fn drop(&mut self) {
        // Re-arm so `playing()` stops reporting the load being torn down here.
        arm_load();
        // SAFETY: best-effort teardown; error ignored (Drop can't propagate a Result).
        let _ = unsafe { (self.fns.unload)() };
    }
}
