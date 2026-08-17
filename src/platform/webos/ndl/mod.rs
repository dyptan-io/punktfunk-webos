//! webOS NDL `DirectMedia` video. Video only; audio goes through SDL2
//! (`platform::webos::audio`).
//!
//! Two generations of the same C API, in the same device library, chosen by
//! `device::ndl_generation()` from the detected `sdkVersion`:
//!
//! - [`v2`] — webOS 5+. `NDL_DirectMediaLoad` + `NDL_DirectVideoPlay(buffer, size, pts)`, with
//!   a render-buffer query, a flush, and HDR mastering metadata. The path every currently
//!   working TV takes.
//! - [`v1`] — webOS 3.5-4.x. `NDL_DirectVideoOpen/SetCallback/SetArea/PlayWithCallback/Close`:
//!   H.264 only, SDR, and **no PTS input**.
//!
//! Neither generation has a decode-context handle — every call is a process singleton — so what
//! both share lives here: the load-state counters behind [`presenting`]/[`playing`], the
//! one-time `NDL_DirectMediaInit`/[`quit`] pair, and the [`poison`] gate.
//!
//! **Everything is `dlopen`'d, and must stay that way.** `libNDL_directmedia.so.1` exists on
//! every supported TV but its *symbol set* does not — webOS 3.5-4.x has none of the v2 entry
//! points. This binary links with `DT_BIND_NOW`/`DF_1_NOW`, so a `DT_NEEDED` reference to that
//! library makes the dynamic loader resolve `NDL_DirectMediaLoad` at exec time and **refuse to
//! start the process at all** on webOS 4 — before `main()`, with nothing logged. A
//! `#[link(name = "NDL_directmedia")]` block is therefore a launch-time regression on every
//! webOS 4 device, not a style choice; see `docs/NOTES.md`.
//!
//! `device::ndl_generation` decides which generation to *try*; [`ffi`]'s `dlsym` probe decides
//! whether it's there. A miss is a named error, never a fallback to the other generation — on
//! webOS 4 the v2 symbols are absent by construction, so falling back buys a doomed connect.
mod ffi;
pub mod v1;
mod v2;

use std::ffi::{c_char, c_int, c_longlong, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

pub use v2::{NdlAudioConfig, NdlVideo, NotLoadedYet};

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

/// NDL load-state values reported through the v2 media-load callback.
const STATE_LOADCOMPLETED: c_int = 0x16;
const STATE_UNLOADCOMPLETED: c_int = 0x17;
const STATE_PLAYING: c_int = 0x1a;

/// Bound, not a requirement: feeding an unloaded decoder is the first-frames-black cause,
/// but a model that never delivers the callback must still stream.
const LOAD_COMPLETE_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Grace for a rejected load's callbacks to land before the video-only retry arms. The callback
/// carries nothing identifying its load, so separating the two in TIME is the only way to stop a
/// stale `LOADCOMPLETED` satisfying the retry's wait — and feeding an unloaded decoder is what
/// turns a launch black.
const CALLBACK_SETTLE: Duration = Duration::from_millis(400);

/// Poll interval for the two waits below (startup path only).
const POLL: Duration = Duration::from_millis(2);

/// A process-global NDL event, counted rather than flagged: a late event still increments, so it
/// stays attributable to the load it came from — a sticky bool cannot tell "this load completed"
/// from "the previous one's callback arrived a moment too late". [`Self::arm`] stamps the count a
/// new load starts from; [`Self::fired`] then answers only about that load.
struct EventSeq {
    seq: AtomicU64,
    base: AtomicU64,
}

impl EventSeq {
    const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            base: AtomicU64::new(0),
        }
    }

    fn bump(&self) {
        self.seq.fetch_add(1, Ordering::SeqCst);
    }

    /// Bump only if this load hasn't seen the event yet; `true` when this call was the first.
    fn bump_first(&self) -> bool {
        if self.fired() {
            return false;
        }
        self.bump();
        true
    }

    fn arm(&self) {
        self.base.store(self.seq.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    fn fired(&self) -> bool {
        self.seq.load(Ordering::SeqCst) > self.base.load(Ordering::SeqCst)
    }

    fn count(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }
}

static LOAD_COMPLETED: EventSeq = EventSeq::new();
static UNLOAD_COMPLETED: EventSeq = EventSeq::new();
/// NDL's own present-pipeline signal (docs/NDL-FRAMERATE-INVESTIGATION.md). Measured on a G5:
/// it lands during `load()`, BEFORE any frame is fed, so it says nothing about there being a
/// picture — kept for the log line only, never as a reveal gate (see [`FRAME_FED`]).
static PLAYING: EventSeq = EventSeq::new();
/// Bumped by the first feed NDL accepts for the armed load. This, not `PLAYING`, is what makes
/// uncovering the punch-through plane safe: before it there is provably nothing on the plane,
/// and a host that delivers its first frame seconds late (new-flow stall, startup capacity
/// probe) would otherwise show as seconds of black.
static FRAME_FED: EventSeq = EventSeq::new();

/// Records v2 load-state transitions so loads, feeds and the UI reveal can wait on them.
extern "C" fn on_load_state(state: c_int, num: c_longlong, detail: *const c_char) {
    let name = match state {
        STATE_LOADCOMPLETED => {
            LOAD_COMPLETED.bump();
            "LOADCOMPLETED"
        }
        STATE_UNLOADCOMPLETED => {
            UNLOAD_COMPLETED.bump();
            "UNLOADCOMPLETED"
        }
        STATE_PLAYING => {
            PLAYING.bump();
            "PLAYING"
        }
        // Never swallow one silently: an error state arriving here is the only signal a load
        // rejected asynchronously, and NDL reports nothing else about it.
        _ => {
            // SAFETY: NDL passes a NUL-terminated string or null, valid for this call only.
            let detail = if detail.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(detail) }.to_string_lossy().into_owned()
            };
            tracing::warn!("NDL load state: unknown 0x{state:x} ({state}) num={num} {detail}");
            return;
        }
    };
    tracing::info!("NDL load state: {name} (0x{state:x})");
}

/// Takes the counters as the baseline for the load about to be issued. Call immediately before
/// every load, and after an unload so [`playing`] stops reporting a dead one.
fn arm_load() {
    LOAD_COMPLETED.arm();
    PLAYING.arm();
    FRAME_FED.arm();
}

/// `PLAYING` for the current load. Diagnostics only — see [`PLAYING`].
pub fn playing() -> bool {
    PLAYING.fired()
}

/// Whether a frame of the current load has reached the decoder: the plane has a picture and is
/// safe to uncover. Both NDL generations report through this, and so does SMP (see
/// [`arm_frame_gate`]), so `runtime`'s reveal gate is backend-blind.
pub fn presenting() -> bool {
    FRAME_FED.fired()
}

/// The reveal gate for a backend outside this module (`platform::webos::smp`). It lives here
/// because the gate is process-global, like the video plane it guards; a backend that doesn't
/// arm and bump it leaves `runtime`'s reveal waiting out its full timeout on every session.
/// Arm before opening the session and again on teardown, and bump on the first accepted frame.
pub fn arm_frame_gate() {
    FRAME_FED.arm();
}

/// Records the first frame of the armed session (see [`arm_frame_gate`]); `true` if it was the
/// first.
pub fn mark_frame_fed() -> bool {
    FRAME_FED.bump_first()
}

/// [`mark_frame_fed`] plus the log line both generations emit; `since` is the handle's load
/// instant. Returns whether this was the armed load's first frame.
fn mark_frame_fed_logged(backend: &str, since: Instant) -> bool {
    let first = mark_frame_fed();
    if first {
        tracing::info!("{backend} first frame fed {:?} after load", since.elapsed());
    }
    first
}

/// Every NDL entry point is a process singleton and none is documented as thread-safe, so one lock
/// serializes all of them. Poison-tolerant: a panic mid-FFI leaves no Rust state to corrupt, and
/// refusing the lock afterwards would only turn it into a dead video plane.
static FFI_LOCK: Mutex<()> = Mutex::new(());

fn lock_ffi() -> MutexGuard<'static, ()> {
    FFI_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sleeps in [`POLL`] steps until `done` or `limit` elapses; `true` if `done` won. Every wait in
/// this module is a callback arriving on NDL's own thread with nothing to block on.
fn poll_until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

/// `UNLOADCOMPLETED` count, for [`settle_before_retry`]'s baseline.
fn unload_count() -> u64 {
    UNLOAD_COMPLETED.count()
}

/// Lets the rejected audio-enabled load's callbacks land before the video-only retry is armed:
/// waits for its `UNLOADCOMPLETED`, then a fixed settle for anything still in flight behind it.
/// `unloads_before` is [`unload_count`] from before the rejected load was even attempted — the
/// caller's own teardown may have unloaded already, and this must not wait out a spent callback.
fn settle_before_retry(unloads_before: u64) {
    poll_until(CALLBACK_SETTLE, || UNLOAD_COMPLETED.count() != unloads_before);
    std::thread::sleep(CALLBACK_SETTLE);
}

/// Blocks until the armed load's `LOADCOMPLETED` lands. Returns `false` on timeout.
fn wait_load_completed() -> bool {
    let completed = poll_until(LOAD_COMPLETE_TIMEOUT, || LOAD_COMPLETED.fired());
    if !completed {
        tracing::warn!("NDL load: no LOADCOMPLETED within {LOAD_COMPLETE_TIMEOUT:?} — holding the first frames");
    }
    completed
}

/// Count of video/audio pump threads leaked past `SHUTDOWN_JOIN_TIMEOUT` (see
/// `session::join_with_timeout`), not yet confirmed exited. A leaked thread may still be
/// inside an `NDL_Direct*` call and still holds a live decode session with its own
/// unsynchronized `ffi` mutex — a second load on top of that races it instead of starting
/// clean, reproducing as an undecodable stream rather than a clean failure.
///
/// No way to force this: an OS thread can't be safely cancelled mid-FFI-call, and racing the
/// unload against it is the exact hazard this guards against. So every `load()` refuses while
/// nonzero, and dropping the [`LeakGuard`] clears it in-process (no restart) once the leaked
/// thread actually returns — its handle's `Drop` has run the real unload by then.
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
/// wording: every `load()` enforces it and `session::connect` checks it early to avoid holding a
/// host session slot for a connect that can only end here.
pub fn ensure_not_poisoned() -> Result<()> {
    if LEAKED_THREADS.load(Ordering::SeqCst) > 0 {
        bail!("NDL is still tearing down a wedged decode thread from the previous session — try reconnecting shortly");
    }
    Ok(())
}

static INIT_DONE: AtomicBool = AtomicBool::new(false);

/// Calls `NDL_DirectMediaInit` once (process-global, idempotent-guarded). One symbol, two
/// prototypes: `api2` picks the app-id-only form v2 declares, against v1's app id plus
/// resource-released callback (always NULL here). See [`ffi::Common`].
fn ensure_init(app_id: &str, api2: bool) -> Result<()> {
    if INIT_DONE.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let fns = ffi::common()?;
    let c_app_id = CString::new(app_id).unwrap_or_default();
    // SAFETY: `c_app_id` is valid for the duration of this call.
    let ret = unsafe {
        if api2 {
            (fns.init_v2)(c_app_id.as_ptr())
        } else {
            (fns.init_v1)(c_app_id.as_ptr(), None)
        }
    };
    if ret != 0 {
        INIT_DONE.store(false, Ordering::SeqCst);
        bail!("NDL_DirectMediaInit failed: ret={ret} error={}", ffi::last_error());
    }
    Ok(())
}

/// The app id NDL is initialised with. Overridable for dev builds installed under another id —
/// NDL keys its session on the caller's app id, so a mismatch fails the load.
pub fn app_id() -> String {
    std::env::var("APPID").unwrap_or_else(|_| "io.dyptan.punktfunk.webos".into())
}

/// Process-wide NDL teardown — call once at exit, after every decode session has dropped.
pub fn quit() {
    if !INIT_DONE.swap(false, Ordering::SeqCst) {
        return;
    }
    // Same symbol on both generations, so no branch needed — and the table must have resolved
    // for `INIT_DONE` to have been set.
    if let Ok(fns) = ffi::common() {
        // SAFETY: no arguments.
        unsafe {
            (fns.quit)();
        }
    }
}
