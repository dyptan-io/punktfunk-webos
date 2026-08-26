//! The `webosbrew/SDL-webOS` fork's own entry points, resolved at runtime rather than linked.
//!
//! Same rule as `ndl::ffi` (`docs/NOTES.md`): only the bundled fork exports these, so linking
//! them would stop the process before `main()` under a stock SDL2. Resolved here, a miss is a
//! runtime error every caller degrades on.
use std::ffi::{c_int, CStr};
use std::sync::OnceLock;

use anyhow::Result;
use sdl2::sys::SDL_bool;

use super::dl;

const LIB_NAME: &CStr = c"libSDL2-2.0.so.0";

/// The fork-only functions this app calls. One table: they ship together, so they fail together.
pub struct Fns {
    /// `SDL_FALSE` self-gates on TVs without `wl_webos_input_manager`.
    pub cursor_visibility: unsafe extern "C" fn(SDL_bool) -> SDL_bool,
    /// Read-only panel-refresh query (`session::timeline::reconciled_frame_interval_ns`).
    pub get_refresh_rate: unsafe extern "C" fn(*mut c_int) -> c_int,
}

/// Resolved once; a miss is a named error the callers degrade on.
pub fn fns() -> Result<&'static Fns> {
    static FNS: OnceLock<std::result::Result<Fns, String>> = OnceLock::new();
    dl::cached(&FNS, LIB_NAME, |lib| {
        Ok(Fns {
            cursor_visibility: lib.sym(c"SDL_webOSCursorVisibility")?,
            get_refresh_rate: lib.sym(c"SDL_webOSGetRefreshRate")?,
        })
    })
}
