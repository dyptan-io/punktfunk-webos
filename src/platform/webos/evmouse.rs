//! Raw HID mouse **and keyboard** input straight from `/dev/input/event*`, bypassing SDL.
//!
//! **Why.** SDL mouse events on webOS come from the compositor's pointer, smoothed/resampled
//! for a wrist-waved remote rather than a 125–1000 Hz mouse — jittery deltas no matter what the
//! client does with them. evdev is the same bypass aurora-tv ships as "Use Hardware Mouse".
//! Keyboards need the same exclusive grab in **both** cursor modes: an ungrabbled USB/Bluetooth
//! keyboard still reaches surface-manager, which then treats modifier+click as a system gesture,
//! warps its pointer to screen centre on the first typed character, and pops "This app does not
//! support quick control" on a double right-click. The Magic Remote is an absolute-pointer node
//! and is never opened here.
//!
//! **Two cursor modes.** Capture on (games): mouse nodes are grabbed too, SDL relative is off,
//! the compositor pointer is hidden. Capture off (desktop/absolute): mouse nodes are left with
//! the compositor so the TV pointer stays the one you aim — only the keyboard is stolen.
//!
//! **Access.** Unlike `/dev/hidraw*` (jail-blocked, see `dualsense.rs`), evdev nodes are
//! reachable: `root:compositor 0660`, and the app's uid carries gid 505 in its supplementary
//! groups — verified on-device, non-rooted, webOS 10.3.
//!
//! **Grabbed while active.** `EVIOCGRAB` is scoped to [`HidMouse::set_active`], not held for the
//! reader's whole life: `cursor::COMPOSITOR_CURSOR_CONTROL` is verified off on webOS 26 (see
//! `cursor.rs`), so an ungrabbed node leaves the compositor drawing its own pointer from the same
//! evdev reports we forward. Scoping it to "caller wants it" rather than the reader's whole life
//! bounds a wedged thread's blast radius to "no HID input" instead of "no mouse input at all,
//! TV-wide" — the kernel releases the grab the moment our fd closes (including on panic), and the
//! surface-manager's own fd stays open throughout, just starved of events while ours holds it.
//! The Magic Remote never matches the mouse/keyboard filter, so it stays usable via SDL —
//! which is what [`HidMouse::owns_sdl_motion`] is for.
//!
//! **One flag, two effects.** "Grabbed" and "forwarded to the host" are the same condition by
//! construction, not two atomics a caller has to keep in sync: [`HidMouse::set_active`]`(false)`
//! both releases the node and stops calling `sink`, in one store — needed for the disconnect
//! dialog, which hands the pointer back to the Magic Remote and needs a HID mouse to go quiet for
//! the same window.

use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use punktfunk_core::input::InputEvent;

use super::keyboard;
use super::mouse;

const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;

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

/// How long after a report this device still owns SDL's pointer events — bridges pauses within
/// one drag, short enough that switching to the remote still feels immediate.
const IN_USE_WINDOW: Duration = Duration::from_millis(250);

/// A HID mouse reader: one thread polling every mouse-shaped evdev node, handing each wire
/// event straight to `sink`. Dropping it stops and joins the thread.
pub struct HidMouse {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Everything the reader thread and its caller touch across the thread boundary, one `Arc` for
/// the lot instead of a clone-and-thread-per-field.
struct Shared {
    stop: AtomicBool,
    has_device: AtomicBool,
    has_keyboard: AtomicBool,
    /// Desired grab/forward state — see [`HidMouse::set_active`]. Read by [`reader_loop`] (to
    /// drive `EVIOCGRAB`) and by the reader thread's gated `sink` wrapper; the per-device
    /// *applied* grab state lives on [`Device`], not here.
    grab: AtomicBool,
    /// Capture on: grab and forward mouse nodes. Capture off (desktop/absolute): leave the
    /// mouse with the compositor so the TV pointer is still the one you aim.
    grab_mouse: AtomicBool,
    activity: Activity,
}

/// Last-report timestamp, shared with the main thread to tell the mouse and remote apart — SDL
/// gives no device identity here, so recency is the only discriminator. Millis since `base`,
/// not `Instant`, since `Instant` isn't atomic.
struct Activity {
    base: Instant,
    motion_ms: std::sync::atomic::AtomicU64,
    /// Separate from motion: a click moves nothing, so a still mouse would look idle otherwise.
    discrete_ms: std::sync::atomic::AtomicU64,
    /// USB/BT keyboard reports — so SDL's echo of those keys can be dropped without
    /// swallowing Magic Remote keys, which never appear on these nodes.
    key_ms: std::sync::atomic::AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            motion_ms: std::sync::atomic::AtomicU64::new(0),
            discrete_ms: std::sync::atomic::AtomicU64::new(0),
            key_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn touch(&self, slot: &std::sync::atomic::AtomicU64) {
        slot.store(self.base.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn recent(&self, slot: &std::sync::atomic::AtomicU64) -> bool {
        let last = slot.load(Ordering::Relaxed);
        // 0 = nothing seen yet.
        last != 0 && self.base.elapsed().as_millis() as u64 - last < IN_USE_WINDOW.as_millis() as u64
    }
}

impl HidMouse {
    /// `None` only if the reader thread itself fails to spawn — a Magic-Remote-only TV (or
    /// jail group mismatch) still gets a thread, so a mouse plugged in mid-session is picked up
    /// by [`reader_loop`]'s own rescan; scanning here on the caller's thread would instead block
    /// every stream (re)connect for the ~40ms/node cost the module's docs describe.
    ///
    /// `sink` runs **on the reader thread**, once per input frame, only while active (see the
    /// module docs), and must not block: queueing for the main loop would re-quantize motion to
    /// tick rate, the resampling this escapes.
    ///
    /// `active` is the initial [`set_active`](Self::set_active) state — passed here instead of
    /// left to a follow-up call so a caller that always wants "started active" can't forget it.
    /// `grab_mouse` is Capture: false leaves mouse nodes with the compositor (desktop/absolute).
    pub fn start(active: bool, grab_mouse: bool, sink: impl Fn(&InputEvent) + Send + 'static) -> Option<Self> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            has_device: AtomicBool::new(false),
            has_keyboard: AtomicBool::new(false),
            grab: AtomicBool::new(active),
            grab_mouse: AtomicBool::new(grab_mouse),
            activity: Activity::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("pf-evmouse".into())
            .spawn(move || {
                let gate = Arc::clone(&thread_shared);
                let gated_sink = move |ev: &InputEvent| {
                    if gate.grab.load(Ordering::Relaxed) {
                        sink(ev);
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

    /// Grab (or release) every open mouse node exclusively, and — same flag — start or stop
    /// calling `sink` with what they report (see the module docs). Applied on the reader thread's
    /// own cadence (bounded by [`POLL_TIMEOUT_MS`]), including to any node that shows up after
    /// this call, so callers don't need to re-invoke it on hot-plug.
    pub fn set_active(&self, active: bool) {
        self.shared.grab.store(active, Ordering::Relaxed);
    }

    /// Whether the reader currently owns at least one open mouse node — `false` right after
    /// [`start`] until its background scan completes, or permanently on a Magic-Remote-only TV.
    /// Callers use this instead of `is_some()`/`is_none()` to tell "no HID mouse yet" apart from
    /// "not asked for one", since the reader thread always exists once `start` returns `Some`.
    pub fn has_device(&self) -> bool {
        self.shared.has_device.load(Ordering::Relaxed)
    }

    /// A USB/Bluetooth keyboard node is open. Distinct from [`Self::has_device`]: a mouse-only
    /// dongle must not make the stream drop Magic Remote keys.
    pub fn has_keyboard(&self) -> bool {
        self.shared.has_keyboard.load(Ordering::Relaxed)
    }

    /// True while the mouse moved within [`IN_USE_WINDOW`] — caller should drop SDL's echo of it.
    pub fn owns_sdl_motion(&self) -> bool {
        self.shared.activity.recent(&self.shared.activity.motion_ms)
    }

    /// Same question for SDL's buttons/wheel; also true during motion, so a click mid-drag is
    /// covered even if its echo arrives before this reader's own read of it.
    pub fn owns_sdl_clicks(&self) -> bool {
        self.shared.activity.recent(&self.shared.activity.discrete_ms) || self.owns_sdl_motion()
    }

    /// True while a HID keyboard key was seen within [`IN_USE_WINDOW`] — caller should drop
    /// SDL's echo of that keyboard without dropping the Magic Remote.
    pub fn owns_sdl_keys(&self) -> bool {
        self.shared.activity.recent(&self.shared.activity.key_ms)
    }
}

impl Drop for HidMouse {
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

/// An open mouse node, plus motion accumulated from it (`REL_X`/`REL_Y` only mean anything
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
    /// Relative pointer (REL_X/REL_Y). Independent of [`Self::keyboard`]: Logitech combo
    /// receivers often expose both on separate nodes, occasionally on one.
    mouse: bool,
    /// Alphanumeric/modifier keys. Magic Remote nodes are excluded by [`open_hid`].
    keyboard: bool,
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

/// Opens every mouse-shaped node not already in `seen`, appending the paths it takes.
fn scan(seen: &mut Vec<PathBuf>, grab_mouse: bool) -> Vec<Device> {
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        tracing::warn!("/dev/input unreadable — no HID mouse support");
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

/// A HID mouse is a relative-pointing device (`EV_REL` on both axes) that isn't also an
/// absolute pointer, which is what separates a desk mouse from the Magic Remote. A HID
/// keyboard is a node with `KEY_A`/`KEY_LEFTCTRL` that isn't a TV-builtin remote — those
/// advertise every key bit, so the name denylist is load-bearing.
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
    };
    let rel = event_bits(fd, EV_REL);
    let abs = event_bits(fd, EV_ABS);
    let key = event_bits(fd, EV_KEY);
    let has_axes = bit(&rel, REL_X) && bit(&rel, REL_Y);
    // Only absolute *pointer* axes disqualify — "any EV_ABS bit" would reject the Logitech
    // receiver here, which advertises `ABS_VOLUME` for media keys while pointing relatively.
    let absolute_pointer = bit(&abs, ABS_X) && bit(&abs, ABS_Y);
    dev.mouse = has_axes && !absolute_pointer;
    // KEY_A / KEY_LEFTCTRL rather than "any EV_KEY": mouse nodes advertise BTN_LEFT in EV_KEY
    // without being a keyboard. Absolute-pointer remotes are already excluded above.
    dev.keyboard = !absolute_pointer && (bit(&key, KEY_A) || bit(&key, KEY_LEFTCTRL));
    if dev.mouse && !dev.keyboard && !grab_mouse {
        // Desktop/absolute: leave the mouse with the compositor so the TV pointer still aims.
        return Probe::Skip;
    }
    if !dev.mouse && !dev.keyboard {
        return Probe::Skip;
    }
    tracing::info!(
        "HID {}: {} ({})",
        match (dev.mouse, dev.keyboard) {
            (true, true) => "mouse+keyboard",
            (true, false) => "mouse",
            _ => "keyboard",
        },
        name,
        path.display()
    );
    Probe::Hid(dev)
}

/// LG's virtual remotes advertise a full QWERTY keymap, so a "has KEY_A" filter would steal
/// the Magic Remote / RCU from the compositor. Match names from `/proc/bus/input/devices`.
fn is_tv_builtin(name: &str) -> bool {
    name.starts_with("LGE") || name == "CHECK INPUT" || name == "IoT keypad" || name.starts_with("Bluetooth-audio")
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
fn apply_grab(devices: &mut [Device], grab: bool, grab_mouse: bool) {
    for dev in devices {
        let want = grab && (dev.keyboard || (dev.mouse && grab_mouse));
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

fn reader_loop(sink: &impl Fn(&InputEvent), shared: &Shared) {
    // Scan at the default niceness: `open`/`ioctl` cost here is the driver's, not scheduling
    // delay, so boosting priority wouldn't speed it up — it would just pull CPU from the video
    // pump during exactly the busiest window (stream connect) for no benefit.
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut devices = scan(&mut seen, shared.grab_mouse.load(Ordering::Relaxed));
    if devices.is_empty() {
        tracing::info!("no HID mouse/keyboard on /dev/input yet — using SDL pointer until one appears");
    } else {
        store_presence(&devices, shared);
    }
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
        apply_grab(
            &mut devices,
            shared.grab.load(Ordering::Relaxed),
            shared.grab_mouse.load(Ordering::Relaxed),
        );
        let iter_start = Instant::now();
        // SAFETY: `fds` is a valid slice of `nfds` pollfds for the duration of the call.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_TIMEOUT_MS) };
        if rc > 0 {
            for (i, pfd) in fds.iter().enumerate() {
                if pfd.revents & libc::POLLIN != 0 {
                    read_device(
                        &mut devices[i],
                        sink,
                        &shared.activity,
                        shared.grab_mouse.load(Ordering::Relaxed),
                    );
                }
            }
            // An unplugged node polls ready forever with POLLERR/HUP; checked cheaply since
            // unplugs are rare but this runs every ready pass.
            if fds.iter().any(|p| p.revents & DEAD != 0) {
                for i in (0..fds.len()).rev() {
                    if fds[i].revents & DEAD == 0 {
                        continue;
                    }
                    let gone = devices.remove(i);
                    tracing::info!("HID mouse gone: {}", gone.path.display());
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
                let found = scan(&mut seen, shared.grab_mouse.load(Ordering::Relaxed));
                if !found.is_empty() {
                    devices.extend(found);
                    fds = pollfds(&devices);
                    store_presence(&devices, shared);
                }
            }
        }
    }
}

fn store_presence(devices: &[Device], shared: &Shared) {
    shared
        .has_device
        .store(devices.iter().any(|d| d.mouse), Ordering::Relaxed);
    shared
        .has_keyboard
        .store(devices.iter().any(|d| d.keyboard), Ordering::Relaxed);
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
fn read_device(dev: &mut Device, sink: &impl Fn(&InputEvent), activity: &Activity, grab_mouse: bool) {
    let size = std::mem::size_of::<InputEventRaw>();
    let mut buf = [0u8; 1024];
    loop {
        // SAFETY: reading into a local byte buffer of exactly `buf.len()`.
        let n = unsafe { libc::read(dev.fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return; // EAGAIN on an empty non-blocking node, or the node went away
        }
        let n = n as usize;
        for chunk in buf[..n].chunks_exact(size) {
            // SAFETY: `InputEventRaw` is plain `repr(C)` integers with no padding
            // invariants, and the chunk is exactly its size; read unaligned because the
            // buffer offset carries no alignment guarantee.
            let ev = unsafe { chunk.as_ptr().cast::<InputEventRaw>().read_unaligned() };
            match ev.kind {
                EV_REL if dev.mouse && grab_mouse => match ev.code {
                    REL_X | REL_Y => {
                        activity.touch(&activity.motion_ms);
                        if ev.code == REL_X {
                            dev.dx += ev.value;
                        } else {
                            dev.dy += ev.value;
                        }
                    }
                    // evdev's wheel is one unit per notch, same as SDL's, so the ×120 wire
                    // scaling accumulator applies unchanged.
                    REL_WHEEL | REL_HWHEEL => {
                        activity.touch(&activity.discrete_ms);
                        flush_motion(dev, sink);
                        if let Some(e) = dev.scroll.scroll_event(ev.value, ev.code == REL_HWHEEL) {
                            sink(&e);
                        }
                    }
                    _ => {}
                },
                // `value == 2` is autorepeat, keyboard-only; matching 0/1 explicitly to be safe.
                EV_KEY if ev.value == 0 || ev.value == 1 => {
                    if grab_mouse && dev.mouse {
                        if let Some(button) = button_code(ev.code) {
                            activity.touch(&activity.discrete_ms);
                            // Motion first: the click must land where the pointer already is.
                            flush_motion(dev, sink);
                            sink(&mouse::raw_button_event(button, ev.value == 1));
                            continue;
                        }
                    }
                    if dev.keyboard {
                        if let Some(vk) = keyboard::vk_from_evdev(ev.code) {
                            activity.touch(&activity.key_ms);
                            sink(&keyboard::raw_key_event(vk, ev.value == 1));
                        }
                    }
                }
                _ => {}
            }
        }
        if n < buf.len() {
            break;
        }
    }
    flush_motion(dev, sink);
}

/// Sends the accumulated deltas as one `MouseMove`, undamped — damping is for the remote's
/// coarse deltas and would make a real mouse sluggish.
fn flush_motion(dev: &mut Device, sink: &impl Fn(&InputEvent)) {
    if dev.dx == 0 && dev.dy == 0 {
        return;
    }
    sink(&mouse::move_relative_event(dev.dx, dev.dy));
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
fn proc_has_hid_mouse(text: &str) -> bool {
    let mut name = "";
    let mut rel = 0u64;
    let mut abs = 0u64;
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !is_tv_builtin(name) && rel & 0b11 == 0b11 && abs & 0b11 != 0b11 {
                return true;
            }
            name = "";
            rel = 0;
            abs = 0;
            continue;
        }
        if let Some(rest) = line.strip_prefix("N: Name=") {
            name = rest.trim_matches('"');
        } else if let Some(rest) = line.strip_prefix("B: REL=") {
            rel = hex_word(rest);
        } else if let Some(rest) = line.strip_prefix("B: ABS=") {
            abs = hex_word(rest);
        }
    }
    false
}

#[cfg(test)]
fn hex_word(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .and_then(|w| u64::from_str_radix(w, 16).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{is_tv_builtin, proc_has_hid_mouse};

    const DEVICES: &str = r#"
I: Bus=0003 Vendor=0000 Product=0000 Version=0004
N: Name="LGE M-RCU - Builtin [0]"
B: REL=100
B: ABS=3

I: Bus=0005 Vendor=046d Product=b367 Version=0111
N: Name="MX MCHNCL M Keyboard"
B: REL=1040
B: ABS=1 0

I: Bus=0005 Vendor=046d Product=b015 Version=0111
N: Name="M720 Triathlon Mouse"
B: REL=1943
"#;

    #[test]
    fn m720_counts_as_a_hid_mouse_and_lg_remote_does_not() {
        assert!(proc_has_hid_mouse(DEVICES));
        assert!(!proc_has_hid_mouse(
            "N: Name=\"LGE M-RCU - Builtin [0]\"\nB: REL=100\nB: ABS=3\n\n"
        ));
        assert!(!proc_has_hid_mouse(
            "N: Name=\"MX MCHNCL M Keyboard\"\nB: REL=1040\nB: ABS=1 0\n\n"
        ));
    }

    #[test]
    fn tv_builtins_are_skipped_by_name() {
        assert!(is_tv_builtin("LGE M-RCU - Builtin [0]"));
        assert!(is_tv_builtin("LGE RCU"));
        assert!(is_tv_builtin("CHECK INPUT"));
        assert!(!is_tv_builtin("M720 Triathlon Mouse"));
        assert!(!is_tv_builtin("MX MCHNCL M Keyboard"));
    }
}
