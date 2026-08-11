//! `dlopen`/`dlsym` table for `libplayerAPIs_C.so` (the C wrapper `c_shim.cpp` builds around the
//! device's C++-only `libplayerAPIs.so`), plus the SDL exported-window entry points SMP
//! punch-through needs.
//!
//! Loaded, never linked — same rule as `ndl::ffi` and for the same reason (`docs/NOTES.md`): the
//! wrapper may be missing from a hand-built package, and a `DT_NEEDED` on it would then stop the
//! process before `main()`.
use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

use anyhow::{bail, Result};

/// SMP load-event ids this backend acts on (`StarfishMediaAPIs_C.h`).
pub const EVENT_STR_VIDEO_INFO: c_int = 0x4;
pub const EVENT_LOADCOMPLETED: c_int = 0x16;

pub type LoadCb = unsafe extern "C" fn(c_int, i64, *const c_char, *mut c_void);

#[repr(C)]
pub struct SdlRect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
}

#[link(name = "SDL2")]
extern "C" {
    pub fn SDL_webOSCreateExportedWindow(hint: c_int) -> *const c_char;
    pub fn SDL_webOSSetExportedWindow(window_id: *const c_char, src: *const SdlRect, dst: *const SdlRect) -> c_int;
    pub fn SDL_webOSDestroyExportedWindow(window_id: *const c_char);
}

/// The `StarfishMediaAPIs_C` functions this backend calls (see `c_shim.cpp`).
pub struct Fns {
    pub create: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pub media_id: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    pub load: unsafe extern "C" fn(*mut c_void, *const c_char, Option<LoadCb>, *mut c_void) -> bool,
    pub feed: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_char, usize) -> bool,
    pub play: unsafe extern "C" fn(*mut c_void) -> bool,
    pub push_eos: unsafe extern "C" fn(*mut c_void) -> bool,
    pub unload: unsafe extern "C" fn(*mut c_void) -> bool,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub notify_fg: unsafe extern "C" fn(*mut c_void) -> bool,
    pub set_hdr_info: unsafe extern "C" fn(*mut c_void, *const c_char) -> bool,
}

// SAFETY: plain function pointers into a library kept loaded for the process lifetime.
unsafe impl Send for Fns {}
unsafe impl Sync for Fns {}

/// Resolved once; a miss is a named error the caller turns into "fall back to NDL".
pub fn fns() -> Result<&'static Fns> {
    static FNS: OnceLock<Option<Fns>> = OnceLock::new();
    FNS.get_or_init(resolve)
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SMP unavailable: libplayerAPIs_C.so did not load — see the log"))
}

fn resolve() -> Option<Fns> {
    match resolve_inner() {
        Ok(fns) => Some(fns),
        Err(e) => {
            tracing::warn!("SMP: {e:#}");
            None
        }
    }
}

fn resolve_inner() -> Result<Fns> {
    // SAFETY: a NUL-terminated literal; RTLD_GLOBAL matches how ss4s loads it.
    let lib = unsafe { libc::dlopen(c"libplayerAPIs_C.so".as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
    if lib.is_null() {
        bail!("dlopen(libplayerAPIs_C.so) failed — not packaged, or SMP absent on this TV");
    }

    macro_rules! need {
        ($sym:literal) => {{
            // SAFETY: dlsym-verified non-null pointer, transmuted to the field's fn-pointer type.
            let ptr = unsafe { libc::dlsym(lib, concat!($sym, "\0").as_ptr() as *const c_char) };
            if ptr.is_null() {
                bail!(concat!("libplayerAPIs_C.so missing symbol: ", $sym));
            }
            unsafe { std::mem::transmute_copy(&ptr) }
        }};
    }

    Ok(Fns {
        create: need!("StarfishMediaAPIs_create"),
        media_id: need!("StarfishMediaAPIs_getMediaID"),
        load: need!("StarfishMediaAPIs_load"),
        feed: need!("StarfishMediaAPIs_feed"),
        play: need!("StarfishMediaAPIs_play"),
        push_eos: need!("StarfishMediaAPIs_pushEOS"),
        unload: need!("StarfishMediaAPIs_unload"),
        destroy: need!("StarfishMediaAPIs_destroy"),
        notify_fg: need!("StarfishMediaAPIs_notifyForeground"),
        set_hdr_info: need!("StarfishMediaAPIs_setHdrInfo"),
    })
}
