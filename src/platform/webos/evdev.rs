//! Raw HID mouse **and keyboard** input straight from `/dev/input/event*`, bypassing SDL.
//!
//! **Also a pad's touchpad.** A `DualSense`'s touchpad is a third evdev node the compositor would
//! otherwise turn into a cursor (see [`is_pad_touchpad`]); it's claimed here and decoded into
//! rich-input contacts for the host's virtual pad, the only route a game's touchpad swipes have.
//!
//! **Why.** SDL mouse events on webOS come from the compositor's pointer, smoothed/resampled
//! for a wrist-waved remote rather than a 125–1000 Hz mouse — jittery deltas no matter what the
//! client does with them. evdev is the same bypass aurora-tv ships as "Use Hardware Mouse".
//! Keyboards need the same exclusive grab, and in **both** cursor modes: an ungrabbed USB
//! keyboard still reaches surface-manager, which reads modifier+click as a system gesture and
//! warps its pointer to screen centre — the TV cursor and the host's then fight each other, which
//! is what made Ctrl/Alt/Shift unusable in a CAD app.
//!
//! **Two cursor modes.** Capture on (games): mouse nodes are grabbed too, SDL relative is off,
//! the compositor pointer is hidden. Capture off (desktop/absolute): a pointer-only node is left
//! with the compositor so the TV cursor stays the one you aim — only keyboards are taken.
//!
//! **Access.** Unlike `/dev/hidraw*` (jail-blocked, see `dualsense.rs`), evdev nodes are
//! reachable: `root:compositor 0660`, and the app's uid carries gid 505 in its supplementary
//! groups — verified on-device, non-rooted, webOS 10.3.
//!
//! **Grabbed while active.** `EVIOCGRAB` is scoped to [`HidInput::set_active`], not held for the
//! reader's whole life: `cursor::COMPOSITOR_CURSOR_CONTROL` is verified off on webOS 26 (see
//! `cursor.rs`), so an ungrabbed node leaves the compositor drawing its own pointer from the same
//! evdev reports we forward. Scoping it to "caller wants it" rather than the reader's whole life
//! bounds a wedged thread's blast radius to "no HID input" instead of "no mouse input at all,
//! TV-wide" — the kernel releases the grab the moment our fd closes (including on panic), and the
//! surface-manager's own fd stays open throughout, just starved of events while ours holds it.
//! The Magic Remote never matches the mouse/keyboard filter, so it stays usable via SDL —
//! which is what [`HidInput::keyboard_busy`] is for (drop HID keyboard echoes, keep remote keys).
//!
//! **One flag, two effects.** "Grabbed" and "forwarded to the host" are the same condition by
//! construction, not two atomics a caller has to keep in sync: [`HidInput::set_active`]`(false)`
//! both releases the node and stops calling `sink`, in one store — needed for the disconnect
//! dialog, which hands the pointer back to the Magic Remote and needs a HID mouse to go quiet for
//! the same window. A claimed pad touchpad is the one exception on both counts: it stays grabbed
//! (handing it back is what re-creates the cursor it exists to suppress) and its contacts still
//! reach the sink, since a dropped lift is a finger the host holds down forever.

use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use punktfunk_core::input::InputEvent;
use punktfunk_core::quic::RichInput;

use super::keyboard;
use super::mouse;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

/// End of one report — every `EV_ABS` since the last one describes the same instant, so touch
/// contacts are only sent here (see [`read_touch`]).
const SYN_REPORT: u16 = 0x00;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
/// The motion node's six axes, contiguous and in the `DualSense` report's own order: `ABS_X`..`_Z`
/// accelerometer, `ABS_RX`..`_RZ` gyro (pitch/yaw/roll).
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RZ: u16 = 0x05;
/// Multitouch protocol B: which contact the following `ABS_MT_*` values belong to.
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
/// `-1` lifts the slot's contact; anything else is a new or continuing one.
const ABS_MT_TRACKING_ID: u16 = 0x39;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
/// Finger-on-surface, which only touch-class nodes carry — the pad's own node reports
/// `BTN_SOUTH`/`BTN_EAST` and never this. See [`is_pad_touchpad`].
const BTN_TOUCH: u16 = 0x14a;

/// The pad's own south face button — what parts the gamepad node from the motion node, which
/// advertises the same six absolute axes but no buttons at all. See [`is_pad_motion`].
const BTN_SOUTH: u16 = 0x130;

/// Sony's USB/BT vendor id. `hid-playstation` binds only Sony pads, and it's the split-node
/// layout that creates the touchpad node this identifies.
const VENDOR_SONY: u16 = 0x054c;

const KEY_LEFTCTRL: u16 = 29;
const KEY_A: u16 = 30;

/// `poll` revents bits that mean the node is gone, not readable.
const DEAD: libc::c_short = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;

/// `poll` wakeup cadence — bounds stop latency and hot-plug pickup delay.
const POLL_TIMEOUT_MS: i32 = 200;

/// Caps how often a burst of ready reads re-triggers `poll` — a report already coalesces every
/// pending event, so re-draining and forwarding faster than this is pure CPU/host-injection
/// churn, not smoother motion. A moving 1kHz mouse would otherwise make `poll` return
/// continuously (see `reader_loop`), spinning this thread and sending at the device's own rate.
const MOTION_INTERVAL: Duration = Duration::from_millis(2); // 500 Hz
/// How often to rescan `/dev/input` for devices plugged in mid-session.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Kernel `struct input_event`. Built on `libc::timeval` rather than a fixed 16 bytes so the
/// layout follows the target's word size instead of assuming 32-bit.
#[repr(C)]
#[derive(Clone, Copy)]
struct InputEventRaw {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

/// `_IOC(dir, 'E', nr, len)` — the evdev ioctls aren't in the `libc` crate.
const fn eioc(dir: u32, nr: u32, len: u32) -> libc::c_ulong {
    ((dir << 30) | (len << 16) | (b'E' as u32) << 8 | nr) as libc::c_ulong
}

/// `_IOC(_IOC_READ, 'E', nr, len)`.
const fn eviocg(nr: u32, len: u32) -> libc::c_ulong {
    const IOC_READ: u32 = 2;
    eioc(IOC_READ, nr, len)
}

/// `EVIOCGRAB` = `_IOW('E', 0x90, int)`. The kernel reads the argument by value (`1` grabs, `0`
/// releases), not as a pointer, despite the `_IOW` direction.
const fn eviocgrab() -> libc::c_ulong {
    const IOC_WRITE: u32 = 1;
    eioc(IOC_WRITE, 0x90, 4)
}

/// `EVIOCSREP` = `_IOW('E', 0x03, unsigned int[2])` — `[REP_DELAY, REP_PERIOD]` in ms, what the
/// kernel generates `value == 2` autorepeat from.
const fn eviocsrep() -> libc::c_ulong {
    const IOC_WRITE: u32 = 1;
    eioc(IOC_WRITE, 0x03, 8)
}

/// Autorepeat delay/period for every keyboard node we own, in ms. The kernel's 250/33 default is
/// console timing; a desktop OS would re-tune it, and here nothing else will.
const REPEAT_DELAY_MS: u32 = 500;
const REPEAT_PERIOD_MS: u32 = 33;

/// How long after a keypress a HID keyboard still owns SDL's key events — bridges the gaps
/// within a burst of typing, short enough that reaching for the remote still feels immediate.
const IN_USE_WINDOW: Duration = Duration::from_millis(250);

/// One report off a claimed node. Two variants because the two go to different wire planes: a
/// mouse/keyboard event is an `InputEvent` datagram, a pad's touch contacts and motion samples
/// are rich-input ones applied to the host's virtual `DualSense`.
pub enum HidReport<'a> {
    Input(&'a InputEvent),
    Rich(RichInput),
}

/// A HID reader: one thread polling every mouse- or keyboard-shaped evdev node, handing each
/// wire event straight to `sink`. Dropping it stops and joins the thread.
pub struct HidInput {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Everything the reader thread and its caller touch across the thread boundary, one `Arc` for
/// the lot instead of a clone-and-thread-per-field.
struct Shared {
    stop: AtomicBool,
    has_mouse: AtomicBool,
    /// Desired grab/forward state — see [`HidInput::set_active`]. Read by [`reader_loop`] (to
    /// drive `EVIOCGRAB`) and by the reader thread's gated `sink` wrapper; the per-device
    /// *applied* grab state lives on [`Device`], not here.
    grab: AtomicBool,
    /// Cursor capture, fixed for this reader's life (the setting only takes effect next stream):
    /// off leaves a pointer-only node with the compositor, so the TV cursor still aims.
    grab_mouse: bool,
    keys: KeyActivity,
}

/// Last keyboard report, shared with the main thread to tell a HID keyboard from the remote —
/// SDL gives no device identity, so recency is the only discriminator. The pointer needs no
/// equivalent: whether this reader owns the mouse is a fixed fact for a stream, not a recency
/// question ([`HidInput::has_mouse`]). Millis since `base`, since `Instant` isn't atomic.
struct KeyActivity {
    base: Instant,
    last_ms: std::sync::atomic::AtomicU64,
}

impl KeyActivity {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            last_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        self.last_ms
            .store(self.base.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn recent(&self) -> bool {
        let last = self.last_ms.load(Ordering::Relaxed);
        // 0 = nothing seen yet.
        last != 0 && self.base.elapsed().as_millis() as u64 - last < IN_USE_WINDOW.as_millis() as u64
    }
}

impl HidInput {
    /// `None` only if the reader thread itself fails to spawn — a Magic-Remote-only TV (or
    /// jail group mismatch) still gets a thread, so a mouse plugged in mid-session is picked up
    /// by [`reader_loop`]'s own rescan; scanning here on the caller's thread would instead block
    /// every stream (re)connect for the ~40ms/node cost the module's docs describe.
    ///
    /// `sink` runs **on the reader thread**, once per input frame, only while active (see the
    /// module docs), and must not block: queueing for the main loop would re-quantize motion to
    /// tick rate, the resampling this escapes. It takes a [`HidReport`] because a claimed pad
    /// touchpad reports contacts, not `InputEvent`s.
    ///
    /// `active` is the initial [`set_active`](Self::set_active) state — passed here instead of
    /// left to a follow-up call so a caller that always wants "started active" can't forget it.
    /// `grab_mouse` is cursor capture: false leaves pointer-only nodes with the compositor
    /// (desktop/absolute), keyboards are taken either way.
    pub fn start(active: bool, grab_mouse: bool, sink: impl Fn(HidReport) + Send + 'static) -> Option<Self> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            has_mouse: AtomicBool::new(false),
            grab: AtomicBool::new(active),
            grab_mouse,
            keys: KeyActivity::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("pf-evdev".into())
            .spawn(move || {
                let gate = Arc::clone(&thread_shared);
                let gated_sink = move |report: HidReport| {
                    // Rich reports are exempt from the gate: both are levels, not edges, so a
                    // dropped contact is a finger the host holds down forever (see
                    // `release_touches`) and a dropped motion sample is an attitude it keeps.
                    // Keys and clicks self-clear on the next report.
                    if gate.grab.load(Ordering::Relaxed) || matches!(report, HidReport::Rich(_)) {
                        sink(report);
                    }
                };
                reader_loop(&gated_sink, &thread_shared)
            })
            .ok()?;
        Some(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Grab (or release) every open node exclusively, and — same flag — start or stop
    /// calling `sink` with what they report (see the module docs). Applied on the reader thread's
    /// own cadence (bounded by [`POLL_TIMEOUT_MS`]), including to any node that shows up after
    /// this call, so callers don't need to re-invoke it on hot-plug.
    pub fn set_active(&self, active: bool) {
        self.shared.grab.store(active, Ordering::Relaxed);
    }

    /// Whether the reader currently owns at least one pointing node — `false` right after
    /// [`start`] until its background scan completes, or permanently on a Magic-Remote-only TV.
    /// Callers use this instead of `is_some()`/`is_none()` to tell "no HID mouse yet" apart from
    /// "not asked for one", since the reader thread always exists once `start` returns `Some`.
    pub fn has_mouse(&self) -> bool {
        self.shared.has_mouse.load(Ordering::Relaxed)
    }

    /// True while a HID keyboard key was seen within [`IN_USE_WINDOW`] — caller should drop
    /// SDL's echo of that keyboard without dropping the Magic Remote.
    pub fn keyboard_busy(&self) -> bool {
        self.shared.keys.recent()
    }
}

impl Drop for HidInput {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Bounded by `POLL_TIMEOUT_MS` — unlike luna-backed workers, this join can't wedge.
            // Each open `Device` closes on this same join, which is also what releases any
            // `EVIOCGRAB` still held — the kernel drops it on `close()`, no explicit ungrab needed.
            let _ = thread.join();
        }
    }
}

/// An open evdev node, plus motion accumulated from it (`REL_X`/`REL_Y` only mean anything
/// together, at the next `SYN_REPORT`).
struct Device {
    fd: RawFd,
    path: PathBuf,
    /// Summed across this read burst — see `flush_motion`.
    dx: i32,
    dy: i32,
    scroll: mouse::ScrollAccumulator,
    /// Mirrors `commons-evmouse`'s `dev_fd_t.grab`: tracks what's actually applied to `fd`, not
    /// just requested, so a failed `EVIOCGRAB` (device gone, already grabbed elsewhere) is
    /// retried on the next call instead of silently believed.
    grabbed: bool,
    /// Which `want` value the last failed ioctl was for, so a device stuck failing every retry
    /// (device gone, held elsewhere) warns once per state change instead of once per poll cycle.
    grab_warned_for: Option<bool>,
    /// Relative pointer (`REL_X`/`REL_Y`) whose motion this reader owns. Independent of
    /// [`Self::keyboard`]: combo receivers usually expose the two on separate nodes.
    mouse: bool,
    /// Alphanumeric/modifier keys. Magic Remote nodes are excluded by [`open_hid`].
    keyboard: bool,
    /// A pad's touchpad ([`is_pad_touchpad`]), claimed so the compositor can't make a cursor of
    /// it and decoded into [`RichInput::Touchpad`] contacts instead. `None` on every other node.
    touchpad: Option<Touchpad>,
    /// A pad's motion sensors ([`is_pad_motion`]), on their own node — never set together with
    /// [`Self::touchpad`]. `None` on every other node.
    sensors: Option<Sensors>,
}

/// Motion decode state. Sensor readings are levels, not deltas, so a read burst collapses to its
/// newest sample and an unchanged sample is worth no datagram at all — a resting pad reports at
/// its full rate forever.
#[derive(Default)]
struct Sensors {
    /// Pitch/yaw/roll then accelerometer, in the pad's own units (`hid-playstation` keeps the
    /// report's scale), as the wire orders them.
    axes: [i32; 6],
    /// A `SYN_REPORT` completed a sample since the last send.
    dirty: bool,
    /// Last sample actually sent, for the unchanged-sample skip.
    sent: Option<[i16; 6]>,
}

/// Multitouch decode state for a claimed pad touchpad, plus the axis ranges its coordinates are
/// normalized against.
///
/// Protocol B (`ABS_MT_SLOT` + `ABS_MT_TRACKING_ID`), which is what `hid-playstation` reports and
/// all the wire has room for: two contacts, matching the `finger` field's `0`/`1`.
struct Touchpad {
    /// The slot `ABS_MT_*` values currently apply to. Out-of-range slots are parked on the last
    /// finger rather than dropped, so a driver reporting more contacts than the wire carries
    /// can't silently write past the array.
    slot: usize,
    fingers: [Finger; 2],
    /// `(min, max)` of `ABS_MT_POSITION_X` / `_Y`, for the 0..=65535 normalization.
    x_range: (i32, i32),
    y_range: (i32, i32),
}

#[derive(Clone, Copy, Default)]
struct Finger {
    active: bool,
    /// The host currently believes this contact is down. Gates the lift: a node claimed with a
    /// finger already on it reports coordinates without a `ABS_MT_TRACKING_ID`, which would
    /// otherwise send a lift for a contact the host never saw start.
    sent: bool,
    x: i32,
    y: i32,
    /// Something in this report changed the contact — sent at the next `SYN_REPORT` and cleared.
    dirty: bool,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: `fd` came from `open` in `open_mouse` and is owned solely by this struct.
        unsafe { libc::close(self.fd) };
    }
}

/// What probing a node found — matters for whether a later rescan retries it, see [`scan`].
enum Probe {
    Hid(Device),
    Skip,
    Unopenable,
}

/// Opens every node this reader wants that isn't already in `seen`, appending the paths it takes.
fn scan(seen: &mut Vec<PathBuf>, grab_mouse: bool) -> Vec<Device> {
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        tracing::warn!("/dev/input unreadable — no HID input support");
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .filter(|p| !seen.contains(p))
        .collect();
    // Stable order so log lines and device indices don't shuffle between scans.
    paths.sort();
    let mut devices = Vec::new();
    for path in paths {
        match open_hid(&path, grab_mouse) {
            Probe::Hid(dev) => {
                seen.push(path);
                devices.push(dev);
            }
            // Opened and isn't ours — settled, no rescan will change that.
            Probe::Skip => seen.push(path),
            // Not marked seen: an unopenable node (`ENXIO`) looks like a not-yet-plugged
            // dongle, so the next rescan retries it.
            Probe::Unopenable => {}
        }
    }
    devices
}

/// A mouse is a relative-pointing device (`EV_REL` on both axes) that isn't also an absolute
/// pointer, which is what separates a desk mouse from the Magic Remote. A keyboard is a node
/// carrying `KEY_A`/`KEY_LEFTCTRL` that isn't a TV builtin — those advertise a full QWERTY
/// keymap, so [`is_tv_builtin`]'s name denylist is load-bearing.
fn open_hid(path: &Path, grab_mouse: bool) -> Probe {
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return Probe::Skip;
    };
    // SAFETY: NUL-terminated path, standard flags; failure is reported as -1, not UB.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    // Normal case, not a diagnostic: ~30 event nodes on this TV have no device (`ENXIO`) or
    // belong to groups we're not in.
    if fd < 0 {
        return Probe::Unopenable;
    }
    let name = device_name(fd).unwrap_or_default();
    if is_tv_builtin(&name) {
        // SAFETY: `fd` came from `open` above; closing before the Device wrapper exists.
        unsafe { libc::close(fd) };
        return Probe::Skip;
    }
    // Built before the checks below so `Drop` closes `fd` on every reject path.
    let mut dev = Device {
        fd,
        path: path.to_path_buf(),
        dx: 0,
        dy: 0,
        scroll: mouse::ScrollAccumulator::default(),
        grabbed: false,
        grab_warned_for: None,
        mouse: false,
        keyboard: false,
        touchpad: None,
        sensors: None,
    };
    let rel = event_bits(fd, EV_REL);
    let abs = event_bits(fd, EV_ABS);
    let key = event_bits(fd, EV_KEY);
    let has_axes = bit(&rel, REL_X) && bit(&rel, REL_Y);
    // Only absolute *pointer* axes disqualify — "any EV_ABS bit" would reject the Logitech
    // receiver here, which advertises `ABS_VOLUME` for media keys while pointing relatively.
    let absolute_pointer = bit(&abs, ABS_X) && bit(&abs, ABS_Y);
    let pointer = has_axes && !absolute_pointer;
    // KEY_A / KEY_LEFTCTRL rather than "any EV_KEY": mouse nodes advertise BTN_LEFT in EV_KEY
    // without being a keyboard. Absolute-pointer remotes are already excluded above.
    dev.keyboard = !absolute_pointer && (bit(&key, KEY_A) || bit(&key, KEY_LEFTCTRL));
    // A grab is per node, never per event type: taking a combo node's keys takes its pointer
    // too, so forward that pointer as well or the mouse would go dead. Desktop mode pays for
    // that — a combo receiver loses the TV cursor and aims relatively like a captured mouse,
    // where a separate mouse node keeps aiming absolutely. Better than the alternatives: not
    // grabbing means the modifier bug this exists to fix comes back on exactly that hardware.
    dev.mouse = pointer && (grab_mouse || dev.keyboard);
    // Claimed in both cursor modes, unlike a mouse node: leaving it to the compositor is what
    // produces the stuck left button, which desktop mode wants gone just as much.
    if is_pad_touchpad(absolute_pointer, bit(&key, BTN_TOUCH), || device_vendor(fd)) {
        dev.touchpad = Some(Touchpad::new(fd));
    } else if is_pad_motion(&abs, bit(&key, BTN_TOUCH) || bit(&key, BTN_SOUTH), || device_vendor(fd)) {
        dev.sensors = Some(Sensors::default());
    }
    if !dev.mouse && !dev.keyboard && dev.touchpad.is_none() && dev.sensors.is_none() {
        // Not ours, or a pointer-only node in desktop mode — left with the compositor so the
        // TV cursor is still the one you aim.
        return Probe::Skip;
    }
    if dev.keyboard {
        set_repeat(fd);
    }
    let kind = if dev.touchpad.is_some() {
        "pad touchpad"
    } else if dev.sensors.is_some() {
        "pad motion"
    } else if dev.mouse && dev.keyboard {
        "mouse+keyboard"
    } else if dev.mouse {
        "mouse"
    } else {
        "keyboard"
    };
    tracing::info!("HID {kind}: {} ({})", name, path.display());
    Probe::Hid(dev)
}

/// Best-effort: a node that refuses keeps the kernel's timing, which costs repeat feel, not input.
fn set_repeat(fd: RawFd) {
    let rep: [u32; 2] = [REPEAT_DELAY_MS, REPEAT_PERIOD_MS];
    // SAFETY: `fd` is an open evdev node and the ioctl reads exactly the two u32s `rep` holds.
    if unsafe { libc::ioctl(fd, eviocsrep(), rep.as_ptr()) } < 0 {
        tracing::debug!("EVIOCSREP: {}", std::io::Error::last_os_error());
    }
}

/// LG's virtual remotes advertise a full QWERTY keymap, so a "has `KEY_A`" filter would steal
/// the Magic Remote / RCU from the compositor and leave the TV unnavigable. Names as they read
/// in `/proc/bus/input/devices` on-device.
/// A `PlayStation` pad's touchpad, which the kernel publishes as its own absolute/multitouch node
/// alongside the pad itself — the source of the stuck left button [`mouse::is_touch_emulated`]
/// describes, since the compositor drives the TV cursor from it. Claiming it takes it away from
/// the compositor entirely and gives [`read_touch`] the contacts to forward as
/// [`RichInput::Touchpad`], which is what makes a game's touchpad swipes work. The pad's own
/// *click* doesn't come through here at all — that arrives on the pad node as `BTN_TOUCHPAD`
/// through SDL's controller layer.
///
/// Matched on ids and capability bits, not the device name, which varies by driver and firmware:
/// `BTN_TOUCH` on an absolute pointer (the pad's own node reports face buttons instead), from a
/// Sony vendor id. `vendor` is lazy because it costs an ioctl the bit tests don't.
fn is_pad_touchpad(absolute_pointer: bool, has_btn_touch: bool, vendor: impl FnOnce() -> u16) -> bool {
    absolute_pointer && has_btn_touch && vendor() == VENDOR_SONY
}

/// A `PlayStation` pad's motion sensors, published as a third node beside the pad and its
/// touchpad. SDL2 never reads it, so gyro aiming reaches the host only if this reader forwards it
/// as [`RichInput::Motion`].
///
/// Matched on capability bits and vendor, like [`is_pad_touchpad`]: all six sensor axes and no
/// buttons. Both other nodes carry those same axes — the pad node's are its sticks and triggers,
/// the touchpad's are the contact — so the buttons (`BTN_SOUTH`, `BTN_TOUCH`) are what part them.
/// Matching the pad node here would grab the gamepad away from SDL entirely.
fn is_pad_motion(abs: &[u8; 128], has_buttons: bool, vendor: impl FnOnce() -> u16) -> bool {
    (ABS_X..=ABS_RZ).all(|c| bit(abs, c)) && !has_buttons && vendor() == VENDOR_SONY
}

impl Touchpad {
    /// Reads the node's axis ranges once, at open. A node whose `ABS_MT_POSITION_*` ranges are
    /// unusable is still worth claiming (the compositor must not have it) — it just reports no
    /// contacts, which [`normalize`] enforces.
    fn new(fd: RawFd) -> Self {
        Self {
            slot: 0,
            fingers: [Finger::default(); 2],
            x_range: abs_range(fd, ABS_MT_POSITION_X),
            y_range: abs_range(fd, ABS_MT_POSITION_Y),
        }
    }
}

/// Device units → the wire's `0..=65535` across the pad. `None` when the driver gave no range to
/// scale against, since a bogus coordinate is worse than no contact at all.
fn normalize(x: i32, y: i32, x_range: (i32, i32), y_range: (i32, i32)) -> Option<(u16, u16)> {
    let scale = |v: i32, (min, max): (i32, i32)| {
        let span = i64::from(max) - i64::from(min);
        (span > 0).then(|| ((i64::from(v.clamp(min, max)) - i64::from(min)) * 65535 / span) as u16)
    };
    // evdev's +y is already down, the direction the wire and the host's virtual pad expect.
    Some((scale(x, x_range)?, scale(y, y_range)?))
}

/// `EVIOCGABS(code)`'s `(minimum, maximum)` out of `struct input_absinfo`
/// (`value, minimum, maximum, fuzz, flat, resolution`). Falls back to `(0, 0)` when the ioctl
/// fails, which [`normalize`] reads as "no usable range".
fn abs_range(fd: RawFd, code: u16) -> (i32, i32) {
    let mut info = [0i32; 6];
    // SAFETY: as `event_bits` — length-encoding request, buffer of exactly that length.
    let rc = unsafe {
        libc::ioctl(
            fd,
            eviocg(0x40 + u32::from(code), std::mem::size_of_val(&info) as u32),
            info.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return (0, 0);
    }
    (info[1], info[2])
}

/// The vendor id out of `EVIOCGID`'s `struct input_id` (`bustype, vendor, product, version`) —
/// the only field anything here needs. `0` when the ioctl fails, which matches no vendor.
fn device_vendor(fd: RawFd) -> u16 {
    let mut id = [0u16; 4];
    // SAFETY: as `event_bits` — length-encoding request, buffer of exactly that length.
    let rc = unsafe { libc::ioctl(fd, eviocg(0x02, std::mem::size_of_val(&id) as u32), id.as_mut_ptr()) };
    if rc < 0 {
        return 0;
    }
    id[1]
}

fn is_tv_builtin(name: &str) -> bool {
    name.starts_with("LGE") || name.starts_with("Bluetooth-audio") || matches!(name, "CHECK INPUT" | "IoT keypad")
}

/// The `EVIOCGBIT` bitmap of supported codes for event type `kind`, or all-zero when the
/// ioctl fails. 128 bytes covers every `REL`/`ABS` code (and `KEY` up to 0x3ff, unused here).
fn event_bits(fd: RawFd, kind: u16) -> [u8; 128] {
    let mut bits = [0u8; 128];
    // SAFETY: the request encodes the buffer length, and the buffer is exactly that long.
    let rc = unsafe { libc::ioctl(fd, eviocg(0x20 + u32::from(kind), bits.len() as u32), bits.as_mut_ptr()) };
    if rc < 0 {
        return [0u8; 128];
    }
    bits
}

fn bit(bits: &[u8; 128], code: u16) -> bool {
    let idx = code as usize / 8;
    idx < bits.len() && bits[idx] & (1 << (code % 8)) != 0
}

/// Applies `want` to every device whose `grabbed` disagrees — same idempotent check as
/// `commons-evmouse`'s `evmouse_set_grab`, so a steady state costs no ioctls at all.
fn apply_grab(devices: &mut [Device], want: bool) {
    for dev in devices {
        // A pad touchpad is claimed whether the reader is active or not: ungrabbing it hands the
        // compositor back a node it turns into a cursor with a stuck left button, which is the
        // whole reason it's claimed (see `is_pad_touchpad`).
        // Same for the motion node: it too advertises absolute axes the compositor would
        // otherwise turn into a cursor, and its samples are only useful forwarded.
        let want = want || dev.touchpad.is_some() || dev.sensors.is_some();
        if dev.grabbed == want {
            continue;
        }
        // SAFETY: plain integer ioctl (see `eviocgrab`'s doc); the kernel reads the argument by
        // value. Failure just means the double cursor persists, not a wedged device.
        let rc = unsafe { libc::ioctl(dev.fd, eviocgrab(), libc::c_int::from(want)) };
        if rc < 0 {
            if dev.grab_warned_for != Some(want) {
                tracing::warn!(
                    "EVIOCGRAB({want}) failed on {}: {}",
                    dev.path.display(),
                    std::io::Error::last_os_error()
                );
                dev.grab_warned_for = Some(want);
            }
            continue;
        }
        dev.grabbed = want;
    }
}

/// `EVIOCGNAME` — for the log line, so a bug report from an unknown dongle names it.
fn device_name(fd: RawFd) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: as `event_bits` — length-encoding request, buffer of that length.
    let rc = unsafe { libc::ioctl(fd, eviocg(0x06, buf.len() as u32), buf.as_mut_ptr()) };
    if rc <= 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

fn reader_loop(sink: &impl Fn(HidReport), shared: &Shared) {
    // Scan at the default niceness: `open`/`ioctl` cost here is the driver's, not scheduling
    // delay, so boosting priority wouldn't speed it up — it would just pull CPU from the video
    // pump during exactly the busiest window (stream connect) for no benefit.
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut devices = scan(&mut seen, shared.grab_mouse);
    if devices.is_empty() {
        tracing::info!("no HID mouse/keyboard on /dev/input yet — using SDL input until one appears");
    }
    store_presence(&devices, shared);
    // Nice -10, like the video pump, from here on: at nice 0 this thread lost the CPU to
    // boosted decode threads for up to 28ms at a stretch while a 1kHz mouse kept reporting —
    // exactly the jitter this module exists to remove.
    // SAFETY: plain scalar arguments; failure returns -1 and changes nothing.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, -10) };
    let mut last_scan = Instant::now();
    let mut dir_mtime = std::fs::metadata("/dev/input").and_then(|m| m.modified()).ok();
    // Rebuilt only on device-set change — a moving 1kHz mouse makes `poll` return continuously,
    // so rebuilding every iteration was a malloc/free per read burst.
    let mut fds = pollfds(&devices);
    while !shared.stop.load(Ordering::Relaxed) {
        // No-op unless the state flipped; also covers the first iteration, so no separate
        // pre-loop call is needed.
        apply_grab(&mut devices, shared.grab.load(Ordering::Relaxed));
        let iter_start = Instant::now();
        // SAFETY: `fds` is a valid slice of `nfds` pollfds for the duration of the call.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_TIMEOUT_MS) };
        if rc > 0 {
            for (i, pfd) in fds.iter().enumerate() {
                if pfd.revents & libc::POLLIN != 0 {
                    read_device(&mut devices[i], sink, &shared.keys);
                }
            }
            // An unplugged node polls ready forever with POLLERR/HUP; checked cheaply since
            // unplugs are rare but this runs every ready pass.
            if fds.iter().any(|p| p.revents & DEAD != 0) {
                for i in (0..fds.len()).rev() {
                    if fds[i].revents & DEAD == 0 {
                        continue;
                    }
                    release_touches(&mut devices[i], sink);
                    let gone = devices.remove(i);
                    tracing::info!("HID device gone: {}", gone.path.display());
                    seen.retain(|p| *p != gone.path);
                }
                fds = pollfds(&devices);
                store_presence(&devices, shared);
            }
            // Each pass above already drained and flushed everything pending, so a mouse
            // reporting faster than `MOTION_INTERVAL` just means going straight back into
            // `poll` ready again — pace that instead of re-draining/re-sending at its rate.
            if let Some(remaining) = MOTION_INTERVAL.checked_sub(iter_start.elapsed()) {
                std::thread::sleep(remaining);
            }
        }
        // Gated on the directory's mtime, not just the interval: opening a node costs ~40ms and
        // ~20 nodes are empty and retryable, so an unconditional rescan would stall this thread
        // (read as motion jitter) every `RESCAN_INTERVAL`.
        if last_scan.elapsed() >= RESCAN_INTERVAL {
            last_scan = Instant::now();
            let mtime = std::fs::metadata("/dev/input").and_then(|m| m.modified()).ok();
            if mtime != dir_mtime {
                dir_mtime = mtime;
                let found = scan(&mut seen, shared.grab_mouse);
                if !found.is_empty() {
                    devices.extend(found);
                    fds = pollfds(&devices);
                    store_presence(&devices, shared);
                }
            }
        }
    }
    // Stopping with a finger on the pad must not leave the host holding it.
    for dev in &mut devices {
        release_touches(dev, sink);
    }
}

/// Only pointing nodes count: the caller uses this to decide whether SDL's pointer is ours,
/// which a keyboard-only reader says nothing about.
fn store_presence(devices: &[Device], shared: &Shared) {
    shared
        .has_mouse
        .store(devices.iter().any(|d| d.mouse), Ordering::Relaxed);
}

fn pollfds(devices: &[Device]) -> Vec<libc::pollfd> {
    devices
        .iter()
        .map(|d| libc::pollfd {
            fd: d.fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect()
}

/// Drains one device's pending events, emitting the summed motion once at the end — a burst is
/// reports the kernel already queued, so summing costs no latency and beats a datagram per event
/// at 1kHz (the host's own injector coalesces the same way).
fn read_device(dev: &mut Device, sink: &impl Fn(HidReport), keys: &KeyActivity) {
    let size = std::mem::size_of::<InputEventRaw>();
    let mut buf = [0u8; 1024];
    loop {
        // SAFETY: reading into a local byte buffer of exactly `buf.len()`.
        let n = unsafe { libc::read(dev.fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return; // EAGAIN on an empty non-blocking node, or the node went away
        }
        let n = n as usize;
        // A claimed touchpad shares none of the mouse/keyboard decode below — different axes,
        // different wire plane — so it takes the whole burst on its own path.
        if let Some(pad) = dev.touchpad.as_mut() {
            read_touch(pad, &buf[..n], size, sink);
        } else if let Some(sensors) = dev.sensors.as_mut() {
            read_sensors(sensors, &buf[..n], size);
        } else {
            decode_hid(dev, &buf[..n], size, sink, keys);
        }
        if n < buf.len() {
            break;
        }
    }
    flush_motion(dev, sink);
    if let Some(sensors) = dev.sensors.as_mut() {
        flush_sensors(sensors, sink);
    }
}

/// Decodes one read burst off a mouse/keyboard node — see [`read_device`] for the drain.
fn decode_hid(dev: &mut Device, buf: &[u8], size: usize, sink: &impl Fn(HidReport), keys: &KeyActivity) {
    for chunk in buf.chunks_exact(size) {
        // SAFETY: `InputEventRaw` is plain `repr(C)` integers with no padding
        // invariants, and the chunk is exactly its size; read unaligned because the
        // buffer offset carries no alignment guarantee.
        let ev = unsafe { chunk.as_ptr().cast::<InputEventRaw>().read_unaligned() };
        match ev.kind {
            EV_REL if dev.mouse => match ev.code {
                REL_X | REL_Y => {
                    if ev.code == REL_X {
                        dev.dx += ev.value;
                    } else {
                        dev.dy += ev.value;
                    }
                }
                // evdev's wheel is one unit per notch, same as SDL's, so the ×120 wire
                // scaling accumulator applies unchanged.
                REL_WHEEL | REL_HWHEEL => {
                    flush_motion(dev, sink);
                    if let Some(e) = dev.scroll.scroll_event(ev.value, ev.code == REL_HWHEEL) {
                        sink(HidReport::Input(&e));
                    }
                }
                _ => {}
            },
            // 0 = release, 1 = press, 2 = autorepeat (keyboard-only).
            EV_KEY if matches!(ev.value, 0..=2) => {
                // Buttons and keys share `EV_KEY`, and a combo node reports both — the code
                // ranges are what tells them apart, not the device.
                // Buttons skip repeats: one would double-fire the click.
                if let Some(button) = (dev.mouse && ev.value != 2).then(|| button_code(ev.code)).flatten() {
                    // Motion first: the click must land where the pointer already is.
                    flush_motion(dev, sink);
                    sink(HidReport::Input(&mouse::raw_button_event(button, ev.value == 1)));
                } else if let Some(vk) = dev.keyboard.then(|| keyboard::vk_from_evdev(ev.code)).flatten() {
                    keys.touch();
                    // Autorepeat rides as a repeated KeyDown — the host has no repeat timer of
                    // its own, so dropping these kills held-key repeat.
                    sink(HidReport::Input(&keyboard::raw_key_event(vk, ev.value != 0)));
                }
            }
            _ => {}
        }
    }
}

/// Decodes one read burst off a claimed pad touchpad into [`RichInput::Touchpad`] contacts.
///
/// Contacts are sent at `SYN_REPORT`, not per axis event: a moving finger reports x and y as two
/// events, and sending between them would put half the samples on a stale axis. Only contacts a
/// report actually touched are sent, so a resting finger costs one burst of decode and no
/// datagrams at all.
fn read_touch(pad: &mut Touchpad, buf: &[u8], size: usize, sink: &impl Fn(HidReport)) {
    for chunk in buf.chunks_exact(size) {
        // SAFETY: as `read_device` — exact-size chunk of plain `repr(C)` integers, read unaligned.
        let ev = unsafe { chunk.as_ptr().cast::<InputEventRaw>().read_unaligned() };
        match (ev.kind, ev.code) {
            // A slot the wire can't carry is parked on the last finger, where its coordinates are
            // simply overwritten by a contact that does fit — the pad only ever reports two.
            (EV_ABS, ABS_MT_SLOT) => pad.slot = (ev.value.max(0) as usize).min(pad.fingers.len() - 1),
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let finger = &mut pad.fingers[pad.slot];
                finger.active = ev.value >= 0;
                finger.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_X | ABS_MT_POSITION_Y) => {
                let finger = &mut pad.fingers[pad.slot];
                if ev.code == ABS_MT_POSITION_X {
                    finger.x = ev.value;
                } else {
                    finger.y = ev.value;
                }
                finger.dirty = true;
            }
            (EV_SYN, SYN_REPORT) => {
                let (x_range, y_range) = (pad.x_range, pad.y_range);
                for (i, finger) in pad.fingers.iter_mut().enumerate() {
                    if !finger.dirty {
                        continue;
                    }
                    finger.dirty = false;
                    if !finger.active && !finger.sent {
                        continue; // never announced — nothing to lift
                    }
                    let Some(contact) = contact(i, finger, x_range, y_range) else {
                        continue;
                    };
                    finger.sent = finger.active;
                    sink(HidReport::Rich(contact));
                }
            }
            _ => {}
        }
    }
}

/// Lifts every contact the host still believes is down on this node. A touch is a level, not an
/// edge: a pad unplugged (or a reader stopped) mid-gesture would otherwise leave the finger down
/// on the host's virtual pad with nothing left to release it.
fn release_touches(dev: &mut Device, sink: &impl Fn(HidReport)) {
    let Some(pad) = dev.touchpad.as_mut() else {
        return;
    };
    let (x_range, y_range) = (pad.x_range, pad.y_range);
    for (i, finger) in pad.fingers.iter_mut().enumerate() {
        if !std::mem::take(&mut finger.sent) {
            continue;
        }
        finger.active = false;
        finger.dirty = false;
        if let Some(contact) = contact(i, finger, x_range, y_range) {
            sink(HidReport::Rich(contact));
        }
    }
}

/// One contact for the wire, or `None` when the node gave no usable axis range ([`normalize`]).
///
/// A lift's coordinates are whatever the contact last had, which is what the host wants — the
/// release still has to land where the finger was. `pad` is 0 throughout: this client drives a
/// single pad.
fn contact(finger_idx: usize, finger: &Finger, x_range: (i32, i32), y_range: (i32, i32)) -> Option<RichInput> {
    let (x, y) = normalize(finger.x, finger.y, x_range, y_range)?;
    Some(RichInput::Touchpad {
        pad: 0,
        finger: finger_idx as u8,
        active: finger.active,
        x,
        y,
    })
}

/// Decodes one read burst off a claimed motion node into the newest complete sample.
fn read_sensors(sensors: &mut Sensors, buf: &[u8], size: usize) {
    for chunk in buf.chunks_exact(size) {
        // SAFETY: as `read_device` — exact-size chunk of plain `repr(C)` integers, read unaligned.
        let ev = unsafe { chunk.as_ptr().cast::<InputEventRaw>().read_unaligned() };
        match (ev.kind, ev.code) {
            // Gyro first, then accelerometer: the wire's order, so no shuffling at send.
            (EV_ABS, ABS_RX..=ABS_RZ) => sensors.axes[(ev.code - ABS_RX) as usize] = ev.value,
            (EV_ABS, ABS_X..=ABS_Z) => sensors.axes[(ev.code - ABS_X) as usize + 3] = ev.value,
            // Only a completed report is a sample: sending mid-report would mix axes from two.
            (EV_SYN, SYN_REPORT) => sensors.dirty = true,
            _ => {}
        }
    }
}

/// Sends the newest sample once per read burst, and only when it moved — a pad lying still
/// reports forever, and re-sending its resting attitude at 500 Hz would cost a datagram per
/// sample to tell the host nothing.
///
/// Clamped to `i16`: the wire carries the `DualSense` report's own signed-16 units, and
/// `hid-playstation`'s calibration can push a sample a little past that at the extremes.
fn flush_sensors(sensors: &mut Sensors, sink: &impl Fn(HidReport)) {
    if !std::mem::take(&mut sensors.dirty) {
        return;
    }
    let axes = sensors
        .axes
        .map(|v| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
    if sensors.sent == Some(axes) {
        return;
    }
    sensors.sent = Some(axes);
    let [gx, gy, gz, ax, ay, az] = axes;
    sink(HidReport::Rich(RichInput::Motion {
        pad: 0,
        gyro: [gx, gy, gz],
        accel: [ax, ay, az],
    }));
}

/// Sends the accumulated deltas as one `MouseMove`, undamped — damping is for the remote's
/// coarse deltas and would make a real mouse sluggish.
fn flush_motion(dev: &mut Device, sink: &impl Fn(HidReport)) {
    if dev.dx == 0 && dev.dy == 0 {
        return;
    }
    sink(HidReport::Input(&mouse::move_relative_event(dev.dx, dev.dy)));
    dev.dx = 0;
    dev.dy = 0;
}

/// evdev button → wire numbering (orderings differ: evdev has RIGHT before MIDDLE, wire has
/// middle as 2).
fn button_code(code: u16) -> Option<u32> {
    match code {
        BTN_LEFT => Some(1),
        BTN_MIDDLE => Some(2),
        BTN_RIGHT => Some(3),
        BTN_SIDE => Some(4),
        BTN_EXTRA => Some(5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_pad_motion, is_pad_touchpad, is_tv_builtin, ABS_RZ, ABS_X};

    #[test]
    fn tv_builtins_are_skipped_by_name() {
        assert!(is_tv_builtin("LGE M-RCU - Builtin [0]"));
        assert!(is_tv_builtin("LGE RCU"));
        assert!(is_tv_builtin("CHECK INPUT"));
        assert!(!is_tv_builtin("M720 Triathlon Mouse"));
        assert!(!is_tv_builtin("MX MCHNCL M Keyboard"));
    }

    /// Sony's other nodes must not match: claiming the pad node would take the gamepad away from
    /// SDL, and the motion-sensor node reports absolutely without ever being touched.
    #[test]
    fn pad_touchpad_matches_only_the_touch_node() {
        let sony = || 0x054c;
        // Touchpad: BTN_TOUCH on an absolute pointer.
        assert!(is_pad_touchpad(true, true, sony));
        // Pad node — absolute (sticks), face buttons instead of BTN_TOUCH.
        assert!(!is_pad_touchpad(true, false, sony));
        // Motion sensors — no touch, no pointer axes.
        assert!(!is_pad_touchpad(false, false, sony));
        // A touchscreen from anyone else stays the compositor's.
        assert!(!is_pad_touchpad(true, true, || 0x046d));
    }

    #[test]
    fn pad_motion_matches_only_the_sensor_node() {
        let sony = || 0x054c;
        let mut sensors = [0u8; 128];
        for c in ABS_X..=ABS_RZ {
            sensors[(c / 8) as usize] |= 1 << (c % 8);
        }
        assert!(is_pad_motion(&sensors, false, sony));
        // The touchpad node (BTN_TOUCH) and the pad node (BTN_SOUTH, sticks and triggers) cover
        // the same six axes — the buttons are the only thing that parts them from the sensors.
        assert!(!is_pad_motion(&sensors, true, sony));
        assert!(!is_pad_motion(&sensors, false, || 0x046d));
    }
}
