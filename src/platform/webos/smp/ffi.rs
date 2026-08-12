//! `dlopen`/`dlsym` table for `libplayerAPIs_C.so` (the C wrapper `c_shim.cpp` builds around the
//! device's C++-only `libplayerAPIs.so`). The SDL exported-window entry points punch-through
//! needs live in `platform::webos::sdl_webos`, resolved the same way.
//!
//! Loaded, never linked — same rule as `ndl::ffi` and for the same reason (`docs/NOTES.md`): the
//! wrapper may be missing from a hand-built package, and a `DT_NEEDED` on it would then stop the
//! process before `main()`.
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

use anyhow::Result;

use crate::platform::webos::dl;

const LIB_NAME: &CStr = c"libplayerAPIs_C.so";

/// SMP load-event ids this backend acts on (`StarfishMediaAPIs_C.h`).
pub const EVENT_STR_VIDEO_INFO: c_int = 0x4;
pub const EVENT_LOADCOMPLETED: c_int = 0x16;

pub type LoadCb = unsafe extern "C" fn(c_int, i64, *const c_char, *mut c_void);

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
    static FNS: OnceLock<std::result::Result<Fns, String>> = OnceLock::new();
    dl::cached(&FNS, LIB_NAME, |lib| {
        Ok(Fns {
            create: lib.sym(c"StarfishMediaAPIs_create")?,
            media_id: lib.sym(c"StarfishMediaAPIs_getMediaID")?,
            load: lib.sym(c"StarfishMediaAPIs_load")?,
            feed: lib.sym(c"StarfishMediaAPIs_feed")?,
            play: lib.sym(c"StarfishMediaAPIs_play")?,
            push_eos: lib.sym(c"StarfishMediaAPIs_pushEOS")?,
            unload: lib.sym(c"StarfishMediaAPIs_unload")?,
            destroy: lib.sym(c"StarfishMediaAPIs_destroy")?,
            notify_fg: lib.sym(c"StarfishMediaAPIs_notifyForeground")?,
            set_hdr_info: lib.sym(c"StarfishMediaAPIs_setHdrInfo")?,
        })
    })
}
