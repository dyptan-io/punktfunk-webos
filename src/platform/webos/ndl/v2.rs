//! NDL `DirectMedia` **v2** (webOS 5+): `NDL_DirectMediaLoad` plus
//! `NDL_DirectVideoPlay(buffer, size, pts)`, a render-buffer query, a flush and HDR mastering
//! metadata. The path every currently-working TV takes.
//!
//! Never calls `NDL_DirectVideoSetArea` — stutters above 1080p, and v2 sizes its own
//! punch-through plane (v1 can't; see [`super::v1`]).
use std::ffi::{c_int, c_longlong, c_uint, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use super::{arm_load, ensure_init, ensure_not_poisoned, ffi, settle_before_retry, wait_load_completed};
use super::{lock_ffi, mark_frame_fed_logged, NdlCodec, LOAD_COMPLETED};

/// How long past `load()` [`NdlVideo::ensure_loaded`] holds frames while `LOADCOMPLETED` is
/// missing. Measured from the load, so it follows `LOAD_COMPLETE_TIMEOUT`.
const FEED_ANYWAY_AFTER: Duration = Duration::from_millis(1_000);

/// [`NdlVideo::play`] refusing a frame because `LOADCOMPLETED` hasn't landed. A distinct type
/// because the caller must NOT respond to it the way it responds to a decode error: the usual
/// answer is `NDL_DirectVideoFlushRenderBuffer`, and issuing that against a pipeline NDL has not
/// finished loading takes the session's audio out for good (see `session::sink`).
#[derive(Debug)]
pub struct NotLoadedYet;

impl std::fmt::Display for NotLoadedYet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NDL pipeline not loaded yet — holding")
    }
}

impl std::error::Error for NotLoadedYet {}

/// [`NdlVideo::pts_offset_ns`] before the video plane has published one. Not 0 — a genuine
/// offset of 0 ns is possible, and "unset" must be distinguishable from it.
const NO_PTS_OFFSET: i64 = i64::MIN;

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
    /// Host-PTS → NDL-player-clock offset in ns, republished by the video plane on every fed
    /// frame (`session::sink`) and read by [`Self::play_audio`] so both planes land in ONE
    /// timeline. [`NO_PTS_OFFSET`] until the first frame is fed.
    pts_offset_ns: AtomicI64,
    /// Highest audio stamp fed so far (ms), so [`Self::play_audio`] can never hand NDL a
    /// timestamp going backwards. Never reset — the ceiling has to survive a re-latch, which is
    /// exactly the case that would otherwise rewind it.
    last_audio_pts_ms: AtomicI64,
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
        ensure_init(app_id, true)?;
        let video = ffi::VideoInfo {
            width,
            height,
            kind: codec.ndl_type(),
            unknown1: 0,
        };
        if let Some(audio) = audio {
            match Self::try_load(fns, video, audio.to_union(), true) {
                Ok(loaded) => return Ok(loaded),
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
            pts_offset_ns: AtomicI64::new(NO_PTS_OFFSET),
            last_audio_pts_ms: AtomicI64::new(0),
            load_confirmed: AtomicBool::new(confirmed),
        })
    }

    /// Whether NDL accepted the Opus config and is decoding audio itself — if false the
    /// caller must run the software Opus path.
    pub fn audio_offloaded(&self) -> bool {
        self.audio_offloaded
    }

    /// Latch the video plane's host-PTS → player-clock offset so [`play_audio`](Self::play_audio)
    /// can stamp audio in the same timeline. Called per fed frame from `session::sink`, but takes
    /// **only the first** value after each [`clear_pts_offset`](Self::clear_pts_offset).
    ///
    /// Latched, not republished per frame: the offset is a mapping between two clocks, and it is
    /// stable only while the video plane's own anchor is. Re-deriving it every frame lets any jump
    /// in the video timeline — a receive-backlog flush jumping to live drops frames, so host PTS
    /// leaps forward while the player clock does not — drag the audio stamp *backwards* by the
    /// size of the jump. NDL takes that as a rewind and stops playing audio for the rest of the
    /// session (observed on CX: audio worked in a session with no flush, gone in one with).
    /// `clear_pts_offset` at the sink's anchor resets is what re-derives it.
    pub(crate) fn latch_pts_offset(&self, offset_ns: i64) {
        // The steady state is "already latched", and this runs per fed frame — keep the exclusive
        // access off the video thread's hot path once the offset is set.
        if self.pts_offset_ns.load(Ordering::Relaxed) != NO_PTS_OFFSET {
            return;
        }
        let _ = self
            .pts_offset_ns
            .compare_exchange(NO_PTS_OFFSET, offset_ns, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Drop the latched offset — the two timelines just decoupled (the sink reset its anchor
    /// after a freeze-until-reanchor hold, or on the pacing off→on edge).
    /// [`play_audio`](Self::play_audio) holds packets until the video plane latches a fresh one.
    pub(crate) fn clear_pts_offset(&self) {
        self.pts_offset_ns.store(NO_PTS_OFFSET, Ordering::Relaxed);
    }

    /// Feed one Opus packet to NDL (only when `audio_offloaded`). `host_pts_ns` is the packet's
    /// own host capture timestamp, NOT arrival time.
    ///
    /// **Both planes must be stamped in one time base** — NDL runs its own A/V synchronisation
    /// against these values, and regulating a video plane on host-capture cadence against an audio
    /// plane on arrival wall-clock is what froze the picture on webOS 10.3 (docs/NOTES.md § "Opus
    /// offload to NDL"). So the host PTS goes through the video plane's own offset
    /// ([`latch_pts_offset`](Self::latch_pts_offset)).
    ///
    /// Returns `Ok(())` having fed nothing while no offset is latched: audio before the first
    /// video frame has no timeline to join yet, and dropping those few packets beats feeding them
    /// at a stamp that jumps once the real offset lands.
    ///
    /// The stamp is also floored at the previous one — NDL reads a timestamp going backwards (an
    /// out-of-order packet, or a re-latch after a hold) as a rewind and mutes the rest of the
    /// session rather than resyncing.
    pub fn play_audio(&self, packet: &[u8], host_pts_ns: u64) -> Result<()> {
        let offset_ns = self.pts_offset_ns.load(Ordering::Relaxed);
        if offset_ns == NO_PTS_OFFSET {
            return Ok(());
        }
        let raw_ms = (host_pts_ns as i64).saturating_add(offset_ns).max(0) / 1_000_000;
        let pts_ms = self.last_audio_pts_ms.fetch_max(raw_ms, Ordering::Relaxed).max(raw_ms) as c_longlong;
        let _ffi = lock_ffi();
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
            return Err(NotLoadedYet.into());
        }
        self.load_confirmed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Feed one access unit at `pts_ns` (ns since `load()`), truncated to ms for NDL.
    /// Pass a paced value, not raw `elapsed_ns()`, to preserve inter-frame spacing.
    pub fn play(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.ensure_loaded()?;
        let pts_ms = (pts_ns / 1_000_000) as c_longlong;
        let _ffi = lock_ffi();
        // SAFETY: NDL reads `size` bytes from `buffer` synchronously and does not
        // retain the pointer.
        let ret = unsafe { (self.fns.video_play)(au.as_ptr() as *mut c_void, au.len() as c_uint, pts_ms) };
        if ret != 0 {
            bail!("NDL_DirectVideoPlay failed: ret={ret} error={}", ffi::last_error());
        }
        mark_frame_fed_logged("NDL", self.load_instant);
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
            display_primaries_x0: c_uint::from(g[0]),
            display_primaries_y0: c_uint::from(g[1]),
            display_primaries_x1: c_uint::from(b[0]),
            display_primaries_y1: c_uint::from(b[1]),
            display_primaries_x2: c_uint::from(r[0]),
            display_primaries_y2: c_uint::from(r[1]),
            white_point_x: c_uint::from(m.white_point[0]),
            white_point_y: c_uint::from(m.white_point[1]),
            max_display_mastering_luminance: m.max_display_mastering_luminance as c_uint,
            min_display_mastering_luminance: m.min_display_mastering_luminance as c_uint,
            max_content_light_level: c_uint::from(m.max_cll),
            max_pic_average_light_level: c_uint::from(m.max_fall),
            transfer_characteristics: c_uint::from(color.transfer),
            color_primaries: c_uint::from(color.primaries),
            matrix_coeffs: c_uint::from(color.matrix),
            reserved: [0; 32],
        };
        let _ffi = lock_ffi();
        // SAFETY: passed by value; no pointers or aliasing.
        let ret = unsafe { (self.fns.set_hdr_info)(info) };
        if ret != 0 {
            bail!(
                "NDL_DirectVideoSetHDRInfo failed: ret={ret} error={}",
                ffi::last_error()
            );
        }
        Ok(())
    }

    /// Buffered-but-undisplayed frames in NDL (None if the query fails).
    /// Rising length = decoder behind; flat near-zero with stutter = upstream problem.
    pub fn render_buffer_length(&self) -> Option<i32> {
        let mut length: c_int = 0;
        let _ffi = lock_ffi();
        // SAFETY: `length` is a valid, writable `c_int` for the duration of the call.
        let ret = unsafe { (self.fns.get_render_buffer_length)(&mut length) };
        (ret == 0).then_some(length)
    }

    pub fn flush(&self) -> Result<()> {
        // Never against a pipeline that hasn't reported LOADCOMPLETED: the flush silently kills
        // the session's audio plane for the rest of the load (see [`NotLoadedYet`]), and nothing
        // has been fed yet, so there is no render buffer to discard either. The sink's loss path
        // flushes before it ever calls `play`, so the guard has to live here.
        if !self.load_confirmed.load(Ordering::Relaxed) && !LOAD_COMPLETED.fired() {
            return Ok(());
        }
        let _ffi = lock_ffi();
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
