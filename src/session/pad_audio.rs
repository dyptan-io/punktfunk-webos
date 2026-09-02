//! The `0xD1` pad-audio plane on this client: the `DualSense` voice-coil ("haptics") and speaker
//! lanes the host captures from the game's own controller-audio endpoint.
//!
//! Rendering tiers, per the plan of record: tier A writes the four channels to the pad itself,
//! tier C derives rumble from the coil lane. This module is the decode thread plus tier C — an
//! [`Envelope`] the main loop applies through the same evdev rumble route every pad already has.
//! Without it a libScePad title (Spider-Man, GTA V Enhanced…) produces **no** vibration at all on
//! this client: those games drive the coils, never the classic motors.
//!
//! Frames: kind 0 = coils, Opus 48 kHz stereo, 5 ms; kind 1 = speaker, Opus stereo, 10 ms. An
//! empty payload is the host's silence gate (deliberate silence, not loss). Lost frames are not
//! concealed here: a 5 ms hole in a rumble envelope is inaudible, and PLC is CPU this SoC lacks.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::client::NativeClient;
use punktfunk_core::quic::{PAD_AUDIO_KIND_HAPTICS, PAD_AUDIO_KIND_SPEAKER};

use crate::core::model::Settings;

/// Arrival-flag / `set_pad_audio_caps` bit: this client renders the coil lane. (Bit `0x02` is the
/// speaker lane, declared only once a transport to the pad exists.)
pub const CAP_HAPTICS: u8 = 0x01;

/// Stereo 48 kHz; a 20 ms frame is the largest Opus can carry in one packet.
const MAX_FRAME_SAMPLES: usize = 960 * 2;

/// How long the derived rumble outlives the last coil frame. Past it the main loop sends one
/// explicit zero and hands the motors back to the wire rumble plane. Matches the host's own
/// silence-gate hangover, so a gated stream and a dead one look the same from here.
const HOLD: Duration = Duration::from_millis(250);

/// Per 5 ms frame: the envelope decays to ~10% in 40 ms. Attack is instant.
const RELEASE: f32 = 0.75;

/// Fewest milliseconds between two motor writes. Each becomes an output report on the pad's
/// link, which the coil lane will share once tier A exists — 60 Hz is plenty for a motor.
const APPLY_INTERVAL: Duration = Duration::from_millis(16);

/// The render capabilities this session declares, from Settings. Speaker stays undeclared: no
/// route plays it yet, and a declared-but-silent lane would make the host stream it for nothing.
pub fn caps_for(settings: &Settings) -> u8 {
    if settings.pad_haptics {
        CAP_HAPTICS
    } else {
        0
    }
}

/// What the decode thread publishes and the main loop applies to the motors.
pub struct Envelope {
    /// `low << 16 | high`, each 0..=65535, quantised so a steady tone is not re-applied.
    levels: AtomicU32,
    /// Milliseconds since `epoch` of the last coil frame; `u64::MAX` before the first.
    last_frame_ms: AtomicU64,
    epoch: Instant,
    /// Main-loop state: the pair last written to the pad, and when.
    applied: AtomicU32,
    applied_at_ms: AtomicU64,
    /// Whether the motors currently hold a derived level (so expiry sends one zero, not many).
    owning: AtomicBool,
    pub frames: AtomicU32,
    pub speaker_frames: AtomicU32,
}

impl Envelope {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            levels: AtomicU32::new(0),
            last_frame_ms: AtomicU64::new(u64::MAX),
            epoch: Instant::now(),
            applied: AtomicU32::new(0),
            applied_at_ms: AtomicU64::new(0),
            owning: AtomicBool::new(false),
            frames: AtomicU32::new(0),
            speaker_frames: AtomicU32::new(0),
        })
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Whether coil frames are arriving: while true the wire rumble plane must not touch the
    /// motors, or a title's classic rumble (the host still forwards it) fights its own haptics.
    pub fn active(&self) -> bool {
        let last = self.last_frame_ms.load(Ordering::Relaxed);
        last != u64::MAX && self.now_ms().saturating_sub(last) < HOLD.as_millis() as u64
    }

    /// The motor pair the main loop should write now, if any: `Some((low, high))` for a changed
    /// level (rate-limited), `Some((0, 0))` once when the envelope expires, `None` otherwise.
    pub fn take_change(&self) -> Option<(u16, u16)> {
        let now = self.now_ms();
        if !self.active() {
            return self.owning.swap(false, Ordering::Relaxed).then(|| {
                self.applied.store(0, Ordering::Relaxed);
                (0, 0)
            });
        }
        let levels = self.levels.load(Ordering::Relaxed);
        if levels == self.applied.load(Ordering::Relaxed) {
            return None;
        }
        if now.saturating_sub(self.applied_at_ms.load(Ordering::Relaxed)) < APPLY_INTERVAL.as_millis() as u64 {
            return None;
        }
        self.applied.store(levels, Ordering::Relaxed);
        self.applied_at_ms.store(now, Ordering::Relaxed);
        self.owning.store(true, Ordering::Relaxed);
        Some(((levels >> 16) as u16, (levels & 0xFFFF) as u16))
    }

    fn publish(&self, low: f32, high: f32) {
        // 64 steps: below what a motor resolves, and what makes a held tone a single write.
        let q = |v: f32| ((v.clamp(0.0, 1.0) * 63.0).round() as u32) * 1040;
        self.levels.store((q(low) << 16) | q(high), Ordering::Relaxed);
        self.last_frame_ms.store(self.now_ms(), Ordering::Relaxed);
    }
}

/// Spawns the decode thread. Ends on `stop` (set at teardown before the join).
pub fn spawn(client: Arc<NativeClient>, stop: Arc<AtomicBool>, envelope: Arc<Envelope>) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("punktfunk-webos-pad-audio".into())
        .spawn(move || {
            if let Err(e) = pump(&client, &stop, &envelope) {
                tracing::warn!("pad audio thread ended: {e:#}");
            }
        })
        .context("spawn pad audio thread")
}

fn pump(client: &NativeClient, stop: &AtomicBool, envelope: &Envelope) -> Result<()> {
    let mut coils = opus::Decoder::new(48_000, opus::Channels::Stereo).map_err(|e| anyhow::anyhow!("opus decoder: {e}"))?;
    let mut pcm = vec![0f32; MAX_FRAME_SAMPLES];
    // Left coil → low (heavy) motor, right coil → high (light) motor: the pad's own left/right
    // split, and the mapping every tier-C client in the plan uses.
    let (mut low, mut high) = (0f32, 0f32);
    let mut logged_first = false;
    while !stop.load(Ordering::Relaxed) {
        let Some(frame) = client.next_pad_audio(Duration::from_millis(50)) else {
            continue;
        };
        if frame.pad != 0 {
            continue;
        }
        match frame.kind {
            PAD_AUDIO_KIND_HAPTICS => {
                let n = envelope.frames.fetch_add(1, Ordering::Relaxed) + 1;
                if !logged_first {
                    logged_first = true;
                    tracing::info!("pad audio: first coil frame (seq {}) — rendering as rumble", frame.seq);
                }
                let (peak_l, peak_r) = if frame.opus.is_empty() {
                    (0.0, 0.0)
                } else {
                    match coils.decode_float(&frame.opus, &mut pcm, false) {
                        Ok(samples) => peaks(&pcm[..samples * 2]),
                        Err(e) => {
                            tracing::debug!("pad audio: coil decode failed: {e}");
                            (0.0, 0.0)
                        }
                    }
                };
                low = (low * RELEASE).max(peak_l);
                high = (high * RELEASE).max(peak_r);
                envelope.publish(low, high);
                if n % 2000 == 0 {
                    tracing::debug!(
                        "pad audio: {n} coil frames, {} speaker frames, level {:.2}/{:.2}",
                        envelope.speaker_frames.load(Ordering::Relaxed),
                        low,
                        high
                    );
                }
            }
            PAD_AUDIO_KIND_SPEAKER => {
                // ponytail: counted only — no route to the pad's speaker yet (tier A over
                // Bluetooth is the follow-up; until then the lane is not declared either).
                envelope.speaker_frames.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Peak absolute sample per channel of interleaved stereo.
fn peaks(pcm: &[f32]) -> (f32, f32) {
    pcm.chunks_exact(2)
        .fold((0f32, 0f32), |(l, r), s| (l.max(s[0].abs()), r.max(s[1].abs())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_applies_once_then_releases_once() {
        let env = Envelope::new();
        assert_eq!(env.take_change(), None, "nothing before the first frame");
        env.publish(0.5, 0.0);
        let first = env.take_change().expect("a changed level is applied");
        assert!(first.0 > 30_000 && first.1 == 0);
        assert_eq!(env.take_change(), None, "a steady level is not re-written");
        // Expiry: force the last frame into the past.
        env.last_frame_ms.store(0, Ordering::Relaxed);
        std::thread::sleep(HOLD + Duration::from_millis(20));
        assert_eq!(env.take_change(), Some((0, 0)), "one explicit zero on expiry");
        assert_eq!(env.take_change(), None, "and only one");
    }

    #[test]
    fn peaks_split_channels() {
        assert_eq!(peaks(&[0.1, -0.9, -0.3, 0.2]), (0.3, 0.9));
    }
}
