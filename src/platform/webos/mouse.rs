//! Maps SDL2 mouse events (Magic Remote pointer mode) to punktfunk wire `InputEvents`.
//! Allows pointing/clicking remote to drive host cursor during stream.
use std::time::Instant;

use punktfunk_core::input::{InputEvent, InputKind};
use sdl2::mouse::MouseButton;

use crate::core::event::LONG_PRESS;

/// `GameStream`'s classic mouse-button numbering (1=left..5=X2) — the convention
/// `punktfunk-host`'s injectors expect in `MouseButtonDown`/`MouseButtonUp`'s `code`
/// (confirmed via `gs_button_to_evdev` in `punktfunk-host/src/inject.rs`).
fn button_code(button: MouseButton) -> Option<u32> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Middle => Some(2),
        MouseButton::Right => Some(3),
        MouseButton::X1 => Some(4),
        MouseButton::X2 => Some(5),
        MouseButton::Unknown => None,
    }
}

/// Whether this is a mouse event synthesized from a touch device (`which` is
/// `SDL_TOUCH_MOUSEID`) rather than one a real pointer reported. Dropped at the head of both of
/// `runtime`'s event loops, so nothing downstream has to know the difference.
///
/// A `DualSense` publishes its touchpad as its own absolute/multitouch evdev node, which the
/// compositor picks up as a touch device; the emulation then holds the left button down for as
/// long as a finger is on the pad, so a thumb resting there drags whatever the host cursor is
/// over. `SDL_TOUCH_MOUSE_EVENTS=0` (set in `runtime::stream`) turns SDL's own half off, and
/// [`super::evdev`] claims the node so the compositor can't drive the TV cursor from it either;
/// this is the last of the three, covering anything synthesized before SDL sees it.
///
/// `SDL_TOUCH_MOUSEID` is `(Uint32)-1`; rust-sdl2 doesn't re-export it.
pub fn is_touch_emulated(event: &sdl2::event::Event) -> bool {
    use sdl2::event::Event;
    let (Event::MouseMotion { which, .. }
    | Event::MouseButtonDown { which, .. }
    | Event::MouseButtonUp { which, .. }
    | Event::MouseWheel { which, .. }) = *event
    else {
        return false;
    };
    which == u32::MAX
}

/// `None` for a button id the host has no mapping for (`MouseButton::Unknown`) —
/// the caller just drops the event.
pub fn button_event(button: MouseButton, pressed: bool) -> Option<InputEvent> {
    Some(raw_button_event(button_code(button)?, pressed))
}

/// Same wire event from an already-mapped button number — for [`super::evdev`], whose
/// buttons come off evdev and never pass through an `sdl2::mouse::MouseButton`.
pub fn raw_button_event(code: u32, pressed: bool) -> InputEvent {
    InputEvent {
        kind: if pressed {
            InputKind::MouseButtonDown
        } else {
            InputKind::MouseButtonUp
        },
        _pad: [0; 3],
        code,
        x: 0,
        y: 0,
        flags: 0,
    }
}

/// Wire button numbers, in `button_code`'s numbering.
const LEFT: u32 = 1;
const RIGHT: u32 = 3;

/// Every mouse button this client *synthesizes* rather than forwards, and the only place their
/// held state lives. The Magic Remote has one pointer button (OK, delivered as SDL `Left`) plus
/// a Red key, and no wheel-tilt — so left, right and drag all have to come out of those two.
///
/// Red is the simple half: [`Self::red`] mirrors it onto the right button, press for press, no
/// timing at all. OK is [`OkPress`], which resolves one press three ways.
///
/// Holding both in one struct is what makes [`Self::release_held`] possible — a single call that
/// leaves the host holding nothing, for the two cases where releases stop arriving (the
/// disconnect dialog swallows input, and the stream can end mid-press).
#[derive(Default)]
pub struct RemoteButtons {
    /// The OK press in flight, `None` when OK is up.
    ok: Option<OkPress>,
    /// Whether Red has put the right button down on the host.
    red_down: bool,
}

/// An OK press being resolved into left click, right click, or a left-button drag:
///
/// * released under [`LONG_PRESS`] → left down+up, the unchanged common case (a tap's own
///   latency, nothing added by the wait)
/// * released between [`LONG_PRESS`] and [`DRAG_HOLD`] having stayed put → right down+up,
///   and no left at all
/// * still held at [`DRAG_HOLD`], or moved past [`DRAG_SLOP`] before then → left goes down
///   and stays down, so the press is a drag
///
/// Two ways into a drag because neither alone is enough on this device. The time branch has
/// to exist since motion events don't reliably arrive while the remote's button is down —
/// without it a deliberate long hold has no way to express "keep the button down" and every
/// hold collapses into a right click. The motion branch exists because waiting out
/// [`DRAG_HOLD`] before every drag is tedious once the pointer is clearly travelling.
///
/// Both thresholds are deliberately coarse. Aiming a remote at arm's length and pressing its
/// button jitters the pointer, so `DRAG_SLOP` is measured against *net* displacement from the
/// press point, not path length — back-and-forth wobble cancels instead of accumulating
/// toward a drag the user didn't ask for. `DRAG_HOLD` sits well clear of `LONG_PRESS` so the
/// right-click window is something a person can actually hit.
///
/// Only ever fed the Magic Remote's SDL button. A real USB mouse (see [`super::evdev`])
/// has its own right button and its clicks stay a straight pass-through — nothing here
/// applies to it.
struct OkPress {
    since: Instant,
    /// Pointer position at the press, for the absolute path's net displacement.
    origin: (i32, i32),
    /// Net displacement since the press.
    net: (i32, i32),
    /// Left is down on the host, i.e. this press became a drag.
    dragging: bool,
}

/// Net px the pointer may drift during a press before it counts as a drag. Wide enough to
/// absorb the wobble of holding a remote at arm's length, but no wider: crossing it resolves
/// the press *immediately* — left goes down at once and the [`DRAG_HOLD`] wait is abandoned
/// — so a pointer that is plainly travelling shouldn't have to keep travelling to be believed.
const DRAG_SLOP: i32 = 24;

/// How long OK must stay down before the press commits to a held left button. Past this the
/// right-click window has closed — see [`OkPress`].
const DRAG_HOLD: std::time::Duration = std::time::Duration::from_millis(1000);

impl OkPress {
    /// Whether the pointer has now travelled far enough to not be press jitter. Once it has,
    /// nothing waits for [`DRAG_HOLD`] and the right-click branch is off the table for the rest
    /// of this press.
    fn drifted(&self) -> bool {
        self.net.0.abs().max(self.net.1.abs()) >= DRAG_SLOP
    }
}

impl RemoteButtons {
    /// OK down at `(x, y)`. Nothing is sent yet — which button this is isn't known until the
    /// release, real motion, or [`DRAG_HOLD`], whichever lands first.
    pub fn ok_press(&mut self, x: i32, y: i32) {
        self.ok = Some(OkPress {
            since: Instant::now(),
            origin: (x, y),
            net: (0, 0),
            dragging: false,
        });
    }

    /// Absolute pointer motion while OK may be down (the uncaptured remote).
    pub fn motion_abs(&mut self, x: i32, y: i32, send: impl FnOnce(&InputEvent)) {
        if let Some(ok) = self.ok.as_mut() {
            ok.net = (x - ok.origin.0, y - ok.origin.1);
        }
        self.start_drag_if(OkPress::drifted, send);
    }

    /// Relative motion while OK may be down — under cursor capture, where SDL pins the absolute
    /// position and only the deltas mean anything.
    pub fn motion_rel(&mut self, dx: i32, dy: i32, send: impl FnOnce(&InputEvent)) {
        if let Some(ok) = self.ok.as_mut() {
            ok.net = (ok.net.0 + dx, ok.net.1 + dy);
        }
        self.start_drag_if(OkPress::drifted, send);
    }

    /// Commits to a drag once OK has been held past [`DRAG_HOLD`]. Polled by the caller each
    /// tick: a stationary hold produces no events of its own, so nothing else would notice.
    pub fn tick(&mut self, send: impl FnOnce(&InputEvent)) {
        self.start_drag_if(|ok| ok.since.elapsed() >= DRAG_HOLD, send);
    }

    /// Puts left down and keeps it there when `ready` says this press is a drag. Both routes in
    /// (drifted far enough, held long enough) are the same commitment, so they share one path.
    fn start_drag_if(&mut self, ready: impl FnOnce(&OkPress) -> bool, send: impl FnOnce(&InputEvent)) {
        let Some(ok) = self.ok.as_mut() else { return };
        if ok.dragging || !ready(ok) {
            return;
        }
        ok.dragging = true;
        send(&raw_button_event(LEFT, true));
    }

    /// OK up: emits whatever the press turned out to mean. No-op when no press is in flight (one
    /// already resolved via [`Self::release_held`]).
    pub fn ok_release(&mut self, mut send: impl FnMut(&InputEvent)) {
        let Some(ok) = self.ok.take() else { return };
        if ok.dragging {
            send(&raw_button_event(LEFT, false));
            return;
        }
        let button = if ok.since.elapsed() >= LONG_PRESS { RIGHT } else { LEFT };
        send(&raw_button_event(button, true));
        send(&raw_button_event(button, false));
    }

    /// Red down/up, mirrored straight onto the right button — no hold, no timing, so it works
    /// whether or not the OK gestures are enabled. Held for as long as the key is, which is what
    /// a right-drag needs.
    pub fn red(&mut self, down: bool, send: impl FnOnce(&InputEvent)) {
        self.red_down = down;
        send(&raw_button_event(RIGHT, down));
    }

    /// Releases everything the host is currently holding, and forgets any press in flight.
    ///
    /// For the cases where the matching release never arrives: the disconnect dialog opens over
    /// the stream and swallows input from there on, and the stream itself can end mid-press.
    /// Idempotent — a second call with nothing held sends nothing.
    pub fn release_held(&mut self, mut send: impl FnMut(&InputEvent)) {
        if self.ok.take().is_some_and(|ok| ok.dragging) {
            send(&raw_button_event(LEFT, false));
        }
        if std::mem::take(&mut self.red_down) {
            send(&raw_button_event(RIGHT, false));
        }
    }
}

/// Relative motion, for a captured stream. Absolute coordinates can't leave the panel —
/// webOS's pointer stops at the screen edge, so `MouseMoveAbs` saturates there and the
/// host cursor can neither cross onto another display nor keep turning in a game that
/// wants continuous motion. Deltas have no such ceiling.
pub fn move_relative_event(dx: i32, dy: i32) -> InputEvent {
    InputEvent {
        kind: InputKind::MouseMove,
        _pad: [0; 3],
        code: 0,
        x: dx,
        y: dy,
        flags: 0,
    }
}

/// Absolute pointer position — `client_w`/`client_h` is this app's own coordinate
/// space (the physical panel resolution the SDL2 window/mouse coordinates are in,
/// not necessarily the negotiated stream resolution); the host normalizes against
/// it before mapping into the output region (see `InputKind::MouseMoveAbs` docs) —
/// the same absolute-pointer path the pre-stream menu's hover/click already rides,
/// just forwarded to the host instead of used for local UI focus.
pub fn move_event(x: i32, y: i32, client_w: u32, client_h: u32) -> InputEvent {
    InputEvent {
        kind: InputKind::MouseMoveAbs,
        _pad: [0; 3],
        code: 0,
        x,
        y,
        flags: (client_w << 16) | (client_h & 0xffff),
    }
}

/// Rescales SDL2's ~±1-per-notch wheel delta to the wire's `GameStream`
/// `WHEEL_DELTA(120)`-per-notch convention (confirmed via `punktfunk-host`'s
/// `pf-inject` `sendinput.rs`/`wlr.rs`), carrying the fractional remainder across
/// calls so a run of small deltas doesn't round away to nothing.
#[derive(Default)]
pub struct ScrollAccumulator {
    x: f64,
    y: f64,
}

impl ScrollAccumulator {
    /// `code` distinguishes the scroll axis (`0` = vertical, `1` = horizontal).
    /// `None` while the accumulated remainder hasn't reached a whole wire unit.
    pub fn scroll_event(&mut self, delta: i32, horizontal: bool) -> Option<InputEvent> {
        let acc = if horizontal { &mut self.x } else { &mut self.y };
        *acc += f64::from(delta) * 120.0;
        let notches = acc.trunc() as i32;
        if notches == 0 {
            return None;
        }
        *acc -= f64::from(notches);
        Some(InputEvent {
            kind: InputKind::MouseScroll,
            _pad: [0; 3],
            code: u32::from(horizontal),
            x: notches,
            y: 0,
            flags: 0,
        })
    }
}
