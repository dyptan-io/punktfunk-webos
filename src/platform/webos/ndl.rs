//! Safe wrapper over webOS's NDL `DirectMedia` v2 API (`NDL_Direct*`, webOS 5+).
//! Video only; audio via SDL2. Never calls `NDL_DirectVideoSetArea` (causes stutter above 1080p).
use std::ffi::{c_char, c_int, c_longlong, c_uint, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{bail, Result};

#[repr(C)]
#[derive(Clone, Copy)]
struct NdlVideoInfo {
    width: c_int,
    height: c_int,
    /// `NDL_VIDEO_TYPE`: 1=H264, 2=H265, 3=VP9.
    kind: c_int,
    unknown1: c_int,
}

/// NDL audio union (8-byte aligned). Tag 0 = no audio (all-zero).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct NdlAudioUnion {
    bytes: [u8; 32],
}

/// `NDL_DIRECTMEDIA_AUDIO_OPUS_INFO_T` (field-for-field match).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct NdlAudioOpusInfo {
    kind: c_int,
    unknown1: c_int,
    channels: c_int,
    unknown2: c_int,
    sample_rate: f64,
    /// Stream header (undocumented, passed null).
    stream_header: *const c_char,
}

/// Opus audio config for NDL. Stereo only (no multistream/surround support).
#[derive(Clone, Copy)]
pub struct NdlAudioConfig {
    pub channels: i32,
    pub sample_rate: f64,
}

impl NdlAudioConfig {
    fn to_union(self) -> NdlAudioUnion {
        let info = NdlAudioOpusInfo {
            kind: 3, // NDL_AUDIO_TYPE_OPUS
            unknown1: 0,
            channels: self.channels as c_int,
            unknown2: 0,
            sample_rate: self.sample_rate,
            stream_header: std::ptr::null(),
        };
        let mut bytes = [0u8; 32];
        // SAFETY: `NdlAudioOpusInfo` is `repr(C)` and no larger than the union's 32-byte
        // arm (the header's own `char padding[32]`), so this copy stays in bounds. Any
        // trailing bytes remain zero, matching the C compiler's own padding.
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(&info).cast::<u8>(),
                bytes.as_mut_ptr(),
                std::mem::size_of::<NdlAudioOpusInfo>().min(32),
            );
        }
        NdlAudioUnion { bytes }
    }
}

#[repr(C)]
struct NdlDataInfo {
    video: NdlVideoInfo,
    audio: NdlAudioUnion,
}

/// `NDL_VIDEO_TYPE` values this client can request (matches the codec the host's
/// `Welcome` resolved — see `punktfunk_core::quic::CODEC_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NdlCodec {
    H264,
    H265,
}

impl NdlCodec {
    fn ndl_type(self) -> c_int {
        match self {
            Self::H264 => 1,
            Self::H265 => 2,
        }
    }

    /// From `punktfunk_core::quic::CODEC_*` wire bit.
    pub fn from_wire(codec: u8) -> Option<Self> {
        match codec {
            punktfunk_core::quic::CODEC_H264 => Some(Self::H264),
            punktfunk_core::quic::CODEC_HEVC => Some(Self::H265),
            _ => None,
        }
    }
}

/// Mirrors `NDL_DIRECTVIDEO_HDR_INFO_T` field-for-field — the field names are the
/// H.265 `mastering_display_colour_volume`/`content_light_level_info` SEI syntax
/// element names verbatim, so punktfunk's own `HdrMeta` (same SEI-derived fields,
/// same units) copies straight across with no unit conversion.
#[repr(C)]
struct NdlHdrInfo {
    display_primaries_x0: c_int,
    display_primaries_y0: c_int,
    display_primaries_x1: c_int,
    display_primaries_y1: c_int,
    display_primaries_x2: c_int,
    display_primaries_y2: c_int,
    white_point_x: c_int,
    white_point_y: c_int,
    max_display_mastering_luminance: c_int,
    min_display_mastering_luminance: c_int,
    max_content_light_level: c_int,
    max_pic_average_light_level: c_int,
    transfer_characteristics: c_int,
    color_primaries: c_int,
    matrix_coeffs: c_int,
    reserved: [u8; 32],
}

#[allow(non_camel_case_types)]
type ResourceReleased = Option<extern "C" fn(*const c_char)>;
#[allow(non_camel_case_types)]
type NdlMediaLoadCallback = Option<extern "C" fn(c_int, c_longlong, *const c_char)>;

#[link(name = "NDL_directmedia")]
extern "C" {
    fn NDL_DirectMediaGetError() -> *const c_char;
    fn NDL_DirectMediaInit(app_id: *const c_char, cb: ResourceReleased) -> c_int;
    fn NDL_DirectMediaQuit() -> c_int;
    fn NDL_DirectMediaLoad(data: *mut NdlDataInfo, cb: NdlMediaLoadCallback) -> c_int;
    fn NDL_DirectMediaUnload() -> c_int;
    fn NDL_DirectVideoPlay(buffer: *mut c_void, size: c_uint, pts: c_longlong) -> c_int;
    fn NDL_DirectVideoFlushRenderBuffer() -> c_int;
    fn NDL_DirectVideoGetRenderBufferLength(length: *mut c_int) -> c_int;
    fn NDL_DirectAudioPlay(buffer: *mut c_void, size: c_uint, pts: c_longlong) -> c_int;
    fn NDL_DirectVideoSetHDRInfo(hdr_info: NdlHdrInfo) -> c_int;
}

/// NDL load-state values reported through the media-load callback (ss4s logs these).
const NDL_STATE_LOADCOMPLETED: c_int = 0x16;
const NDL_STATE_UNLOADCOMPLETED: c_int = 0x17;
const NDL_STATE_PLAYING: c_int = 0x1a;

/// One silent stereo Opus frame (ss4s `opus_empty_frame_211`) fed once post-load to make
/// sure the NDL Opus decoder is ready before real audio arrives.
const OPUS_EMPTY_FRAME: [u8; 3] = [0xec, 0xff, 0xfe];

/// Bound, not a requirement: feeding an unloaded decoder is the first-frames-black cause,
/// but a model that never delivers the callback must still stream.
const LOAD_COMPLETE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Grace for a rejected load's callbacks to land before the video-only retry arms. The callback
/// carries nothing identifying its load, so separating the two in TIME is the only way to stop a
/// stale `LOADCOMPLETED` satisfying the retry's wait — and feeding an unloaded decoder is what
/// turns a launch black.
const CALLBACK_SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

/// How long past `load()` [`NdlVideo::ensure_loaded`] holds frames while `LOADCOMPLETED` is
/// missing. Measured from the load, so it follows [`LOAD_COMPLETE_TIMEOUT`].
const FEED_ANYWAY_AFTER: std::time::Duration = std::time::Duration::from_millis(1_000);

/// Counters, not flags: a late event still increments, so it stays attributable to the load it
/// came from — a per-load sticky bool cannot tell "this load completed" from "the previous one's
/// callback arrived a moment too late". Process-global like the NDL session itself.
static LOAD_COMPLETED_SEQ: AtomicU64 = AtomicU64::new(0);
static UNLOAD_COMPLETED_SEQ: AtomicU64 = AtomicU64::new(0);
/// NDL's own present-pipeline signal (docs/NDL-FRAMERATE-INVESTIGATION.md). Measured on a G5:
/// it lands during `load()`, BEFORE any frame is fed, so it says nothing about there being a
/// picture — kept for the log line only, never as a reveal gate (see [`PRESENTED_SEQ`]).
static PLAYING_SEQ: AtomicU64 = AtomicU64::new(0);
/// Bumped by the first `play()` NDL accepts for the armed load. This, not `PLAYING`, is what
/// makes uncovering the punch-through plane safe: before it there is provably nothing on the
/// plane, and a host that delivers its first frame seconds late (new-flow stall, startup
/// capacity probe) would otherwise show as seconds of black.
static PRESENTED_SEQ: AtomicU64 = AtomicU64::new(0);
/// Counter values as of the current load, stamped by [`arm_load`]. Anything at or below these
/// belongs to an earlier load.
static LOAD_COMPLETED_BASE: AtomicU64 = AtomicU64::new(0);
static PLAYING_BASE: AtomicU64 = AtomicU64::new(0);
static PRESENTED_BASE: AtomicU64 = AtomicU64::new(0);

/// Records NDL load-state transitions so `load()`, `play()` and the UI reveal can wait on them.
extern "C" fn on_load_state(state: c_int, _num: c_longlong, _str: *const c_char) {
    let name = match state {
        NDL_STATE_LOADCOMPLETED => {
            LOAD_COMPLETED_SEQ.fetch_add(1, Ordering::SeqCst);
            "LOADCOMPLETED"
        }
        NDL_STATE_UNLOADCOMPLETED => {
            UNLOAD_COMPLETED_SEQ.fetch_add(1, Ordering::SeqCst);
            "UNLOADCOMPLETED"
        }
        NDL_STATE_PLAYING => {
            PLAYING_SEQ.fetch_add(1, Ordering::SeqCst);
            "PLAYING"
        }
        _ => "other",
    };
    tracing::info!("NDL load state: {name} (0x{state:x})");
}

/// Takes the counters as the baseline for the load about to be issued. Call immediately before
/// every `NDL_DirectMediaLoad`, and after an unload so `playing()` stops reporting a dead load.
fn arm_load() {
    LOAD_COMPLETED_BASE.store(LOAD_COMPLETED_SEQ.load(Ordering::SeqCst), Ordering::SeqCst);
    PLAYING_BASE.store(PLAYING_SEQ.load(Ordering::SeqCst), Ordering::SeqCst);
    PRESENTED_BASE.store(PRESENTED_SEQ.load(Ordering::SeqCst), Ordering::SeqCst);
}

/// Whether `LOADCOMPLETED` has landed for the armed load.
fn load_completed() -> bool {
    LOAD_COMPLETED_SEQ.load(Ordering::SeqCst) > LOAD_COMPLETED_BASE.load(Ordering::SeqCst)
}

/// `PLAYING` for the current load. Diagnostics only — see [`PLAYING_SEQ`].
pub fn playing() -> bool {
    PLAYING_SEQ.load(Ordering::SeqCst) > PLAYING_BASE.load(Ordering::SeqCst)
}

/// Whether a frame of the current load has reached NDL: the plane has a picture and is safe to
/// uncover.
pub fn presenting() -> bool {
    PRESENTED_SEQ.load(Ordering::SeqCst) > PRESENTED_BASE.load(Ordering::SeqCst)
}

/// Lets the rejected audio-enabled load's callbacks land before the video-only retry is armed:
/// waits for its `UNLOADCOMPLETED`, then a fixed settle for anything still in flight behind it.
fn settle_before_retry() {
    let unloads = UNLOAD_COMPLETED_SEQ.load(Ordering::SeqCst);
    let start = Instant::now();
    while UNLOAD_COMPLETED_SEQ.load(Ordering::SeqCst) == unloads && start.elapsed() < CALLBACK_SETTLE {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::thread::sleep(CALLBACK_SETTLE);
}

/// Blocks until the armed load's `LOADCOMPLETED` lands. Returns `false` on timeout.
fn wait_load_completed() -> bool {
    let start = Instant::now();
    while !load_completed() {
        if start.elapsed() >= LOAD_COMPLETE_TIMEOUT {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    true
}

/// Reads NDL's last error string (set on the most recent failing call).
fn last_error() -> String {
    // SAFETY: returns a pointer to NDL's internal buffer; only borrowed here.
    unsafe {
        let p = NDL_DirectMediaGetError();
        if p.is_null() {
            "(no NDL error message)".to_string()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

static INIT_DONE: AtomicBool = AtomicBool::new(false);

/// Count of video/audio pump threads leaked past `SHUTDOWN_JOIN_TIMEOUT` (see
/// `session::join_with_timeout`), not yet confirmed exited. A leaked thread may still be
/// inside an `NDL_Direct*` call and still holds a live `NdlVideo` with its own unsynchronized
/// `ffi` mutex — a second `NDL_DirectMediaLoad` on top of that races it instead of starting
/// clean, reproducing as an undecodable stream rather than a clean failure.
///
/// No way to force this: an OS thread can't be safely cancelled mid-FFI-call, and racing
/// `NDL_DirectMediaUnload` against it is the exact hazard this guards against. So `load()`
/// refuses while nonzero, and dropping the [`LeakGuard`] clears it in-process (no restart) once the
/// leaked thread actually returns — its `NdlVideo::drop` has run the real unload by then.
static LEAKED_THREADS: AtomicUsize = AtomicUsize::new(0);

/// One leaked NDL-touching thread, for as long as this value lives (see [`LEAKED_THREADS`]).
///
/// A guard rather than a `poison`/`recovered` pair of calls: both ways of mispairing them are
/// silent — a missed decrement refuses streaming until restart, an extra one re-permits it while a
/// thread is still inside NDL. Ownership makes the pairing structural.
pub struct LeakGuard(());

impl Drop for LeakGuard {
    fn drop(&mut self) {
        if LEAKED_THREADS.fetch_sub(1, Ordering::SeqCst) == 1 {
            tracing::info!(
                "NDL recovered: the wedged decode thread finished and unloaded cleanly — streaming re-enabled"
            );
        }
    }
}

/// Marks one NDL-touching thread as leaked until the returned guard drops — which the caller must
/// arrange to happen when that same thread actually finishes, however late. Leaking the guard
/// (`mem::forget`) is the deliberate "nothing will ever tell us it recovered" case.
#[must_use = "NDL stays poisoned until this guard drops — hold it until the wedged thread returns"]
pub fn poison() -> LeakGuard {
    if LEAKED_THREADS.fetch_add(1, Ordering::SeqCst) == 0 {
        tracing::error!(
            "NDL poisoned: a decode thread is wedged past its join deadline — streaming \
             refused until it actually finishes (no safe way to force it sooner)"
        );
    }
    LeakGuard(())
}

/// `Err` while any leaked thread might still be live (see [`LEAKED_THREADS`]). One gate, one
/// wording: `load()` enforces it and `session::connect` checks it early to avoid holding a host
/// session slot for a connect that can only end here.
pub fn ensure_not_poisoned() -> Result<()> {
    if LEAKED_THREADS.load(Ordering::SeqCst) > 0 {
        bail!("NDL is still tearing down a wedged decode thread from the previous session — try reconnecting shortly");
    }
    Ok(())
}

/// Calls `NDL_DirectMediaInit` once (process-global, idempotent-guarded).
fn ensure_init(app_id: &str) -> Result<()> {
    if INIT_DONE.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let c_app_id = CString::new(app_id).unwrap_or_default();
    // SAFETY: `c_app_id` is valid for the duration of this call.
    let ret = unsafe { NDL_DirectMediaInit(c_app_id.as_ptr(), None) };
    if ret != 0 {
        INIT_DONE.store(false, Ordering::SeqCst);
        bail!("NDL_DirectMediaInit failed: ret={ret} error={}", last_error());
    }
    Ok(())
}

/// The app id NDL is initialised with. Overridable for dev builds installed under another id —
/// NDL keys its session on the caller's app id, so a mismatch fails the load.
pub fn app_id() -> String {
    std::env::var("APPID").unwrap_or_else(|_| "io.dyptan.punktfunk.webos".into())
}

/// One loaded NDL video decode session. Dropping unloads it (not `NDL_DirectMediaQuit`).
pub struct NdlVideo {
    /// PTS in ms since load (NDL's local clock, not wall-clock or host capture clock).
    load_instant: Instant,
    audio_offloaded: bool,
    /// Serializes `NDL_Direct*` calls (singleton C API not documented as thread-safe).
    ffi: std::sync::Mutex<()>,
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
        ensure_init(app_id)?;
        let video = NdlVideoInfo {
            width,
            height,
            kind: codec.ndl_type(),
            unknown1: 0,
        };
        if let Some(audio) = audio {
            let mut info = NdlDataInfo {
                video,
                audio: audio.to_union(),
            };
            arm_load();
            // SAFETY: `info` is valid for the duration of this call.
            let ret = unsafe { NDL_DirectMediaLoad(&mut info, Some(on_load_state)) };
            if ret == 0 {
                // `ret == 0` means the request was accepted, not that the pipeline is ready —
                // the Opus prime below and the caller's first `play()` both need LOADCOMPLETED.
                let confirmed = wait_load_completed();
                if !confirmed {
                    tracing::warn!(
                        "NDL load: no LOADCOMPLETED within {LOAD_COMPLETE_TIMEOUT:?} — holding the first frames"
                    );
                }
                // Prime the Opus decoder with one silent frame (ss4s does this right after a
                // successful audio-enabled load). Best-effort — a failure here doesn't
                // invalidate the load, so it's logged but not propagated.
                let mut frame = OPUS_EMPTY_FRAME;
                // SAFETY: NDL reads `size` bytes synchronously and does not retain the pointer.
                let prime = unsafe { NDL_DirectAudioPlay(frame.as_mut_ptr() as *mut c_void, frame.len() as c_uint, 0) };
                if prime != 0 {
                    tracing::warn!("NDL empty-Opus prime failed (ret={prime} error={})", last_error());
                }
                return Ok(Self {
                    load_instant: Instant::now(),
                    audio_offloaded: true,
                    ffi: std::sync::Mutex::new(()),
                    load_confirmed: AtomicBool::new(confirmed),
                });
            }
            // Fall through to video-only: audio offload is optimization, not critical.
            // Unload first: failed load may hold decoder resources (docs/NOTES.md).
            tracing::warn!(
                "NDL audio-enabled load failed (ret={ret} error={}) — retrying video-only",
                last_error()
            );
            let _ = unsafe { NDL_DirectMediaUnload() };
            // The rejected load's callbacks are indistinguishable from the retry's, so let them
            // land BEFORE arming below rather than racing them.
            settle_before_retry();
        }
        let mut info = NdlDataInfo {
            video,
            audio: NdlAudioUnion { bytes: [0; 32] },
        };
        arm_load();
        // SAFETY: `info` is valid for the duration of this call.
        let ret = unsafe { NDL_DirectMediaLoad(&mut info, Some(on_load_state)) };
        if ret != 0 {
            bail!("NDL_DirectMediaLoad failed: ret={ret} error={}", last_error());
        }
        let confirmed = wait_load_completed();
        if !confirmed {
            tracing::warn!("NDL load: no LOADCOMPLETED within {LOAD_COMPLETE_TIMEOUT:?} — holding the first frames");
        }
        Ok(Self {
            load_instant: Instant::now(),
            audio_offloaded: false,
            ffi: std::sync::Mutex::new(()),
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
        let ret = unsafe { NDL_DirectAudioPlay(packet.as_ptr() as *mut c_void, packet.len() as c_uint, pts_ms) };
        if ret != 0 {
            bail!("NDL_DirectAudioPlay failed: ret={ret} error={}", last_error());
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
        if load_completed() {
            tracing::info!("NDL LOADCOMPLETED landed {:?} after load", self.load_instant.elapsed());
            self.load_confirmed.store(true, Ordering::Relaxed);
            return Ok(());
        }
        if self.load_instant.elapsed() >= FEED_ANYWAY_AFTER {
            tracing::warn!("NDL: still no LOADCOMPLETED after {FEED_ANYWAY_AFTER:?} — feeding anyway");
            self.load_confirmed.store(true, Ordering::Relaxed);
            return Ok(());
        }
        bail!("NDL pipeline not loaded yet — holding");
    }

    /// Feed one access unit at `pts_ns` (ns since `load()`), truncated to ms for NDL.
    /// Pass a paced value, not raw `elapsed_ns()`, to preserve inter-frame spacing.
    pub fn play(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.ensure_loaded()?;
        let pts_ms = (pts_ns / 1_000_000) as c_longlong;
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: NDL reads `size` bytes from `buffer` synchronously and does not
        // retain the pointer.
        let ret = unsafe { NDL_DirectVideoPlay(au.as_ptr() as *mut c_void, au.len() as c_uint, pts_ms) };
        if ret != 0 {
            bail!("NDL_DirectVideoPlay failed: ret={ret} error={}", last_error());
        }
        if !presenting() {
            PRESENTED_SEQ.fetch_add(1, Ordering::SeqCst);
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
        let ([g, b, r], white, max_dml, min_dml, cll, fall) = (
            m.display_primaries,
            m.white_point,
            m.max_display_mastering_luminance,
            m.min_display_mastering_luminance,
            m.max_cll,
            m.max_fall,
        );
        let info = NdlHdrInfo {
            display_primaries_x0: c_int::from(g[0]),
            display_primaries_y0: c_int::from(g[1]),
            display_primaries_x1: c_int::from(b[0]),
            display_primaries_y1: c_int::from(b[1]),
            display_primaries_x2: c_int::from(r[0]),
            display_primaries_y2: c_int::from(r[1]),
            white_point_x: c_int::from(white[0]),
            white_point_y: c_int::from(white[1]),
            max_display_mastering_luminance: max_dml as c_int,
            min_display_mastering_luminance: min_dml as c_int,
            max_content_light_level: c_int::from(cll),
            max_pic_average_light_level: c_int::from(fall),
            transfer_characteristics: c_int::from(color.transfer),
            color_primaries: c_int::from(color.primaries),
            matrix_coeffs: c_int::from(color.matrix),
            reserved: [0; 32],
        };
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: passed by value; no pointers or aliasing.
        let ret = unsafe { NDL_DirectVideoSetHDRInfo(info) };
        if ret != 0 {
            bail!("NDL_DirectVideoSetHDRInfo failed: ret={ret} error={}", last_error());
        }
        Ok(())
    }

    /// Buffered-but-undisplayed frames in NDL (None if query fails).
    /// Rising length = decoder behind; flat near-zero with stutter = upstream problem.
    pub fn render_buffer_length(&self) -> Option<i32> {
        let mut length: c_int = 0;
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: `length` is a valid, writable `c_int` for the duration of the call.
        let ret = unsafe { NDL_DirectVideoGetRenderBufferLength(&mut length) };
        (ret == 0).then_some(length)
    }

    pub fn flush(&self) -> Result<()> {
        let _ffi = self.ffi.lock().expect("NDL FFI mutex poisoned");
        // SAFETY: no arguments.
        let ret = unsafe { NDL_DirectVideoFlushRenderBuffer() };
        if ret != 0 {
            bail!(
                "NDL_DirectVideoFlushRenderBuffer failed: ret={ret} error={}",
                last_error()
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
        let _ = unsafe { NDL_DirectMediaUnload() };
    }
}

/// Process-wide NDL teardown — call once at exit, after every `NdlVideo` has dropped.
pub fn quit() {
    if INIT_DONE.swap(false, Ordering::SeqCst) {
        // SAFETY: no arguments.
        unsafe {
            NDL_DirectMediaQuit();
        }
    }
}
