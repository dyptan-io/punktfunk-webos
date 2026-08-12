//! Where SMP's decoded video actually lands — and it is **not** the same mechanism across
//! releases. Two exist, picked by webOS major; this module is both, behind one enum:
//!
//! - [`Sink::Acb`] — webOS 3.5-4.x. The app registers an
//!   *appswitching-control-block* (`libAcbAPI.so`), binds SMP's media id to it, and drives
//!   `LOADED`/`PLAYING`/`UNLOADED` state plus the display window through it. There is no
//!   exported window on these releases, and no `windowId` in the load payload.
//! - [`Sink::Window`] — webOS 5+ (`smp_resource_webos5.c`). An SDL exported window whose id goes
//!   *into* the load payload, positioned with `SDL_webOSSetExportedWindow` after the load.
//!
//! Getting this wrong is not a degraded picture, it's no picture: the pipeline loads and feeds
//! happily with nothing composited. The generation split is [`Sink::create`]'s only job.
use std::ffi::{c_char, c_int, c_long, CStr, CString};
use std::sync::OnceLock;

use anyhow::{bail, Result};
use sdl2::sys::SDL_Rect;

use super::ffi;
use crate::platform::webos::device;
use crate::platform::webos::dl;

const LIB_NAME: &CStr = c"libAcbAPI.so";

/// `AcbAPI.h` enum values (from the webOS NDK sysroot).
///
/// `PLAYER_TYPE_MSE` is **10**, not its ordinal position: the enum opens with `GROUP = 0` and
/// `VIDEO = 0` sharing a value, so every later variant sits one below where counting the lines
/// would put it, so the value has to be hardcoded rather than counted.
const PLAYER_TYPE_MSE: c_int = 10;
const SINK_TYPE_MAIN: c_int = 0;
const APPSTATE_FOREGROUND: c_int = 1;
const PLAYSTATE_UNLOADED: c_int = 0;
const PLAYSTATE_LOADED: c_int = 1;
const PLAYSTATE_PLAYING: c_int = 2;

type AcbCallback = unsafe extern "C" fn(c_long, c_long, c_long, c_long, c_long, *const c_char);

/// The `libAcbAPI.so` functions the ACB sink calls. `dlopen`'d for the same reason as everything
/// else here: it is absent on some releases, and a `DT_NEEDED` would break launch there.
struct AcbFns {
    create: unsafe extern "C" fn() -> c_long,
    initialize: unsafe extern "C" fn(c_long, c_int, *const c_char, Option<AcbCallback>) -> bool,
    set_media_id: unsafe extern "C" fn(c_long, *const c_char) -> bool,
    set_sink_type: unsafe extern "C" fn(c_long, c_int) -> bool,
    set_state: unsafe extern "C" fn(c_long, c_int, c_int, *mut c_long) -> c_int,
    set_display_window: unsafe extern "C" fn(c_long, c_long, c_long, c_long, c_long, bool, *mut c_long) -> c_int,
    set_media_video_data: unsafe extern "C" fn(c_long, *const c_char) -> c_int,
    destroy: unsafe extern "C" fn(c_long),
}

unsafe impl Send for AcbFns {}
unsafe impl Sync for AcbFns {}

fn acb_fns() -> Result<&'static AcbFns> {
    static FNS: OnceLock<std::result::Result<AcbFns, String>> = OnceLock::new();
    dl::cached(&FNS, LIB_NAME, |lib| {
        Ok(AcbFns {
            create: lib.sym(c"AcbAPI_create")?,
            initialize: lib.sym(c"AcbAPI_initialize")?,
            set_media_id: lib.sym(c"AcbAPI_setMediaId")?,
            set_sink_type: lib.sym(c"AcbAPI_setSinkType")?,
            set_state: lib.sym(c"AcbAPI_setState")?,
            set_display_window: lib.sym(c"AcbAPI_setDisplayWindow")?,
            set_media_video_data: lib.sym(c"AcbAPI_setMediaVideoData")?,
            destroy: lib.sym(c"AcbAPI_destroy")?,
        })
    })
}

/// Logged only. The states this backend cares about are driven, not awaited.
unsafe extern "C" fn on_acb_event(
    acb_id: c_long,
    task_id: c_long,
    event: c_long,
    app_state: c_long,
    play_state: c_long,
    reply: *const c_char,
) {
    let reply = if reply.is_null() {
        String::new()
    } else {
        CStr::from_ptr(reply).to_string_lossy().into_owned()
    };
    tracing::info!(
        "ACB event: acb={acb_id} task={task_id} type={event} app={app_state} play={play_state} reply={reply}"
    );
}

pub enum Sink {
    /// webOS 3.5-4.x. `task` is scratch for the `taskId` out-params, which are logged by
    /// [`on_acb_event`] and otherwise unused.
    Acb { id: c_long, task: c_long },
    /// webOS 5+. The exported window's id, also sent in the load payload.
    Window(CString),
}

impl Sink {
    /// Build the sink this release actually composites through (see the module docs).
    pub fn create(app_id: &str) -> Result<Self> {
        if device::is_webos_pre5() {
            Self::create_acb(app_id)
        } else {
            // Unreachable in practice today: caps only offers SMP where the NDL baseline is narrow.
            Self::create_window()
        }
    }

    fn create_acb(app_id: &str) -> Result<Self> {
        let fns = acb_fns()?;
        let app_id = CString::new(app_id)?;
        // SAFETY: `app_id` outlives the call; `on_acb_event` is a plain `extern "C"` fn.
        let id = unsafe { (fns.create)() };
        if id == 0 {
            bail!("AcbAPI_create returned 0");
        }
        // SAFETY: `id` is a live ACB handle; destroyed in `Drop` on every path from here.
        if !unsafe { (fns.initialize)(id, PLAYER_TYPE_MSE, app_id.as_ptr(), Some(on_acb_event)) } {
            unsafe { (fns.destroy)(id) };
            bail!("AcbAPI_initialize failed");
        }
        Ok(Self::Acb { id, task: 0 })
    }

    fn create_window() -> Result<Self> {
        // SAFETY: SDL call with no arguments to keep alive.
        let raw = unsafe { ffi::SDL_webOSCreateExportedWindow(0) };
        if raw.is_null() {
            bail!("SDL_webOSCreateExportedWindow returned null");
        }
        // SAFETY: SDL returns a NUL-terminated id, valid until the window is destroyed.
        Ok(Self::Window(unsafe { CStr::from_ptr(raw) }.to_owned()))
    }

    /// The `windowId` the load payload must carry — empty on the ACB path, which has none.
    pub fn window_id(&self) -> &str {
        match self {
            Self::Window(id) => id.to_str().unwrap_or_default(),
            Self::Acb { .. } => "",
        }
    }

    /// Bind SMP's media id to the ACB, before the load.
    pub fn set_media_id(&self, media_id: &CStr) {
        let Self::Acb { id, .. } = self else {
            return;
        };
        // SAFETY: `media_id` outlives the call; `id` is a live handle.
        if !unsafe { (acb_fns().expect("ACB resolved at create").set_media_id)(*id, media_id.as_ptr()) } {
            tracing::warn!("AcbAPI_setMediaId({media_id:?}) failed");
        }
    }

    /// Place the video plane. Called once the load returns, with the stream's size — the ACB path
    /// asks for full-screen, the window path scales `src` into the app's own surface.
    pub fn post_load(&mut self, width: i32, height: i32) {
        match self {
            Self::Acb { id, task } => {
                let fns = acb_fns().expect("ACB resolved at create");
                // SAFETY: `task` is a valid out-param for the duration of the call.
                let ret = unsafe {
                    (fns.set_display_window)(*id, 0, 0, c_long::from(width), c_long::from(height), true, task)
                };
                tracing::info!("ACB display window {width}x{height} fullscreen: ret={ret}");
            }
            Self::Window(id) => {
                let src = SDL_Rect {
                    x: 0,
                    y: 0,
                    w: width,
                    h: height,
                };
                let dst = SDL_Rect {
                    x: 0,
                    y: 0,
                    w: SURFACE_W,
                    h: SURFACE_H,
                };
                // SAFETY: both rects and the id outlive the call.
                unsafe { ffi::SDL_webOSSetExportedWindow(id.as_ptr(), &src, &dst) };
            }
        }
    }

    /// On `LOADCOMPLETED`: claim the main sink and report the loaded state (ACB only).
    pub fn load_completed(&self, media_id: &CStr) {
        let Self::Acb { id, .. } = self else {
            return;
        };
        let fns = acb_fns().expect("ACB resolved at create");
        // SAFETY: live handle; the `task` out-param is optional and NULL is accepted.
        unsafe {
            (fns.set_media_id)(*id, media_id.as_ptr());
            (fns.set_sink_type)(*id, SINK_TYPE_MAIN);
            (fns.set_state)(*id, APPSTATE_FOREGROUND, PLAYSTATE_LOADED, std::ptr::null_mut());
        }
    }

    /// On the first frame SMP accepts — ACB won't show the plane until the app says playing.
    pub fn start_playing(&self) {
        self.set_play_state(PLAYSTATE_PLAYING);
    }

    /// SMP's `STARFISH_EVENT_STR_VIDEO_INFO` payload, forwarded to the ACB with `hdrType` added
    /// when this session is HDR.
    pub fn set_media_video_data(&self, info: &str, hdr: bool) {
        let Self::Acb { id, .. } = self else {
            return;
        };
        let payload = match serde_json::from_str::<serde_json::Value>(info) {
            Ok(mut v) if hdr => {
                if let Some(video) = v.get_mut("video").and_then(|v| v.as_object_mut()) {
                    video.entry("hdrType").or_insert_with(|| "HDR10".into());
                }
                v.to_string()
            }
            Ok(v) => v.to_string(),
            Err(e) => {
                tracing::warn!("ACB video info not JSON ({e}) — forwarding verbatim");
                info.to_string()
            }
        };
        let Ok(payload) = CString::new(payload) else {
            return;
        };
        // SAFETY: `payload` outlives the call; live handle.
        let ret = unsafe { (acb_fns().expect("ACB resolved at create").set_media_video_data)(*id, payload.as_ptr()) };
        tracing::info!("ACB media video data: ret={ret} payload={payload:?}");
    }

    fn set_play_state(&self, state: c_int) {
        let Self::Acb { id, .. } = self else {
            return;
        };
        // SAFETY: live handle, NULL task out-param.
        unsafe {
            (acb_fns().expect("ACB resolved at create").set_state)(
                *id,
                APPSTATE_FOREGROUND,
                state,
                std::ptr::null_mut(),
            );
        }
    }
}

/// The exported window's destination rect: webOS composites this app on a fixed 1920x1080
/// surface whatever the panel is (the same reason NDL v1 hardcodes its plane).
const SURFACE_W: c_int = 1920;
const SURFACE_H: c_int = 1080;

impl Drop for Sink {
    fn drop(&mut self) {
        match *self {
            Self::Acb { id, .. } => {
                self.set_play_state(PLAYSTATE_UNLOADED);
                if let Ok(fns) = acb_fns() {
                    // SAFETY: last use of `id`.
                    unsafe { (fns.destroy)(id) };
                }
            }
            // SAFETY: last use of the id; SDL owns nothing else here.
            Self::Window(ref id) => unsafe { ffi::SDL_webOSDestroyExportedWindow(id.as_ptr()) },
        }
    }
}
