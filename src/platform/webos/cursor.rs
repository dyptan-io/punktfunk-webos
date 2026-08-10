//! Two layers can draw the local pointer: SDL's own cursor (`show_cursor`, works) and the
//! compositor's (`SDL_webOSCursorVisibility`, global state, re-shown on activity — see
//! [`restore_on_exit`]).
//!
//! The compositor layer is normally kept quiet by `evmouse`'s `EVIOCGRAB` — starved of reports,
//! it stops drawing. But starving only stops *future* draws: an arrow already on screen when the
//! stream starts stays painted until something retracts it, which is why the hide is also
//! requested outright on each [`Cursor::apply`] and again once the grab is actually in place
//! ([`Cursor::reassert_hidden`]). The 4 Hz re-assert *loop* stays off, see
//! [`COMPOSITOR_REASSERT`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use sdl2::mouse::MouseUtil;
use sdl2::sys::SDL_bool;

extern "C" {
    /// SDL-webOS extension; `SDL_FALSE` self-gates on TVs without `wl_webos_input_manager`.
    fn SDL_webOSCursorVisibility(visible: SDL_bool) -> SDL_bool;
}

/// Polling re-assert is off: verified on webOS 26 the compositor re-shows its arrow regardless,
/// so the loop only spent Wayland requests. Kept, not deleted, in case a firmware/SDL-fork fix
/// makes flipping this worth it. The one-shot retracts are unconditional — a different question
/// from whether the hide *sticks* against later pointer activity.
const COMPOSITOR_REASSERT: bool = false;

/// Compositor gives no "took the pointer back" event, so re-hiding just polls on activity,
/// capped here at 4 Wayland requests/sec.
const REASSERT_INTERVAL: Duration = Duration::from_millis(250);

/// Global: shared with the panic hook, which has no [`Cursor`] to reach for.
static COMPOSITOR_HIDDEN: AtomicBool = AtomicBool::new(false);
static SUPPORT_LOGGED: AtomicBool = AtomicBool::new(false);
static OWNER_THREAD: OnceLock<ThreadId> = OnceLock::new();

/// The local pointer's visibility on every layer, plus capture state. Drive from the SDL video thread.
pub struct Cursor {
    mouse: MouseUtil,
    last_assert: Instant,
    captured: bool,
    sdl_relative: bool,
}

impl Cursor {
    pub fn new(mouse: MouseUtil) -> Self {
        Self {
            mouse,
            last_assert: Instant::now(),
            captured: false,
            sdl_relative: true,
        }
    }

    /// Stop asking SDL for relative mode, for when motion is read via `super::evmouse` instead:
    /// the fork emulates relative mode with a screen-centre warp per motion event, which is
    /// pure waste for a source we don't read. aurora-tv does the same under `hardware_mouse`.
    pub fn disable_sdl_relative(&mut self) {
        self.sdl_relative = false;
        self.apply();
    }

    /// Capture the pointer for the host — hidden on both layers, and SDL switched to
    /// relative mode so motion arrives as unbounded deltas instead of coordinates that
    /// stop at the panel edge. Uncaptured is the menu/desktop state: visible, absolute.
    pub fn set_captured(&mut self, captured: bool) {
        self.captured = captured;
        self.apply();
    }

    pub fn is_captured(&self) -> bool {
        self.captured
    }

    fn apply(&mut self) {
        let _ = OWNER_THREAD.set(std::thread::current().id());
        self.mouse.show_cursor(!self.captured);
        self.mouse.set_relative_mouse_mode(self.captured && self.sdl_relative);
        set_compositor_visible(!self.captured);
        COMPOSITOR_HIDDEN.store(self.captured, Ordering::Relaxed);
        self.last_assert = Instant::now();
    }

    /// Asks the compositor once more to drop its pointer. For the point where the evdev grab has
    /// actually landed — [`apply`](Self::apply) runs before `evmouse`'s background scan finds a
    /// node, so any motion in that window can repaint the arrow it just retracted. No-op while
    /// uncaptured.
    pub fn reassert_hidden(&mut self) {
        // The interval doubles as a debounce: callers pair this with a state change that already
        // ran `apply` (`disable_sdl_relative`), and repeating its request in the same tick would
        // be a pure duplicate.
        if !self.captured || self.last_assert.elapsed() < REASSERT_INTERVAL {
            return;
        }
        set_compositor_visible(false);
        self.last_assert = Instant::now();
    }

    /// Re-asserts the hide when due; no-op unless hidden and [`COMPOSITOR_REASSERT`] is on.
    pub fn on_pointer_activity(&mut self) {
        if !COMPOSITOR_REASSERT {
            return;
        }
        if !COMPOSITOR_HIDDEN.load(Ordering::Relaxed) {
            return;
        }
        if self.last_assert.elapsed() < REASSERT_INTERVAL {
            return;
        }
        self.last_assert = Instant::now();
        set_compositor_visible(false);
    }
}

fn set_compositor_visible(visible: bool) -> bool {
    // SAFETY: plain integer argument, no pointers; caller is the SDL video thread.
    let supported = unsafe { SDL_webOSCursorVisibility(bool_to_sdl(!visible)) } == SDL_bool::SDL_TRUE;
    // Logged once, for stray-cursor bug reports.
    if !SUPPORT_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::info!(
            "compositor cursor visibility control: {}",
            if supported { "available" } else { "unavailable" }
        );
    }
    supported
}

const fn bool_to_sdl(value: bool) -> SDL_bool {
    if value {
        SDL_bool::SDL_TRUE
    } else {
        SDL_bool::SDL_FALSE
    }
}

/// Put the compositor pointer back if a [`Cursor`] hid it — for exits that skip its teardown
/// (the panic hook); a graceful quit already calls [`Cursor::set_captured`]`(false)`. No-op
/// off the hiding thread, since a panicking thread has no business touching its Wayland connection.
pub fn restore_on_exit() {
    if !COMPOSITOR_HIDDEN.swap(false, Ordering::Relaxed) {
        return;
    }
    if OWNER_THREAD.get() != Some(&std::thread::current().id()) {
        tracing::warn!("cursor left hidden — panic is off the SDL thread, leaving it to client teardown");
        return;
    }
    set_compositor_visible(true);
}
