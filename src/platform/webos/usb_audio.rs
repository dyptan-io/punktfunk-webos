//! The wired `DualSense`'s own audio card: the `0xD1` lanes without the Bluetooth machinery.
//!
//! A USB pad is a UAC1 device, and the TV's kernel enumerates it — interface 1 alt 1, isochronous
//! OUT, **4 channels S16LE at 48 kHz**, one packet per millisecond (verified on a G5, webOS 10.3).
//! The four channels are the pad's real hardware layout: ch0/ch1 are the headphone pair, of which
//! ch1 is also the internal speaker, and ch2/ch3 are the two voice coils.
//!
//! So a wired pad needs none of what [`super::dualsense`]'s Bluetooth lane is made of: no `0x36`
//! framing, no CRC, no sniff, no Luna, and **no Opus re-encode** — the host's frames decode
//! straight into the interleaved PCM the card takes. `snd_pcm_writei` blocks until the card
//! accepts, so it paces the stream and there is no tick to keep honest.
//!
//! `libasound.so.2` is on the device but `PulseAudio` mints no sink for this card, so it is opened
//! directly and resolved at runtime like every other vendor library here (see [`super::dl`]).

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

use super::dl;

const ASOUND_LIB: &CStr = c"libasound.so.2";

/// `SND_PCM_STREAM_PLAYBACK`.
const STREAM_PLAYBACK: c_int = 0;
/// `SND_PCM_FORMAT_S16_LE`.
const FORMAT_S16_LE: c_int = 2;
/// `SND_PCM_ACCESS_RW_INTERLEAVED`.
const ACCESS_RW_INTERLEAVED: c_int = 3;

/// The card's fixed shape — not negotiable, it is what the pad's UAC descriptor declares.
pub const CHANNELS: usize = 4;
const RATE: u32 = 48_000;

/// Buffer the card is asked to hold. The pad takes a packet every millisecond, so this is latency
/// the user feels in their hands: enough to ride scheduling on a 2-3 core TV, far short of the
/// ~107 ms pre-fill the Bluetooth lane needs to survive its link.
const LATENCY_US: u32 = 30_000;

type Pcm = *mut c_void;

struct Fns {
    open: unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, c_int) -> c_int,
    set_params: unsafe extern "C" fn(Pcm, c_int, c_int, u32, u32, c_int, u32) -> c_int,
    writei: unsafe extern "C" fn(Pcm, *const c_void, c_ulong) -> isize,
    prepare: unsafe extern "C" fn(Pcm) -> c_int,
    close: unsafe extern "C" fn(Pcm) -> c_int,
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

fn fns() -> Result<&'static Fns> {
    static FNS: OnceLock<std::result::Result<Fns, String>> = OnceLock::new();
    dl::cached(&FNS, ASOUND_LIB, |lib| {
        Ok(Fns {
            open: lib.sym(c"snd_pcm_open")?,
            set_params: lib.sym(c"snd_pcm_set_params")?,
            writei: lib.sym(c"snd_pcm_writei")?,
            prepare: lib.sym(c"snd_pcm_prepare")?,
            close: lib.sym(c"snd_pcm_close")?,
            strerror: lib.sym(c"snd_strerror")?,
        })
    })
}

/// ALSA's own text for a negative return code.
fn describe(fns: &Fns, rc: c_int) -> String {
    // SAFETY: `snd_strerror` returns a static NUL-terminated string for any input.
    let msg = unsafe { (fns.strerror)(rc) };
    if msg.is_null() {
        return format!("error {rc}");
    }
    // SAFETY: non-null and NUL-terminated, owned by the library.
    unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
}

/// The ALSA card index of an attached `DualSense`, if the kernel enumerated one.
///
/// `/proc/asound/cards` rather than a sysfs walk, because the jail has no `/sys/class/sound`. Each
/// card is two lines and the index leads the first; the pad is matched on its description rather
/// than on the card id, which is the generic string `Controller`.
fn find_card() -> Option<u32> {
    let text = std::fs::read_to_string("/proc/asound/cards").ok()?;
    let mut lines = text.lines();
    while let Some(head) = lines.next() {
        let detail = lines.next().unwrap_or_default();
        let names = format!("{head}\n{detail}").to_ascii_lowercase();
        if !names.contains("dualsense") {
            continue;
        }
        return head.split_whitespace().next()?.parse().ok();
    }
    None
}

/// An open playback stream on a wired pad's audio card.
pub struct PadSink {
    pcm: Pcm,
    fns: &'static Fns,
    /// The card index claimed, for the log line — indices move across replug.
    pub card: u32,
}

impl PadSink {
    /// Opens the pad's card, or `None` when no wired pad has one.
    ///
    /// Deliberately not `Send`: the raw handle belongs to whichever thread writes it, so it is
    /// opened there rather than handed over.
    pub fn open() -> Result<Self> {
        let fns = fns().context("libasound")?;
        let card = find_card().context("no DualSense audio card in /proc/asound/cards")?;
        let name = CString::new(format!("hw:{card},0"))?;
        let mut pcm: Pcm = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated and outlives the call; `pcm` is a live local.
        let rc = unsafe { (fns.open)(&raw mut pcm, name.as_ptr(), STREAM_PLAYBACK, 0) };
        if rc < 0 || pcm.is_null() {
            bail!("snd_pcm_open(hw:{card},0): {}", describe(fns, rc));
        }
        let sink = Self { pcm, fns, card };
        // SAFETY: `pcm` is the handle just opened, live until this value is dropped.
        let rc = unsafe {
            (fns.set_params)(
                sink.pcm,
                FORMAT_S16_LE,
                ACCESS_RW_INTERLEAVED,
                CHANNELS as u32,
                RATE,
                1,
                LATENCY_US,
            )
        };
        if rc < 0 {
            bail!("snd_pcm_set_params: {}", describe(fns, rc));
        }
        Ok(sink)
    }

    /// Writes one interleaved chunk: `frames * CHANNELS` samples.
    ///
    /// Recovers from an underrun rather than failing: the card stops on one, and a session that
    /// dropped a chunk should keep playing rather than lose the lane for the rest of the run.
    pub fn write(&self, interleaved: &[i16]) -> Result<()> {
        let frames = (interleaved.len() / CHANNELS) as c_ulong;
        // SAFETY: the slice is live and holds `frames * CHANNELS` samples of the format set above.
        let n = unsafe { (self.fns.writei)(self.pcm, interleaved.as_ptr().cast(), frames) };
        if n >= 0 {
            return Ok(());
        }
        // SAFETY: the handle is live; `prepare` is the documented recovery for -EPIPE.
        let rc = unsafe { (self.fns.prepare)(self.pcm) };
        if rc < 0 {
            bail!("snd_pcm_writei: {}", describe(self.fns, n as c_int));
        }
        Ok(())
    }
}

impl Drop for PadSink {
    fn drop(&mut self) {
        // SAFETY: `pcm` came from `snd_pcm_open` and is closed exactly once.
        unsafe { (self.fns.close)(self.pcm) };
    }
}

/// Whether a wired pad's audio card is present, without opening it — the caller needs this before
/// declaring the speaker lane to the host.
pub fn card_present() -> bool {
    find_card().is_some()
}

/// Frames per write: 5 ms. Short enough that a lane starting mid-chunk is not audibly late,
/// long enough that the write rate is nowhere near the pad's per-millisecond packet rate.
const CHUNK_FRAMES: usize = 240;

/// Spawns the writer for a wired pad's `0xD1` lanes, or `None` when the card cannot be opened.
///
/// `snd_pcm_writei` blocks until the card takes the chunk, so it is the clock: there is no tick to
/// keep on the pad's phase and nothing to overfeed, which is the whole difficulty of the Bluetooth
/// lane gone. Silence is written when both lanes are quiet rather than stopping, so the stream
/// never has to restart mid-effect.
pub fn spawn(
    envelope: std::sync::Arc<crate::session::pad_audio::Envelope>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("pad-usb-audio".into())
        .spawn(move || {
            let sink = match PadSink::open() {
                Ok(sink) => {
                    tracing::info!("pad audio: wired lanes on the pad's own card (hw:{},0)", sink.card);
                    sink
                }
                Err(e) => {
                    tracing::warn!("pad audio: wired card unavailable ({e:#}); coils stay on the motors");
                    return;
                }
            };
            // Claimed only once the card is actually open: a failed open must leave the motor
            // envelope holding the coils rather than park it for a lane that never plays.
            envelope.own_usb();
            // Route the pad's audio to its speaker. Its own handle rather than the feedback
            // thread's: routing belongs to whoever plays the lane, and the pad may have no
            // feedback sender at all (a host that sends no effects still sends audio).
            match super::hidraw::Hidraw::find_dualsense() {
                Some(node) => {
                    if let Err(e) = node.write_report(&super::dualsense::build_usb_speaker_setup()) {
                        tracing::warn!("pad audio: speaker routing not set ({e:#}); expect the jack, not the speaker");
                    }
                }
                None => tracing::warn!("pad audio: no hidraw node to route the speaker with"),
            }

            let mut speaker = vec![0f32; CHUNK_FRAMES * 2];
            let mut coils = vec![0f32; CHUNK_FRAMES * 2];
            let mut out = vec![0i16; CHUNK_FRAMES * CHANNELS];
            let mut failing = false;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                envelope.take_speaker_pcm(&mut speaker);
                envelope.take_coils_pcm(&mut coils);
                let q = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                for (i, frame) in out.chunks_exact_mut(CHANNELS).enumerate() {
                    // ch0/ch1 are the headphone pair, ch1 doubling as the internal speaker;
                    // ch2/ch3 are the coils. The pad's own hardware layout, not a choice.
                    frame[0] = q(speaker[i * 2]);
                    frame[1] = q(speaker[i * 2 + 1]);
                    frame[2] = q(coils[i * 2]);
                    frame[3] = q(coils[i * 2 + 1]);
                }
                match sink.write(&out) {
                    Ok(()) => failing = false,
                    Err(e) => {
                        if !failing {
                            tracing::warn!("pad audio: wired write failed (further errors quiet): {e:#}");
                            failing = true;
                        }
                    }
                }
            }
        })
        .ok()
}
