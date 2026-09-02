//! Adaptive triggers, lightbar and player LEDs on a real `DualSense`, over webOS's
//! Bluetooth HID plane.
//!
//! **Why this route exists.** SDL's own `DualSense` support (`SDL_GameControllerSendEffect`,
//! present in the bundled fork) drives the pad through `/dev/hidraw*`, and a webOS app's jail
//! exposes no hidraw node at all — not even with the pad connected, and with no `hidraw`
//! class in `/sys` either (verified on webOS 10.3, non-rooted; see `docs/NOTES.md`). So the
//! effect bytes cannot go through SDL. What *is* reachable is the TV's own Bluetooth stack:
//! `com.webos.service.bluetooth2/hid/internal/sendData` writes an arbitrary HID output report
//! to a connected HID device, and it sits in the `public` API group a dev-mode install
//! already holds — no root required.
//!
//! Two hard-won details, both verified on-device:
//!   * the payload carries `reportData` as an int array **with no `reportId` field** — adding
//!     one makes the service reject the call with a generic schema error that names nothing,
//!     and `setReport` (the method that *does* take a report id) always answers "operation can
//!     not be performed at this time";
//!   * the report needs the same CRC32 the kernel's `hid-playstation` appends, over a `0xA2`
//!     seed byte. Without it the pad silently ignores a call the service reports as success.
//!
//! Rumble deliberately does **not** go through here: it reaches the pad as ordinary evdev
//! force feedback via SDL (`GameController::set_rumble`), which works for every controller
//! type rather than only this one. Reports built here never set the vibration valid-flag, so
//! they cannot fight the kernel's force-feedback state — see [`build_report`].
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use punktfunk_core::quic::HidOutput;

use crate::session::pad_audio::{Envelope, COIL_REPORT_FRAMES};

/// `hid/internal/sendData` — the only one of the three HID methods that works. `getReport`
/// hangs on a pad that doesn't answer; `setReport` refuses with error 4 whatever the payload.
const SEND_DATA_URI: &str = "luna://com.webos.service.bluetooth2/hid/internal/sendData";
/// Takes the pad's link out of Bluetooth sniff mode. In sniff, the stack delivers output reports
/// in bursts at the sniff anchor points; that starved the pad's audio buffer between bursts and
/// made every speaker layout choppy until this call — the single fix that made audio continuous
/// (G5, webOS 10.3). `public` group, like `sendData`. Its counterpart re-enters sniff on release.
const STOP_SNIFF_URI: &str = "luna://com.webos.service.bluetooth2/device/internal/stopSniff";
const START_SNIFF_URI: &str = "luna://com.webos.service.bluetooth2/device/internal/startSniff";

/// Bluetooth `DualSense` output report, per Linux `hid-playstation`'s
/// `dualsense_output_report_bt`: `0x31`, seq/tag, tag, the 47-byte common block, 24 reserved
/// bytes, then the CRC32. Exactly this length was verified accepted end-to-end.
const REPORT_LEN: usize = 78;
/// Where the 47-byte common block starts (after report id, `seq_tag`, tag).
const COMMON: usize = 3;

/// Bluetooth `0x32` coil report: id, seq, a 7-byte `0x11` session sub-packet, the 64-byte `0x12`
/// coil sub-packet (32 stereo s8 frames at 3 kHz), padding, CRC32. Verified on a G5: the pad
/// buzzes, with the SAxense flag convention (`tag | 0x80`) and the same CRC as `0x31`.
const COIL_REPORT_LEN: usize = 142;
/// One coil report per 512 samples at 48 kHz — the pad's clock. 10 ms (6.7% fast) overruns it.
const COIL_TICK: Duration = Duration::from_nanos(10_666_667);

/// `valid_flag0` bit: the right (R2) trigger effect block is meaningful in this report.
const FLAG0_RIGHT_TRIGGER: u8 = 0x04;
/// `valid_flag0` bit: the left (L2) trigger effect block is meaningful in this report.
const FLAG0_LEFT_TRIGGER: u8 = 0x08;
/// `valid_flag1` bit: take over the lightbar colour.
const FLAG1_LIGHTBAR: u8 = 0x04;
/// `valid_flag1` bit: drive the five player-indicator LEDs.
const FLAG1_PLAYER_LEDS: u8 = 0x10;

/// Common-block offset of the right trigger's effect block.
const OFF_RIGHT_TRIGGER: usize = 10;
/// Common-block offset of the left trigger's effect block.
const OFF_LEFT_TRIGGER: usize = 21;
/// Common-block offset of the player-indicator LED bits.
const OFF_PLAYER_LEDS: usize = 43;
/// Common-block offset of lightbar red; green and blue follow it.
const OFF_LIGHTBAR_RED: usize = 44;

/// A `DualSense` trigger parameter block: effect mode byte plus up to ten parameters. Sized to
/// match `punktfunk_core::abi::PUNKTFUNK_HID_EFFECT_MAX`, which is what the host forwards
/// from the game — so a block copies straight in with no reinterpretation.
const EFFECT_LEN: usize = 11;
type Effect = [u8; EFFECT_LEN];

/// Everything this client drives on the pad, as absolute state rather than deltas.
///
/// Absolute on purpose: one report carries all of it, so re-sending the whole struct makes
/// every update idempotent and self-healing. The host's feedback plane is lossy by design
/// (newest-drops on overflow, no retransmit), and a dropped *delta* would leave a trigger
/// stuck resisting until the game happened to change it again.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct State {
    /// `None` until a game sets a colour — the system keeps painting the lightbar until then.
    lightbar: Option<(u8, u8, u8)>,
    /// `None` until a game sets them, for the same reason as `lightbar`.
    player_leds: Option<u8>,
    /// Mode `0x00` (the default) means "no resistance", so the zero value is already correct.
    right_trigger: Effect,
    left_trigger: Effect,
    /// Whether to assert the trigger valid-flags at all. Travels *with* the state rather than
    /// being re-derived when sending: an explicit release is mode `0x00` on both triggers,
    /// which is indistinguishable from "never touched" by looking at the bytes — deriving it
    /// would drop the one report that lets go of a stiffened trigger.
    triggers_owned: bool,
}

/// CRC32 (IEEE 802.3, reflected) — the `crc32_le` variant `hid-playstation` signs Bluetooth
/// output reports with. Computed bitwise: 78 bytes per report at human-paced update rates
/// isn't worth a 1 KiB lookup table. Over an iterator so the seed byte the signature covers
/// chains onto the report without copying it into a buffer first.
fn crc32_le(bytes: impl IntoIterator<Item = u8>) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            // 0xEDB88320 = reflected 0x04C11DB7.
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The pad's Bluetooth address, in the form `hid/internal/sendData`'s `address` wants.
///
/// Read from `/proc/bus/input/devices`' `U: Uniq=` line, which `hid-playstation` sets to the
/// pad's MAC. A `DualSense` publishes three input devices (pad, motion sensors, touchpad) all
/// sharing one `Uniq`, so the first match is the right one. `None` for a USB-connected pad
/// (no `Uniq`), which is correct — this path is Bluetooth-only.
pub fn find_address() -> Option<String> {
    let devices = std::fs::read_to_string("/proc/bus/input/devices").ok()?;
    // Bound rather than returned directly: the iterator borrows `devices`, and a tail-position
    // temporary outlives the local it borrows from.
    let address = dualsense_blocks(&devices).find_map(|block| {
        block
            .lines()
            .find_map(|l| l.strip_prefix("U: Uniq="))
            .map(|u| u.trim().to_ascii_lowercase())
            .filter(|u| !u.is_empty())
    });
    address
}

/// The `/proc/bus/input/devices` records of every connected `DualSense`, matched on the `N: Name=`
/// line. One pad publishes three of them, and only some carry the field a caller wants.
fn dualsense_blocks<'a>(devices: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    devices.split("\n\n").filter(|block| {
        block
            .lines()
            .any(|l| l.starts_with("N: Name=") && l.to_ascii_lowercase().contains("dualsense"))
    })
}

/// Whether an attached `DualSense`/`Edge` is bound to the kernel's `hid-playstation` driver
/// (registered as `playstation`) rather than falling back to `hid-generic`.
///
/// The Bluetooth output report this module builds is a `hid-playstation` behavior, not a
/// property of the `luna` call: on a TV whose kernel never got that driver backported, the pad
/// still pairs and shows `connectedProfiles: ["hid"]`, and `hid/internal/sendData` still answers
/// `returnValue: true` for every report — but nothing reaches the pad, silently. Verified on a
/// webOS 5/6-class set (kernel 4.4.84): `/sys/bus/hid/devices/0005:054C:0CE6.0002/driver` links to
/// `hid-generic`, and neither the lightbar nor the player LEDs moved for a report identical to
/// the one confirmed working on webOS 10.3. This is the caption Settings shows for that case.
///
/// Answered from a short-lived cache. The Settings screen reads this while building its rows,
/// which the render pass does every tick the modal animates — and the uncached answer is two
/// filesystem reads. A pad binding changes only when one is plugged in or out, so a second of
/// staleness is invisible and 60 reads a second are not.
pub fn hid_playstation_bound() -> bool {
    /// How long a probe's answer stands. Human-scale: a hotplug shows up in the caption within
    /// one refresh, and nothing else in the app reacts faster than that.
    const TTL: Duration = Duration::from_secs(1);
    static CACHED: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

    let mut cached = CACHED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, bound)) = *cached {
        if at.elapsed() < TTL {
            return bound;
        }
    }
    let bound = probe_hid_playstation_bound();
    *cached = Some((Instant::now(), bound));
    bound
}

/// The uncached probe behind [`hid_playstation_bound`].
fn probe_hid_playstation_bound() -> bool {
    let devices = std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default();
    // Bound for the same reason as in `find_address`.
    let bound = dualsense_blocks(&devices)
        // The evdev node's `Sysfs=` line points at `<hid-device>/input/inputN`; the driver
        // binding lives on the HID device itself, one level up.
        .filter_map(|block| block.lines().find_map(|l| l.strip_prefix("S: Sysfs=")))
        .any(|sysfs| {
            let device_dir = sysfs.split("/input/input").next().unwrap_or(sysfs);
            std::fs::read_link(format!("/sys{device_dir}/driver"))
                .is_ok_and(|d| d.file_name().is_some_and(|f| f == "playstation"))
        });
    bound
}

/// Owns the pad's feedback state and the thread that ships it.
///
/// The thread exists because a send is a fork/exec of `luna-send-pub` (see [`crate::platform::webos::luna`]),
/// which must never land on the render/input loop. Queue depth is one with latest-wins
/// replacement — the same discipline as [`crate::services::store::StateWriter`] — because the state
/// is absolute: a superseded update carries no information the newer one lacks.
pub struct Feedback {
    state: State,
    /// `None` only after [`Drop`] has taken it to close the channel.
    tx: Option<SyncSender<State>>,
    /// `None` only after [`Drop`] has taken and joined it.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Feedback {
    /// Starts a feedback sender for the pad at Bluetooth `address`, or `None` when the
    /// platform can't support it (no `luna-send-pub`). Resolve `address` via [`find_address`].
    pub fn new(address: String, coils: Option<Arc<Envelope>>) -> Option<Self> {
        if !crate::platform::webos::luna::available() {
            return None;
        }
        // Depth 1 + `try_send`: at most one pending state, and a full queue means the sender
        // is mid-call — that pending value is stale by definition, so the next update replaces
        // it rather than queueing behind it.
        let (tx, rx) = std::sync::mpsc::sync_channel::<State>(1);
        let thread = std::thread::Builder::new()
            .name("ds5-feedback".into())
            .spawn(move || sender_loop(&address, &rx, coils))
            .ok()?;
        tracing::info!("DualSense feedback active (adaptive triggers, lightbar)");
        Some(Self {
            state: State::default(),
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    /// Folds one host feedback event into the pad state and queues a send.
    ///
    /// Variants this pad has no route for are dropped: `TrackpadHaptic` is a Steam
    /// Controller voice-coil buzz, and `HidRaw` is a passthrough report for a device the host
    /// mirrors as-is — replaying either on a `DualSense` would mean writing arbitrary bytes in
    /// the wrong protocol. `AudioCtl` is the routing/volume half of pad audio, which this client
    /// never asks for (it advertises no `CLIENT_CAP_PAD_AUDIO`, so no host sends it) and could
    /// not honour anyway: the feedback path here is the Bluetooth service's state model, not a
    /// hidraw node, so there is nothing to write a DS5 output report to.
    pub fn apply(&mut self, event: &HidOutput) {
        match event {
            HidOutput::Led { r, g, b, .. } => self.state.lightbar = Some((*r, *g, *b)),
            HidOutput::PlayerLeds { bits, .. } => self.state.player_leds = Some(*bits),
            HidOutput::Trigger { which, effect, .. } => {
                let mut block = Effect::default();
                let n = effect.len().min(EFFECT_LEN);
                block[..n].copy_from_slice(&effect[..n]);
                // `which`: 0 = L2, 1 = R2 (`punktfunk_core::quic::HidOutput`).
                if *which == 0 {
                    self.state.left_trigger = block;
                } else {
                    self.state.right_trigger = block;
                }
                self.state.triggers_owned = true;
            }
            HidOutput::TrackpadHaptic { .. } | HidOutput::HidRaw { .. } | HidOutput::AudioCtl { .. } => return,
        }
        // Dropping on a full queue *is* the coalescing: the waiting value is strictly older.
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(self.state);
        }
    }

    /// Releases everything this client took over — both triggers back to no resistance, the
    /// lightbar handed back to the system.
    ///
    /// Trigger resistance lives in the pad's firmware, not in the stream, so without this a
    /// game that left R2 stiff leaves it stiff on the TV's home screen and after the app
    /// exits, with nothing to connect that to punktfunk. Blocking send, unlike
    /// [`apply`](Self::apply): this one must not be the update that gets coalesced away, and
    /// [`Drop`] joins the sender right after so it is actually delivered.
    ///
    /// Called on the way out of a stream, so it delays return-to-menu by one send — tens of
    /// milliseconds normally, and at worst two [`crate::platform::webos::luna::CALL_TIMEOUT`] windows if the
    /// Bluetooth service has stopped answering.
    pub fn release(&mut self) {
        self.state = State {
            triggers_owned: true, // assert mode 0x00 explicitly rather than staying silent
            ..State::default()
        };
        if let Some(tx) = &self.tx {
            let _ = tx.send(self.state);
        }
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        // Closing the channel ends `sender_loop`; joining lets a send in flight (and anything
        // `release` just queued) complete — the same reason `StateWriter` joins its writer.
        drop(self.tx.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Builds one Bluetooth output report for `state`.
///
/// Note which valid-flags are never set: not `0x01` (compatible vibration), so the motor
/// bytes stay ignored and this report cannot cancel rumble that SDL's evdev force-feedback
/// path is driving independently.
fn build_report(seq: u8, state: &State) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[0] = 0x31; // Bluetooth output report id
    r[1] = (seq & 0x0F) << 4; // high nibble = sequence, low nibble = tag mask (0)
    r[2] = 0x10; // DS_OUTPUT_TAG
    if state.triggers_owned {
        r[COMMON] = FLAG0_RIGHT_TRIGGER | FLAG0_LEFT_TRIGGER;
        r[COMMON + OFF_RIGHT_TRIGGER..][..EFFECT_LEN].copy_from_slice(&state.right_trigger);
        r[COMMON + OFF_LEFT_TRIGGER..][..EFFECT_LEN].copy_from_slice(&state.left_trigger);
    }
    if let Some((red, green, blue)) = state.lightbar {
        r[COMMON + 1] |= FLAG1_LIGHTBAR;
        r[COMMON + OFF_LIGHTBAR_RED] = red;
        r[COMMON + OFF_LIGHTBAR_RED + 1] = green;
        r[COMMON + OFF_LIGHTBAR_RED + 2] = blue;
    }
    if let Some(bits) = state.player_leds {
        r[COMMON + 1] |= FLAG1_PLAYER_LEDS;
        r[COMMON + OFF_PLAYER_LEDS] = bits & 0x1F;
    }
    // CRC32 over a 0xA2 seed byte (the HIDP DATA/Output header the stack prepends) followed
    // by everything ahead of the CRC field itself.
    let signed = std::iter::once(0xA2).chain(r[..REPORT_LEN - 4].iter().copied());
    let crc = crc32_le(signed);
    r[REPORT_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
    r
}

/// `reportData` as a bare int array — **no `reportId` key**. The service validates the whole
/// object against a strict schema, so one extra property fails the call outright.
fn payload_for(address: &str, report: &[u8]) -> String {
    let mut payload = String::with_capacity(report.len() * 4 + 64);
    let _ = write!(payload, "{{\"address\":\"{address}\",\"reportData\":[");
    for (i, b) in report.iter().enumerate() {
        let _ = if i == 0 {
            write!(payload, "{b}")
        } else {
            write!(payload, ",{b}")
        };
    }
    payload.push_str("]}");
    payload
}

fn send_report(address: &str, report: &[u8; REPORT_LEN]) -> anyhow::Result<()> {
    crate::platform::webos::luna::call(SEND_DATA_URI, &payload_for(address, report))
}

/// Builds one Bluetooth coil report: `frames` are 32 stereo s8 samples at 3 kHz.
///
/// Sub-packet layout per `awalol/DS5Dongle` / `egormanga/SAxense`: `[tag | 0x80][len][payload]`.
/// The `0x11` session packet carries a free-running `counter`; its other bytes are what every
/// working implementation sends and mean nothing documented.
fn build_coil_report(seq: u8, counter: u8, frames: &[[i8; 2]; COIL_REPORT_FRAMES]) -> [u8; COIL_REPORT_LEN] {
    let mut r = [0u8; COIL_REPORT_LEN];
    r[0] = 0x32;
    r[1] = (seq & 0x0F) << 4;
    r[2] = 0x11 | 0x80;
    r[3] = 7;
    r[4] = 0xFE;
    r[9] = 0xFF;
    r[10] = counter;
    r[11] = 0x12 | 0x80;
    r[12] = (COIL_REPORT_FRAMES * 2) as u8;
    for (i, [l, right]) in frames.iter().enumerate() {
        r[13 + i * 2] = *l as u8;
        r[14 + i * 2] = *right as u8;
    }
    let signed = std::iter::once(0xA2).chain(r[..COIL_REPORT_LEN - 4].iter().copied());
    let crc = crc32_le(signed);
    r[COIL_REPORT_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
    r
}

/// Spacing between sends on the in-process bus ([`crate::platform::webos::ls2`]): no spawn per
/// report, so this only bounds how fast a host animating the lightbar can push reports through
/// the Bluetooth service. Well inside what the pad accepts (audio runs at 10.7 ms per report).
const BUS_SEND_INTERVAL: Duration = Duration::from_millis(16);

/// Minimum wall time between two sends.
///
/// A send is a fork/exec of `luna-send-pub` (see [`crate::platform::webos::luna`]), which copies the page
/// tables of a process holding SDL, the decoder and its frame buffers. **Unthrottled, this
/// blacked out the video plane on a real stream**: a Steam/Gamescope host animates the
/// `DualSense` lightbar continuously, which turned every animation step into a process spawn —
/// dozens per second on a 2-3 core TV. Frames kept decoding (that thread is priority-boosted)
/// while the compositor never got to present, so the panel stayed black with the frame counter
/// climbing and nothing dropped.
///
/// 250 ms is far finer than a trigger effect meaningfully changes (weapon swaps, state
/// transitions) and caps the cost at four spawns a second in the worst case.
const MIN_SEND_INTERVAL: Duration = Duration::from_millis(250);

/// Drains queued states, sending each. Ends when the channel closes (`Feedback` dropped).
///
/// Two guards keep the spawn rate down, both necessary: identical states are dropped outright
/// (a host re-asserting the same lightbar colour costs nothing), and the remainder are spaced
/// by [`MIN_SEND_INTERVAL`]. Sleeping *after* a send rather than dropping the update is what
/// makes the throttle lossless — the depth-1 channel keeps replacing the pending state while
/// this thread waits, so the newest one goes out next.
///
/// One log per run of failures, not per send: if the Bluetooth service stops accepting (pad
/// powered off mid-session) every later update would otherwise repeat the same line for as
/// long as the game keeps changing effects.
fn sender_loop(address: &str, rx: &Receiver<State>, coils: Option<Arc<Envelope>>) {
    // In-process bus first: one function call per report instead of a spawn. Its failure is the
    // normal state on a TV whose hub refuses the registration, and the spawn route still works
    // there — so it is logged as information and the throttle stays at the spawn-safe value.
    let bus = match crate::platform::webos::ls2::Bus::open() {
        Ok(bus) => {
            tracing::info!("DualSense feedback: in-process Luna bus");
            Some(bus)
        }
        Err(e) => {
            tracing::info!("DualSense feedback: in-process Luna bus unavailable ({e:#}); using luna-send-pub");
            None
        }
    };
    // The coil lane needs the bus: ~94 reports a second is not a spawn rate. Claiming it here
    // parks the motor envelope for the session (`Envelope::own_coils`).
    let lane = match (&bus, coils) {
        (Some(bus), Some(envelope)) => {
            // Un-burst the link before the first coil report; the reply is counted like any
            // other (a refusal shows up once in the log through `REPLIES`).
            let payload = format!("{{\"address\":\"{address}\"}}");
            match bus.call(STOP_SNIFF_URI, &payload) {
                Ok(()) => tracing::info!("DualSense audio haptics: coil lane over the Luna bus, sniff stopped"),
                Err(e) => tracing::warn!("DualSense audio haptics: stopSniff refused ({e:#}); expect bursts"),
            }
            envelope.own_coils();
            Some(envelope)
        }
        _ => None,
    };
    let interval = if bus.is_some() { BUS_SEND_INTERVAL } else { MIN_SEND_INTERVAL };
    let mut failing = false;
    let mut last_sent: Option<State> = None;
    let mut last_sent_at: Option<Instant> = None;
    let mut pending: Option<State> = None;
    let mut sends: u32 = 0;
    // The pad expects a changing sequence number per report; low 4 bits, so it wraps freely.
    // Shared by state and coil reports: it is per link, not per report kind.
    let mut seq: u8 = 0;
    let mut next_tick = Instant::now() + COIL_TICK;
    let mut counter: u8 = 0;
    let mut frames = [[0i8; 2]; COIL_REPORT_FRAMES];
    let mut coil_sends: u32 = 0;
    loop {
        // A state update, or the coil tick — whichever is first. Without a lane the wait is
        // unbounded, as before; the spawn route also sleeps between sends, which the lane cannot.
        let received = match &lane {
            Some(_) => match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(state) => Some(state),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(state) => Some(state),
                Err(_) => break,
            },
        };
        if let Some(state) = received {
            if last_sent != Some(state) {
                pending = Some(state);
            }
        }
        let due = last_sent_at.is_none_or(|at| at.elapsed() >= interval);
        if let (Some(state), true) = (pending, due) {
            pending = None;
            seq = seq.wrapping_add(1);
            let report = build_report(seq, &state);
            let sent = match &bus {
                Some(bus) => bus.call(SEND_DATA_URI, &payload_for(address, &report)),
                None => send_report(address, &report),
            };
            match sent {
                Ok(()) => {
                    failing = false;
                    last_sent = Some(state);
                    last_sent_at = Some(Instant::now());
                    sends += 1;
                    // Rate visible in the log without one line per send — the symptom the
                    // spawn throttle exists for is invisible from the client's own counters.
                    if sends % 50 == 0 {
                        tracing::debug!("DualSense feedback: {sends} reports sent");
                    }
                }
                Err(e) => {
                    if !failing {
                        tracing::warn!("DualSense feedback send failed (further errors quiet): {e}");
                        failing = true;
                    }
                }
            }
            if bus.is_none() {
                std::thread::sleep(interval);
            }
        }
        if let (Some(envelope), Some(bus)) = (&lane, &bus) {
            let now = Instant::now();
            if now >= next_tick {
                next_tick += COIL_TICK;
                if next_tick + COIL_TICK < now {
                    // Stalled (a slow bus call, scheduling): resync rather than burst to catch up.
                    next_tick = now + COIL_TICK;
                }
                // Keep the cadence through the hold window after the last frame — zeros, so the
                // pad's buffer runs out cleanly rather than looping its tail — then go quiet.
                let had_data = envelope.take_coils(&mut frames);
                if had_data || envelope.active() {
                    seq = seq.wrapping_add(1);
                    counter = counter.wrapping_add(1);
                    let report = build_coil_report(seq, counter, &frames);
                    if let Err(e) = bus.call(SEND_DATA_URI, &payload_for(address, &report)) {
                        if !failing {
                            tracing::warn!("DualSense audio haptics send failed (further errors quiet): {e}");
                            failing = true;
                        }
                    } else {
                        coil_sends += 1;
                        if coil_sends % 940 == 0 {
                            tracing::debug!("DualSense audio haptics: {coil_sends} coil reports");
                        }
                    }
                }
            }
        }
        if let Some(bus) = &bus {
            bus.pump();
            // A refused reply is asynchronous: surface the first one per run of failures.
            if let Some(text) = crate::platform::webos::ls2::REPLIES.take_failure() {
                tracing::warn!("DualSense feedback: Bluetooth service refused a report: {text}");
            }
        }
    }
    // Give the link back to sniff: the TV's own power policy, and what the pad expects at idle.
    if let (Some(bus), Some(_)) = (&bus, &lane) {
        let payload = format!("{{\"address\":\"{address}\"}}");
        let _ = bus.call(START_SNIFF_URI, &payload);
        bus.pump();
    }
}
