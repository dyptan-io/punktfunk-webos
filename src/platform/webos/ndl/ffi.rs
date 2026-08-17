//! The raw NDL surface: C structs, function-pointer tables, and the one `dlopen` behind them.
//! No policy — see [`super`] for why this is `dlopen`'d rather than linked.
use std::ffi::{c_char, c_int, c_longlong, c_uint, c_ulonglong, c_void, CStr};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

const LIB_NAME: &CStr = c"libNDL_directmedia.so.1";

// --- C structs -------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct VideoInfo {
    pub(super) width: c_int,
    pub(super) height: c_int,
    /// `NDL_VIDEO_TYPE`: 1=H264, 2=H265, 3=VP9.
    pub(super) kind: c_int,
    pub(super) unknown1: c_int,
}

/// NDL audio union (8-byte aligned). Tag 0 = no audio (all-zero).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(super) struct AudioUnion {
    pub(super) bytes: [u8; 32],
}

impl AudioUnion {
    /// The "no audio" arm: a video-only load.
    pub(super) const SILENT: Self = Self { bytes: [0; 32] };
}

/// `NDL_DIRECTMEDIA_AUDIO_OPUS_INFO_T` (field-for-field match).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(super) struct AudioOpusInfo {
    /// `NDL_AUDIO_TYPE`: 3 = Opus.
    pub(super) kind: c_int,
    pub(super) unknown1: c_int,
    pub(super) channels: c_int,
    pub(super) unknown2: c_int,
    /// kHz, not Hz.
    pub(super) sample_rate: f64,
    /// Stream header (undocumented, passed null).
    pub(super) stream_header: *const c_char,
}

impl AudioOpusInfo {
    /// Pack into the union [`DataInfo`] takes.
    pub(super) fn to_union(self) -> AudioUnion {
        let mut bytes = [0u8; 32];
        // SAFETY: `Self` is `repr(C)` and no larger than the union's 32-byte arm (the header's
        // own `char padding[32]`), so this copy stays in bounds. Any trailing bytes remain
        // zero, matching the C compiler's own padding.
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(&self).cast::<u8>(),
                bytes.as_mut_ptr(),
                size_of::<Self>().min(32),
            );
        }
        AudioUnion { bytes }
    }
}

#[repr(C)]
pub(super) struct DataInfo {
    pub(super) video: VideoInfo,
    pub(super) audio: AudioUnion,
}

/// Mirrors `NDL_DIRECTVIDEO_HDR_INFO_T` field-for-field — the field names are the
/// H.265 `mastering_display_colour_volume`/`content_light_level_info` SEI syntax
/// element names verbatim, so punktfunk's own `HdrMeta` (same SEI-derived fields,
/// same units) copies straight across with no unit conversion.
/// Fifteen `unsigned int` per `webosbrew/webos-userland@00a5f87`, which also dropped the
/// trailing `reserved[32]` an older header revision declared. The padding is kept anyway: this
/// struct is passed BY VALUE, so a set built against the older layout would read 32 bytes past
/// our argument copy. Carrying it is safe under both revisions; dropping it only under the new
/// one, and which revision a given TV's `libndl-directmedia` was built against isn't knowable
/// from here.
#[repr(C)]
pub(super) struct HdrInfo {
    pub(super) display_primaries_x0: c_uint,
    pub(super) display_primaries_y0: c_uint,
    pub(super) display_primaries_x1: c_uint,
    pub(super) display_primaries_y1: c_uint,
    pub(super) display_primaries_x2: c_uint,
    pub(super) display_primaries_y2: c_uint,
    pub(super) white_point_x: c_uint,
    pub(super) white_point_y: c_uint,
    pub(super) max_display_mastering_luminance: c_uint,
    pub(super) min_display_mastering_luminance: c_uint,
    pub(super) max_content_light_level: c_uint,
    pub(super) max_pic_average_light_level: c_uint,
    pub(super) transfer_characteristics: c_uint,
    pub(super) color_primaries: c_uint,
    pub(super) matrix_coeffs: c_uint,
    pub(super) reserved: [u8; 32],
}

/// `NDL_DIRECTVIDEO_DATA_INFO_T` (v1). `source` (`NDL_DIRECTVIDEO_SRC_TYPE`) is always `NONE`
/// (0) but must be declared: `NDL_DirectVideoOpen` reads all 12 bytes, so omitting it hands the
/// library four bytes of stack garbage.
#[repr(C)]
pub(super) struct V1VideoInfo {
    pub(super) width: c_int,
    pub(super) height: c_int,
    pub(super) source: c_int,
}

// --- Callback and function types -------------------------------------------------------------

/// `NDL_DirectMediaInit`'s resource-released callback. Always `None` here.
pub(super) type ResourceReleased = Option<extern "C" fn(*const c_char)>;
/// v2's load-state callback: `(state, num, str)`.
pub(super) type LoadStateCallback = Option<extern "C" fn(c_int, c_longlong, *const c_char)>;
/// v1's frame-done callback: the `userdata` its feed was given, echoed back.
pub(super) type FrameCallback = Option<extern "C" fn(c_ulonglong)>;

/// `NDL_DirectMediaInit`'s two prototypes: API 1 takes the resource-released callback, API 2 the
/// app id alone (`webosbrew/webos-userland@00a5f87`). One symbol, resolved once, typed twice.
pub(super) type InitV1 = unsafe extern "C" fn(*const c_char, ResourceReleased) -> c_int;
pub(super) type InitV2 = unsafe extern "C" fn(*const c_char) -> c_int;

/// The three calls both generations share.
pub(super) struct Common {
    pub(super) get_error: unsafe extern "C" fn() -> *const c_char,
    pub(super) init_v1: InitV1,
    pub(super) init_v2: InitV2,
    pub(super) quit: unsafe extern "C" fn() -> c_int,
}

/// webOS 5+ `DirectMedia` v2. Every symbol is required: a partial table means an unknown
/// flavour of the library, and guessing which calls are safe on it is worse than refusing.
pub(super) struct V2 {
    pub(super) load: unsafe extern "C" fn(*mut DataInfo, LoadStateCallback) -> c_int,
    pub(super) unload: unsafe extern "C" fn() -> c_int,
    pub(super) video_play: unsafe extern "C" fn(*mut c_void, c_uint, c_longlong) -> c_int,
    pub(super) flush_render_buffer: unsafe extern "C" fn() -> c_int,
    pub(super) get_render_buffer_length: unsafe extern "C" fn(*mut c_int) -> c_int,
    pub(super) audio_play: unsafe extern "C" fn(*mut c_void, c_uint, c_longlong) -> c_int,
    pub(super) set_hdr_info: unsafe extern "C" fn(HdrInfo) -> c_int,
}

/// webOS 3.5-4.x `DirectMedia` v1 — see [`super::v1`].
pub(super) struct V1 {
    pub(super) video_open: unsafe extern "C" fn(*mut V1VideoInfo) -> c_int,
    pub(super) video_close: unsafe extern "C" fn() -> c_int,
    pub(super) video_set_area: unsafe extern "C" fn(c_int, c_int, c_int, c_int) -> c_int,
    pub(super) video_set_callback: unsafe extern "C" fn(FrameCallback) -> c_int,
    pub(super) video_play_with_callback: unsafe extern "C" fn(*mut c_void, usize, c_ulonglong) -> c_int,
}

// --- Resolution ------------------------------------------------------------------------------

/// An open handle to [`LIB_NAME`]. Never closed — the resolved function pointers outlive it by
/// design, as a `DT_NEEDED` load would have.
struct Lib(*mut c_void);

impl Lib {
    /// `RTLD_GLOBAL` matches what a `DT_NEEDED` load would have given the process.
    fn open() -> Result<Self> {
        // SAFETY: `LIB_NAME` is a NUL-terminated literal.
        let handle = unsafe { libc::dlopen(LIB_NAME.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            bail!("dlopen({LIB_NAME:?}) failed — NDL DirectMedia is not available on this device");
        }
        Ok(Self(handle))
    }

    /// One symbol, or an error naming it — *which* symbol is missing is what says which
    /// generation this device has. `T` must be a function-pointer type.
    fn sym<T: Sized>(&self, name: &CStr) -> Result<T> {
        // SAFETY: `self.0` is a live `dlopen` handle and `name` NUL-terminated.
        let ptr = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        if ptr.is_null() {
            bail!("{LIB_NAME:?} is missing symbol {name:?}");
        }
        debug_assert_eq!(size_of::<T>(), size_of::<*mut c_void>(), "T must be a function pointer");
        // SAFETY: `T` is a function-pointer type and `ptr` is non-null and dlsym-verified.
        Ok(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

/// Resolve a table once, caching the outcome **including the failure**: a symbol missing from
/// this device's library won't appear on a retry. Text rather than `anyhow::Error` because
/// errors aren't `Clone` and callers only print it.
fn cached<T: 'static>(
    cache: &'static OnceLock<std::result::Result<T, String>>,
    build: impl FnOnce(&Lib) -> Result<T>,
) -> Result<&'static T> {
    cache
        .get_or_init(|| Lib::open().and_then(|lib| build(&lib)).map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::Error::msg(e.clone()))
        .context("NDL DirectMedia")
}

pub(super) fn common() -> Result<&'static Common> {
    static CACHE: OnceLock<std::result::Result<Common, String>> = OnceLock::new();
    cached(&CACHE, |lib| {
        let init_v1: InitV1 = lib.sym(c"NDL_DirectMediaInit")?;
        Ok(Common {
            get_error: lib.sym(c"NDL_DirectMediaGetError")?,
            init_v1,
            // SAFETY: the same address, retyped to the prototype API 2 declares for it.
            init_v2: unsafe { std::mem::transmute::<InitV1, InitV2>(init_v1) },
            quit: lib.sym(c"NDL_DirectMediaQuit")?,
        })
    })
}

pub(super) fn v2() -> Result<&'static V2> {
    static CACHE: OnceLock<std::result::Result<V2, String>> = OnceLock::new();
    cached(&CACHE, |lib| {
        Ok(V2 {
            load: lib.sym(c"NDL_DirectMediaLoad")?,
            unload: lib.sym(c"NDL_DirectMediaUnload")?,
            video_play: lib.sym(c"NDL_DirectVideoPlay")?,
            flush_render_buffer: lib.sym(c"NDL_DirectVideoFlushRenderBuffer")?,
            get_render_buffer_length: lib.sym(c"NDL_DirectVideoGetRenderBufferLength")?,
            audio_play: lib.sym(c"NDL_DirectAudioPlay")?,
            set_hdr_info: lib.sym(c"NDL_DirectVideoSetHDRInfo")?,
        })
    })
}

pub(super) fn v1() -> Result<&'static V1> {
    static CACHE: OnceLock<std::result::Result<V1, String>> = OnceLock::new();
    cached(&CACHE, |lib| {
        Ok(V1 {
            video_open: lib.sym(c"NDL_DirectVideoOpen")?,
            video_close: lib.sym(c"NDL_DirectVideoClose")?,
            video_set_area: lib.sym(c"NDL_DirectVideoSetArea")?,
            video_set_callback: lib.sym(c"NDL_DirectVideoSetCallback")?,
            video_play_with_callback: lib.sym(c"NDL_DirectVideoPlayWithCallback")?,
        })
    })
}

/// NDL's last error string (set on the most recent failing call). Messages only, so an
/// unresolved library answers with a placeholder rather than an error of its own.
pub(super) fn last_error() -> String {
    let Ok(fns) = common() else {
        return "(NDL not loaded)".to_string();
    };
    // SAFETY: returns a pointer to NDL's internal buffer; only borrowed here.
    let p = unsafe { (fns.get_error)() };
    if p.is_null() {
        return "(no NDL error message)".to_string();
    }
    // SAFETY: non-null NUL-terminated string owned by NDL; copied out before returning.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}
