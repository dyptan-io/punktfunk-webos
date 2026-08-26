//! NDL `DirectMedia` **v2** (webOS 5+): `NDL_DirectMediaLoad` plus
//! `NDL_DirectVideoPlay(buffer, size, pts)`, a render-buffer query, a flush and HDR mastering
//! metadata. The path every currently-working TV takes.
//!
//! Never calls `NDL_DirectVideoSetArea` — stutters above 1080p, and v2 sizes its own
//! punch-through plane (v1 can't; see [`super::v1`]).
use std::ffi::c_uint;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::core::media::{AudioFormat, AudioPlane, AudioSink, MediaClock, NotReady, Samples, VideoSink, VideoSinkCaps};

use super::{arm_load, ensure_init, ensure_not_poisoned, ffi, settle_before_retry, wait_load_completed};
use super::{lock_ffi, mark_frame_fed_logged, NdlCodec, LOAD_COMPLETED};

/// How long past the `NDL_DirectMediaLoad` CALL [`NdlVideo::ensure_loaded`] holds frames while
/// `LOADCOMPLETED` is missing.
///
/// Measured from `load_requested`, not `load_instant`: the latter is stamped after
/// `wait_load_completed`, so timing the grace from it stacks the two windows and a unit whose
/// callback never comes eats `LOAD_COMPLETE_TIMEOUT` + this before the first frame — three seconds
/// of black on a CX, for a callback that provably was not going to arrive. Overlapping them means
/// the wait alone already spends the grace, so the first feed goes straight through.
///
/// **A backstop, not a live path.** `load()` already waits `LOAD_COMPLETE_TIMEOUT` (longer than
/// this), so the first `ensure_loaded` always feeds and [`NotReady`] is never constructed.
/// Kept because it is what would make an early return from that wait safe.
const FEED_ANYWAY_AFTER: Duration = Duration::from_millis(1_000);

/// One empty Opus frame — `mariotaku/ss4s`'s `opus_empty_frame_211`. Its TOC declares STEREO,
/// matching the load; the generic `0xF8 0xFF 0xFE` declares mono. (A CX took both.)
const OPUS_SILENCE: [u8; 3] = [0xec, 0xff, 0xfe];

/// Packet duration of the prime's stamps (ms), matching the real audio plane's 48 kHz / 5 ms
/// (`SAMPLE_RATE` in `platform::webos::audio`).
const PRIME_PACKET_MS: i64 = 5;

/// How far ahead of wall-clock the prime's stamps may run, in packets — a burst big enough to
/// configure a decoder, and the bound on how far its `last_audio_pts_ms` ceiling overshoots.
const PRIME_LEAD: i64 = 8;

/// Lead the REAL stream's stamps carry over the player clock, i.e. the audio queue depth NDL is
/// left holding — [`PRIME_LEAD`] packets, the same depth the metronome maintains while it is the
/// only feed.
///
/// **This is not a sync tweak, it is what keeps the picture paced.** NDL regulates the video plane
/// against its audio renderer, and the renderer's clock only advances smoothly while it has data
/// queued ahead of it. Fed straight off the wire the real stream stamps at ≈ the player clock (a
/// packet arrives *after* the frame it was captured with, and the PTS trim pulled the shared offset
/// another ~36 ms earlier), so the renderer runs at the edge of underrun and the picture stutters
/// on network jitter — the exact failure the clock plane was introduced to fix, back again the
/// moment real audio displaced the metronome. Adding a constant here restores the depth without
/// interleaving silence, which would raise the ceiling real packets then floor onto (see
/// [`NdlVideo::play_audio`] — that is a permanent session mute, not a stutter).
///
/// **The SDL path does the same thing, in its own currency** — `platform::webos::audio` primes and
/// holds a 25 ms ring ahead of the speaker for exactly this reason: a renderer needs data queued
/// ahead of it. NDL takes no depth argument, so the only way to ask it for one is a stamp in the
/// future, which is this. Note what neither path does: correct the resulting lip sync. The A/V
/// offset is measured and published, never steered on.
///
/// The cost is lip sync: sound lands this far behind the picture. The PTS trim already moved the
/// picture ~36 ms earlier, so it roughly cancels — walk the value down on device against
/// `plane_lead` in the video heartbeat, which is the only place the depth is observable.
const PLANE_LEAD_MS: i64 = PRIME_LEAD * PRIME_PACKET_MS;

/// Gap between prime bursts. Polled through, not slept through — the callback lands mid-gap,
/// and this is launch-path time, i.e. black screen.
const PRIME_RETRY: Duration = Duration::from_millis(20);

/// Re-latch skew past which lip sync is audibly off and the jump that caused it is worth a line.
const SKEW_WARN_MS: i64 = 200;

/// Real packets dropped for want of a latched timeline between warnings — 200 × 5 ms = 1 s.
const NO_OFFSET_WARN_PACKETS: u32 = 200;

/// How long the clock plane waits for the real stream before feeding the plane itself.
///
/// The test is "no packets at all", never amplitude: a silent game still streams, since the host
/// encodes silence into the same continuous 5 ms datagrams. Only a dead host capture gaps this wide.
const REAL_FEED_GRACE_MS: i64 = 300;

/// The audio plane every V2 load asks for: Opus, stereo, 48 kHz.
///
/// **Every accepted V2 load asks for a plane** — NDL only paces the picture against a fed audio
/// plane (docs/NOTES.md § "NDL's audio plane"). The offload route puts the wire's own Opus on it;
/// every other session runs [`NdlVideo::run_clock_plane`]'s metronome instead, and the silent
/// frame's TOC declares stereo either way, so the config is the same one.
fn plane_config() -> ffi::AudioUnion {
    ffi::AudioOpusInfo {
        kind: 3, // NDL_AUDIO_TYPE_OPUS
        unknown1: 0,
        channels: 2,
        unknown2: 0,
        // kHz, not Hz — NDL's own unit, and what ss4s passes (`info->sampleRate / 1000.0`).
        sample_rate: 48.0,
        stream_header: std::ptr::null(),
        _padding: [0; 4],
    }
    .to_union()
}

/// One loaded NDL v2 video decode session. Dropping unloads it (not `NDL_DirectMediaQuit`).
pub struct NdlVideo {
    fns: &'static ffi::V2,
    /// PTS in ms since load (NDL's local clock, not wall-clock or host capture clock).
    load_instant: Instant,
    /// When `NDL_DirectMediaLoad` was issued — earlier than `load_instant` by however long
    /// `wait_load_completed` blocked. Only [`FEED_ANYWAY_AFTER`] is measured from it; the PTS
    /// domain stays on `load_instant`.
    load_requested: Instant,
    /// Whether this load got an audio plane. False on a video-only load, i.e. one whose
    /// audio-enabled attempt was rejected — and with it goes the picture's pacing reference.
    audio: bool,
    /// The session's shared host-PTS → player-clock mapping, attached by the video stage before
    /// anything is fed (`core::media::SessionClock`). Both planes stamp through it, which is the
    /// whole reason it is one object rather than two agreeing copies.
    clock: std::sync::OnceLock<std::sync::Arc<crate::core::media::SessionClock>>,
    /// Mapping epoch the audio thread has already derived its skew for — see
    /// [`Self::derive_audio_skew`]. Re-derivation runs on the audio thread's own next packet
    /// rather than on the video thread that latched, which keeps `lock_ffi` out of the picture's
    /// way on every re-anchor.
    skew_epoch: AtomicU64,
    /// Highest audio stamp fed so far (ms), so [`Self::play_audio`] can never hand NDL a
    /// timestamp going backwards. Never reset — the ceiling has to survive a re-latch, which is
    /// exactly the case that would otherwise rewind it.
    last_audio_pts_ms: AtomicI64,
    /// Constant added to every mapped real-audio stamp, re-derived on each latch
    /// ([`Self::derive_audio_skew`]) so a resumed run lands above [`Self::last_audio_pts_ms`]
    /// rather than flooring onto it — see [`Self::play_audio`].
    audio_skew_ms: AtomicI64,
    /// Set by [`Self::run_clock_plane`] on every silence burst so the next real packet re-derives
    /// its skew instead of flooring onto the moved ceiling. Consumed alongside a mapping-epoch
    /// change in [`Self::play_audio`], which needs the same correction.
    ceiling_raised: AtomicBool,
    /// Real packets dropped since the current offset gap opened; both the periodic warning and
    /// the one on the packet that ends the gap read it.
    dropped_no_offset: AtomicU32,
    /// Player-clock ms at the last REAL packet fed by [`Self::play_audio`].
    /// [`Self::run_clock_plane`] reads it to stay off the plane while the real stream carries it.
    ///
    /// Starts at 0 (the load instant), NOT a sentinel: the grace then runs from session start, so
    /// an offloaded session whose audio arrives normally never feeds a single silent packet. The
    /// sentinel made every such session open by bursting silence to a ceiling a whole prime ahead
    /// of the real timeline, and the real packets that followed were all floored onto it.
    last_real_feed_ms: AtomicI64,
    /// `false` while `LOADCOMPLETED` still hasn't been seen for this load. Latched once, so the
    /// steady-state feed path costs one relaxed load.
    ///
    /// It is also what [`Self::flush`] refuses on, which is the guard that matters most: flushing
    /// a pipeline NDL has not finished loading kills the session's audio permanently.
    load_confirmed: AtomicBool,
    /// HDR mastering metadata that arrived before the video plane had taken a frame.
    /// `NDL_DirectVideoSetHDRInfo` returns success against a pipeline that isn't ingesting yet
    /// but does nothing — the panel never enters HDR mode, and since the host only sends the
    /// packet on change there is no second chance. Observed on a CX as an all-black launch that
    /// a reconnect fixed.
    ///
    /// The gate is the first ACCEPTED frame, not `LOADCOMPLETED`: that callback says the pipeline
    /// loaded, not that it is ingesting, and a frame NDL took is the only evidence of the latter.
    pending_hdr: Mutex<Option<ffi::HdrInfo>>,
    /// Last metadata actually handed to `NDL_DirectVideoSetHDRInfo`, so an unchanged one is never
    /// re-applied.
    ///
    /// The host re-sends the mastering packet unchanged — three identical ones inside 10 ms at
    /// session start — and every call re-enters panel HDR mode, which on a CX drops 1440p120 out of
    /// its high-rate mode and leaves the stream black. (Latent until the metadata was deferred: the
    /// repeats used to land on a pipeline that wasn't ingesting yet.)
    applied_hdr: Mutex<Option<ffi::HdrInfo>>,
}

impl NdlVideo {
    /// Load NDL video stream. Calls `NDL_DirectMediaInit` on first use.
    /// Audio request is a probe: fails silently on unsupported models, retries video-only.
    pub fn load(app_id: &str, width: i32, height: i32, codec: NdlCodec, audio: bool) -> Result<Self> {
        ensure_not_poisoned()?;
        let fns = ffi::v2()?;
        ensure_init(app_id, true)?;
        let video = ffi::VideoInfo {
            width,
            height,
            kind: codec.ndl_type(),
            unknown1: 0,
        };
        if audio {
            // An audio-enabled load must PROVE itself with `LOADCOMPLETED`; a video-only one may
            // proceed unconfirmed (below). `NDL_DirectMediaLoad` returning 0 is only "request
            // accepted" — an Opus config the pipeline rejects fails asynchronously, then accepts
            // every fed frame into a pipeline that is not running, which is a whole session of
            // black picture with no error anywhere. Falling back needs an unload first (a failed
            // load may hold decoder resources, docs/NOTES.md); it covers the `Err` arm, where no
            // handle exists to unload on drop, and is harmless as a repeat.
            // Snapshotted BEFORE the attempt, because the unconfirmed handle's own `Drop` unloads
            // at the end of the match: a snapshot taken after it would miss that
            // `UNLOADCOMPLETED` and wait out the settle for a callback already spent.
            let unloads_before = super::unload_count();
            match Self::try_load(fns, video, true) {
                Ok(loaded) if loaded.load_confirmed.load(Ordering::Relaxed) => return Ok(loaded),
                Ok(_) => tracing::warn!("NDL audio-enabled load failed (no LOADCOMPLETED) — retrying video-only"),
                Err(e) => tracing::warn!("NDL audio-enabled load failed ({e:#}) — retrying video-only"),
            }
            // Best-effort cleanup of the rejected load.
            fns.unload();
            // The rejected load's callbacks are indistinguishable from the retry's, so let them
            // land BEFORE arming below rather than racing them.
            settle_before_retry(unloads_before);
        }
        Self::try_load(fns, video, false)
    }

    /// One `NDL_DirectMediaLoad` attempt, waited out to `LOADCOMPLETED` — priming the audio plane
    /// through the wait when the load asked for one (see [`Self::prime_audio`]).
    fn try_load(fns: &'static ffi::V2, video: ffi::VideoInfo, audio: bool) -> Result<Self> {
        let mut info = ffi::DataInfo {
            video,
            audio: if audio { plane_config() } else { ffi::AudioUnion::SILENT },
        };
        arm_load();
        let load_requested = Instant::now();
        fns.load(&mut info, Some(super::on_load_state))?;
        // `ret == 0` is "request accepted", not "pipeline ready" — the first feed still needs
        // LOADCOMPLETED, and an audio-enabled load will not report it until its audio plane has
        // seen a packet, which is what the prime supplies.
        let (primed_pts_ms, confirmed) = if audio {
            Self::prime_audio(fns)
        } else {
            (0, wait_load_completed())
        };
        Ok(Self {
            fns,
            load_instant: Instant::now(),
            load_requested,
            audio,
            clock: std::sync::OnceLock::new(),
            skew_epoch: AtomicU64::new(0),
            last_audio_pts_ms: AtomicI64::new(primed_pts_ms),
            audio_skew_ms: AtomicI64::new(0),
            ceiling_raised: AtomicBool::new(false),
            dropped_no_offset: AtomicU32::new(0),
            last_real_feed_ms: AtomicI64::new(0),
            load_confirmed: AtomicBool::new(confirmed),
            pending_hdr: Mutex::new(None),
            applied_hdr: Mutex::new(None),
        })
    }

    /// Feed silent Opus packets until the audio-enabled load reports `LOADCOMPLETED`, bounded by
    /// `LOAD_COMPLETE_TIMEOUT`. Returns the highest stamp fed and whether the load confirmed.
    ///
    /// An audio-enabled load will not report until its audio plane has received data, but the
    /// pumps that would supply it don't spawn until `session::connect` returns — i.e. until this
    /// wait is over. That deadlock is the whole black-picture-with-sound bug, so the load window
    /// feeds itself.
    ///
    /// A burst at a time, because a packet fed before the plane exists may be dropped silently
    /// (`NDL_DirectAudioPlay` reports success either way).
    ///
    /// The ceiling is handed to `last_audio_pts_ms`: real packets are stamped in the video plane's
    /// domain, which also starts near 0, so without that floor the first would read as a rewind —
    /// which mutes the session permanently (see [`Self::play_audio`]). Flooring costs a few early
    /// packets their exact stamp; the alternative is no audio at all.
    fn prime_audio(fns: &'static ffi::V2) -> (i64, bool) {
        let silence = &OPUS_SILENCE[..];
        let start = Instant::now();
        let mut pts_ms = 0;
        while !LOAD_COMPLETED.fired() {
            if start.elapsed() >= super::LOAD_COMPLETE_TIMEOUT {
                tracing::warn!(
                    "NDL load: no LOADCOMPLETED within {:?} of priming {pts_ms}ms of silence",
                    super::LOAD_COMPLETE_TIMEOUT
                );
                return (pts_ms, false);
            }
            // Stamps track wall-clock, topped up to PRIME_LEAD packets ahead of it — so the burst
            // per gap is however many 5 ms packets that gap consumed, and the ceiling stays a
            // fixed lead over real time. That ceiling is the floor real audio is pinned to.
            let target_ms = start.elapsed().as_millis() as i64 + PRIME_LEAD * PRIME_PACKET_MS;
            {
                let _ffi = lock_ffi();
                while pts_ms < target_ms {
                    if let Err(e) = fns.audio_play(silence, pts_ms) {
                        tracing::warn!("NDL audio prime rejected at {pts_ms}ms: {e:#}");
                        return (pts_ms, LOAD_COMPLETED.fired());
                    }
                    pts_ms += PRIME_PACKET_MS;
                }
            }
            super::poll_until(PRIME_RETRY, || LOAD_COMPLETED.fired());
        }
        tracing::info!(
            "NDL audio prime: LOADCOMPLETED after {:?} ({pts_ms}ms of silence)",
            start.elapsed()
        );
        (pts_ms, true)
    }

    /// Whether this load has an audio plane. It always does when the load confirmed, since NDL
    /// only paces the picture against a fed audio plane — see [`Self::run_clock_plane`]. False means
    /// the audio-enabled load was rejected and `load()` fell back to video-only.
    pub fn has_audio_plane(&self) -> bool {
        self.audio
    }

    /// Place the run that starts at `base_ms` one packet above the audio ceiling, so the resumed
    /// stream advances instead of flooring onto it — see [`Self::play_audio`].
    ///
    /// The run's own first stamp is `base_ms + PLANE_LEAD_MS`, not `base_ms`, so the lead is
    /// discounted here — charging skew for a gap the lead already closes would stack the two and
    /// push audio a second lead behind the picture on every re-latch.
    ///
    /// Under `lock_ffi` because the clock plane raises that ceiling from its own thread. Called
    /// from the audio thread on the first packet of a new mapping epoch, so the video thread never
    /// pays for this guard at all.
    fn derive_audio_skew(&self, base_ms: i64) {
        let skew = {
            let _ffi = lock_ffi();
            // Never negative: a run already above the ceiling needs no help, and pulling it DOWN
            // to meet one is the rewind NDL mutes on.
            let skew =
                (self.last_audio_pts_ms.load(Ordering::Relaxed) + PRIME_PACKET_MS - (base_ms + PLANE_LEAD_MS)).max(0);
            self.audio_skew_ms.store(skew, Ordering::Relaxed);
            skew
        };
        // The cost of the skew is lip sync: audio rides `skew` ms behind the picture until the next
        // re-latch, because NDL has no way to pull its ceiling back down. A filler burst costs
        // `PRIME_LEAD` packets of it and nobody notices; a video plane that jumped seconds ahead
        // costs seconds, which is worth seeing in the log rather than only hearing. Logged outside
        // the guard — the video feed shares it.
        if skew > SKEW_WARN_MS {
            tracing::warn!("NDL audio re-latched {skew}ms behind the picture (video plane jumped)");
        }
    }

    /// Feed one Opus packet to NDL's audio plane, for the TV's own decoder to render. Called only
    /// on the offload route, where the real stream rides the plane. `host_pts_ns` is the packet's own host capture
    /// timestamp, NOT arrival time.
    ///
    /// Every stamp carries [`PLANE_LEAD_MS`] on top of the mapped host time, so NDL always holds
    /// that much audio ahead of its renderer — read that constant before touching this arithmetic.
    ///
    /// **Both planes must be stamped in one time base** — NDL runs its own A/V synchronisation
    /// against these values, and regulating a video plane on host-capture cadence against an audio
    /// plane on arrival wall-clock is what froze the picture on webOS 10.3 (docs/NOTES.md § "NDL's
    /// audio plane"). So the host PTS goes through the video plane's own offset
    /// ([`latch_pts_offset`](Self::latch_pts_offset)).
    ///
    /// Returns `Ok(())` having fed nothing while no offset is latched: audio before the first
    /// video frame has no timeline to join yet, and dropping those few packets beats feeding them
    /// at a stamp that jumps once the real offset lands. A gap is logged while it lasts and again
    /// on the packet that ends it — the silent version of this cost a session its audio with
    /// nothing in the log to find.
    ///
    /// **The stamp is skewed, not floored, across a re-latch.** NDL reads a timestamp going
    /// backwards as a rewind and mutes the rest of the session, so the ceiling below is mandatory
    /// — but flooring EVERY packet onto it is what killed audio in the field: the sink's re-anchor
    /// maps the resumed stream onto the current player clock, and when that lands below the
    /// ceiling (the video plane was running ahead — a receive-backlog flush jumping to live — or
    /// the clock plane bursted its [`PRIME_LEAD`] of silence during the gap) every packet floors
    /// to the same stamp, audio stops advancing, and nothing ever lifts it back off.
    /// [`Self::derive_audio_skew`] moves the whole run above the ceiling instead; the floor stays
    /// as the guard for what it cannot see — an out-of-order packet, or a burst landing between
    /// the latch and the run's first packet — and is also what publishes the ceiling itself.
    pub fn play_audio(&self, packet: &[u8], host_pts_ns: u64) -> Result<()> {
        // Fast path: no latched timeline means nothing to stamp against, and the packet is dropped.
        let Some((clock, probe_ns)) = self.clock.get().and_then(|c| Some((c, c.map_host_ns(host_pts_ns)?))) else {
            let dropped = self.dropped_no_offset.fetch_add(1, Ordering::Relaxed) + 1;
            // Reported on the way through, not only on the packet that ends the gap: a gap that
            // never ends is exactly the failure worth seeing, and that path logs nothing at all.
            if dropped % NO_OFFSET_WARN_PACKETS == 0 {
                tracing::warn!(
                    "NDL audio dropped for {}ms — no latched timeline (the video plane has fed no \
                     accepted frame since the last hold); the clock plane is pacing the picture",
                    i64::from(dropped) * PRIME_PACKET_MS,
                );
            }
            return Ok(());
        };
        // Re-derive skew on a new mapping epoch, or when the clock plane raised the ceiling
        // without an epoch change (a hiccup it filled over) — either way the run must land
        // ABOVE the ceiling left behind, or it floors and the session's audio is gone for good.
        // Outside `lock_ffi`, which `derive_audio_skew` takes for itself.
        let epoch = clock.epoch();
        // Unconditional swaps: `||` short-circuiting past the second leaves the flag set,
        // re-deriving again on the next packet onto the same stamp this one just published.
        let new_epoch = self.skew_epoch.swap(epoch, Ordering::Relaxed) != epoch;
        let raised = self.ceiling_raised.swap(false, Ordering::Relaxed);
        if new_epoch || raised {
            self.derive_audio_skew((probe_ns / 1_000_000) as i64);
        }
        {
            let _ffi = lock_ffi();
            // Re-read under the guard: the check above is only a fast path, and a clear/re-latch
            // landing between the two would pair a stale mapping with the new skew.
            let Some(base_ns) = clock.map_host_ns(host_pts_ns) else {
                return Ok(());
            };
            let raw_ms = ((base_ns / 1_000_000) as i64)
                .saturating_add(self.audio_skew_ms.load(Ordering::Relaxed))
                .saturating_add(PLANE_LEAD_MS);
            let pts_ms = self.last_audio_pts_ms.fetch_max(raw_ms, Ordering::Relaxed).max(raw_ms);
            self.fns.audio_play(packet, pts_ms)?;
        }
        // Player clock, not the packet's domain: the reader asks "how long since a packet ARRIVED".
        self.last_real_feed_ms
            .store((self.elapsed_ns() / 1_000_000) as i64, Ordering::Relaxed);
        let dropped = self.dropped_no_offset.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            tracing::info!(
                "NDL audio resumed after {dropped} packet(s) ({}ms) with no latched timeline, \
                 skew now {}ms",
                i64::from(dropped) * PRIME_PACKET_MS,
                self.audio_skew_ms.load(Ordering::Relaxed),
            );
        }
        Ok(())
    }

    /// Feed silence from `from_ms` up to `target_ms`, returning the last stamp fed.
    ///
    /// One `lock_ffi` for the whole burst, like [`prime_audio`](Self::prime_audio) — the video feed
    /// shares that guard, so per-packet acquires would be up to 60 of them in the picture's way.
    ///
    /// The floor is read under the guard, which is also the whole race story: `lock_ffi` is the
    /// audio plane's only feed path, so no real packet can land mid-burst and read a stale ceiling
    /// — it either precedes this and is picked up by the floor, or follows the final publish.
    fn burst_silence(&self, from_ms: i64, target_ms: i64) -> Result<i64> {
        let silence = &OPUS_SILENCE[..];
        let _ffi = lock_ffi();
        let mut pts_ms = from_ms.max(self.last_audio_pts_ms.load(Ordering::Relaxed));
        while pts_ms < target_ms {
            pts_ms += PRIME_PACKET_MS;
            if let Err(e) = self.fns.audio_play(silence, pts_ms) {
                // Publish before unwinding: this burst has already handed NDL stamps above the old
                // ceiling, and leaving it stale lets the next real packet floor below them — a
                // rewind, which mutes the session for good.
                self.last_audio_pts_ms.fetch_max(pts_ms, Ordering::Relaxed);
                return Err(e);
            }
        }
        self.last_audio_pts_ms.fetch_max(pts_ms, Ordering::Relaxed);
        Ok(pts_ms)
    }

    /// Keep the audio plane fed until `stop`. Blocks, so the caller gives it a thread.
    ///
    /// **A fed audio plane is what makes NDL pace the picture at all** (docs/NOTES.md § "NDL's
    /// audio plane"), so every V2 load with a plane runs this — the invariant is the decoder's, not
    /// whichever session-layer pump happens to exist.
    ///
    /// `yields_to_real` is the whole difference between the two audio paths. Off (software decode):
    /// a pure metronome, the only feed. On (NDL offload): the real stream is the metronome, and
    /// this fills in only after [`REAL_FEED_GRACE_MS`] without a packet — a dead host capture,
    /// which would otherwise starve the plane and freeze the picture.
    pub fn run_clock_plane(&self, stop: &std::sync::atomic::AtomicBool, yields_to_real: bool) {
        // Continue the prime's stamps instead of restarting from the player clock: the prime runs
        // BEFORE `load_instant`, so its ceiling already sits a whole prime ahead, and targeting the
        // raw clock would feed nothing until it caught up — dead exactly at session start. The
        // resulting constant offset from the video timeline costs a metronome nothing.
        let mut base_ms = self.last_audio_pts_ms.load(Ordering::Relaxed);
        let mut pts_ms = base_ms;
        let mut filling = false;
        while !stop.load(Ordering::Relaxed) {
            let now_ms = (self.elapsed_ns() / 1_000_000) as i64;
            if yields_to_real && now_ms - self.last_real_feed_ms.load(Ordering::Relaxed) < REAL_FEED_GRACE_MS {
                if filling {
                    tracing::info!("NDL clock plane: host audio resumed at {now_ms}ms — yielding");
                    filling = false;
                }
                std::thread::sleep(PRIME_RETRY);
                continue;
            }
            if yields_to_real && !filling {
                // Rebase onto where the real stream left the ceiling: the start-of-session base
                // would re-add the prime's whole lead on top of it, and recovered audio then floors
                // to that jumped ceiling — pinned to one stamp for the length of the jump.
                base_ms = self.last_audio_pts_ms.load(Ordering::Relaxed) - now_ms;
                tracing::warn!(
                    "NDL clock plane: no host audio for {REAL_FEED_GRACE_MS}ms — filling silence \
                     to keep the picture paced (host capture is likely dead)"
                );
                filling = true;
            }
            let burst = self.burst_silence(pts_ms, base_ms + now_ms + PRIME_LEAD * PRIME_PACKET_MS);
            // Per burst, including a failed one (it still published stamps before unwinding, and
            // this thread won't run again): the real stream must re-skew off the moved ceiling.
            self.ceiling_raised.store(true, Ordering::Relaxed);
            match burst {
                Ok(fed_to) => pts_ms = fed_to,
                Err(e) => {
                    // Dead for the session; the picture keeps running unpaced, as it did before
                    // the clock plane existed.
                    tracing::warn!("NDL clock plane stopping at {pts_ms}ms: {e:#}");
                    return;
                }
            }
            std::thread::sleep(PRIME_RETRY);
        }
        tracing::info!("NDL clock plane ending at {pts_ms}ms");
    }

    /// How far the audio plane's stamps currently run ahead of the player clock, in ms — the queue
    /// depth NDL paces the picture on ([`PLANE_LEAD_MS`]), and the only observable proxy for it.
    /// Reads the ceiling, so it reports whichever feed last raised it. Sagging towards zero under
    /// real audio is the stutter signature.
    pub fn audio_plane_lead_ms(&self) -> i64 {
        self.last_audio_pts_ms.load(Ordering::Relaxed) - (self.elapsed_ns() / 1_000_000) as i64
    }

    /// Nanoseconds since `load()` (NDL PTS domain). The sink anchors the host PTS onto this
    /// (`session::timeline::HostPtsAnchor`) — NDL has no PTS clock of its own.
    pub(crate) fn elapsed_ns(&self) -> u64 {
        self.load_instant.elapsed().as_nanos() as u64
    }

    /// `Err` while this load hasn't reported `LOADCOMPLETED`: the sink then flushes, holds and
    /// requests a keyframe — exactly what a late load needs, instead of frames into a decoder that
    /// isn't there. Bounded by [`FEED_ANYWAY_AFTER`], since a model that never delivers the
    /// callback must still stream.
    fn ensure_loaded(&self) -> Result<()> {
        if self.load_confirmed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let elapsed = self.load_requested.elapsed();
        if LOAD_COMPLETED.fired() {
            tracing::info!("NDL LOADCOMPLETED landed {elapsed:?} after load");
        } else if elapsed >= FEED_ANYWAY_AFTER {
            // Real elapsed, not the constant — `load()` has already spent
            // `LOAD_COMPLETE_TIMEOUT` by the time a frame gets here.
            tracing::warn!("NDL: still no LOADCOMPLETED {elapsed:?} after the load — feeding anyway");
        } else {
            return Err(NotReady.into());
        }
        self.load_confirmed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Guard on the deferred-HDR slot (see [`Self::pending_hdr`]).
    fn pending_hdr(&self) -> MutexGuard<'_, Option<ffi::HdrInfo>> {
        self.pending_hdr.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Apply metadata held back for the load, on the edge where the first frame was accepted.
    /// A colour failure only logs: the caller reads an `Err` out of [`Self::play`] as a decode
    /// error and answers it with a flush and a keyframe request.
    fn replay_pending_hdr(&self) {
        // Held across the apply: a `set_color_info` racing in from the connect thread must not
        // store into a slot already drained (stranding it for the session) or overwrite this. Held ACROSS the
        // apply too: released at the drain, a racing newer value applies first and this stale one
        // then overwrites it, and `applied_hdr` won't dedupe a value that genuinely differs.
        let mut pending = self.pending_hdr();
        if let Some(info) = pending.take() {
            tracing::info!("NDL: applying HDR metadata held until the first accepted frame");
            if let Err(e) = self.apply_hdr_info(info) {
                tracing::warn!("NDL: applying held HDR metadata failed: {e:#}");
            }
        }
    }

    /// Apply metadata, unless it is what the panel is already on — see [`Self::applied_hdr`].
    /// The slot is held across the FFI call so two threads can't both decide the value is new.
    fn apply_hdr_info(&self, info: ffi::HdrInfo) -> Result<()> {
        let mut applied = self.applied_hdr.lock().unwrap_or_else(PoisonError::into_inner);
        if *applied == Some(info) {
            return Ok(());
        }
        self.set_hdr_info(info)?;
        *applied = Some(info);
        Ok(())
    }

    fn set_hdr_info(&self, info: ffi::HdrInfo) -> Result<()> {
        let _ffi = lock_ffi();
        self.fns.set_hdr_info(info)
    }

    /// Feed one access unit at `pts_ns` (ns since `load()`), truncated to ms for NDL.
    /// Pass the host-anchored base (`session::timeline::HostPtsAnchor`), not raw `elapsed_ns()`,
    /// so video and offloaded audio share one timeline.
    pub fn play(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.ensure_loaded()?;
        let pts_ms = (pts_ns / 1_000_000) as i64;
        let first_frame = {
            let _ffi = lock_ffi();
            self.fns.video_play(au, pts_ms)?;
            mark_frame_fed_logged("NDL", self.load_instant)
        };
        // Outside the FFI guard — `replay_pending_hdr` takes it again, and it isn't reentrant.
        if first_frame {
            self.replay_pending_hdr();
        }
        Ok(())
    }

    /// Apply HDR mastering metadata. `meta` and `color` use the same SEI-standard
    /// units NDL expects (G/B/R order per ST.2086), so no conversion is needed.
    ///
    /// `meta: None` (an SDR stream) is a **no-op**: on this platform
    /// `NDL_DirectVideoSetHDRInfo` emits an HDR infoframe on *any* call — it ignores the
    /// SDR `transfer`/`primaries` triplet and flips the panel into HDR picture mode
    /// regardless (observed on OLED65CX with an H.264 SDR stream). So an SDR stream must
    /// not call it at all; its colorimetry rides the bitstream VUI instead. (This means
    /// NDL can't be used to correct a bitstream with missing/"unspecified" VUI colour
    /// info — the earlier reason this was called unconditionally — but forcing the panel
    /// into HDR for SDR content is the worse outcome.)
    ///
    /// Deferred until the plane has accepted a frame — see [`Self::pending_hdr`].
    pub fn set_color_info(
        &self,
        meta: Option<&punktfunk_core::quic::HdrMeta>,
        color: punktfunk_core::quic::ColorInfo,
    ) -> Result<()> {
        let Some(m) = meta else {
            return Ok(());
        };
        // G/B/R order (ST.2086 convention).
        let [g, b, r] = m.display_primaries;
        let info = ffi::HdrInfo {
            display_primaries_x0: c_uint::from(g[0]),
            display_primaries_y0: c_uint::from(g[1]),
            display_primaries_x1: c_uint::from(b[0]),
            display_primaries_y1: c_uint::from(b[1]),
            display_primaries_x2: c_uint::from(r[0]),
            display_primaries_y2: c_uint::from(r[1]),
            white_point_x: c_uint::from(m.white_point[0]),
            white_point_y: c_uint::from(m.white_point[1]),
            max_display_mastering_luminance: m.max_display_mastering_luminance as c_uint,
            min_display_mastering_luminance: m.min_display_mastering_luminance as c_uint,
            max_content_light_level: c_uint::from(m.max_cll),
            max_pic_average_light_level: c_uint::from(m.max_fall),
            transfer_characteristics: c_uint::from(color.transfer),
            color_primaries: c_uint::from(color.primaries),
            matrix_coeffs: c_uint::from(color.matrix),
            reserved: [0; 32],
        };
        // Checked under the slot's lock, against the drain in `replay_pending_hdr`.
        let mut pending = self.pending_hdr();
        if !super::presenting() {
            *pending = Some(info);
            return Ok(());
        }
        // Guard held across the apply, like `replay_pending_hdr`: released here, a racing replay
        // could land its older value last, and `applied_hdr` won't dedupe differing values.
        *pending = None;
        self.apply_hdr_info(info)
    }

    /// Buffered-but-undisplayed frames in NDL (None if the query fails).
    /// Rising length = decoder behind; flat near-zero with stutter = upstream problem.
    pub fn render_buffer_length(&self) -> Option<i32> {
        let _ffi = lock_ffi();
        self.fns.render_buffer_length()
    }

    pub fn flush(&self) -> Result<()> {
        // Never against a pipeline that hasn't reported LOADCOMPLETED: the flush silently kills
        // the session's audio plane for the rest of the load (see [`NotReady`]), and nothing
        // has been fed yet, so there is no render buffer to discard either. The sink's loss path
        // flushes before it ever calls `play`, so the guard has to live here.
        if !self.load_confirmed.load(Ordering::Relaxed) && !LOAD_COMPLETED.fired() {
            return Ok(());
        }
        let _ffi = lock_ffi();
        self.fns.flush_render_buffer()
    }
}

impl Drop for NdlVideo {
    fn drop(&mut self) {
        // Re-arm so `playing()` stops reporting the load being torn down here.
        arm_load();
        self.fns.unload();
    }
}

impl MediaClock for NdlVideo {
    fn now_ns(&self) -> u64 {
        self.elapsed_ns()
    }
}

impl AudioSink for NdlVideo {
    fn name(&self) -> &'static str {
        "NDL Opus plane"
    }

    /// What the load asked for, and what every silence burst here already speaks — see
    /// [`plane_config`].
    fn format(&self) -> AudioFormat {
        AudioFormat::Opus { channels: 2 }
    }

    fn feed(&self, samples: Samples<'_>, host_pts_ns: u64) -> Result<()> {
        let Samples::Opus(packet) = samples else {
            // The plane decodes; decoded samples are the SDL device's shape and never reach here.
            bail!("NDL audio plane takes Opus packets only");
        };
        self.play_audio(packet, host_pts_ns)
    }

    fn depth_ms(&self) -> Option<i64> {
        Some(self.audio_plane_lead_ms())
    }
}

impl AudioPlane for NdlVideo {
    fn attach_clock(&self, clock: std::sync::Arc<crate::core::media::SessionClock>) {
        let _ = self.clock.set(clock);
    }

    fn lead_ms(&self) -> i64 {
        self.audio_plane_lead_ms()
    }

    fn run_keepalive(&self, stop: &std::sync::atomic::AtomicBool, yields_to_real: bool) {
        self.run_clock_plane(stop, yields_to_real);
    }
}

/// Implemented for the `Arc`, not for `NdlVideo`: the audio plane is the same handle as the video
/// one (NDL has no per-plane context), and the plane's threads must keep the load alive — the
/// process-global unload in `Drop` cannot run while one of them is still inside an FFI call.
impl VideoSink for std::sync::Arc<NdlVideo> {
    fn name(&self) -> &'static str {
        "NDL v2"
    }

    fn caps(&self) -> VideoSinkCaps {
        VideoSinkCaps {
            pts: true,
            partial_au: true,
            flush: true,
            render_queue: true,
        }
    }

    fn feed(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.play(au, pts_ns)
    }

    fn flush(&self) -> Result<()> {
        NdlVideo::flush(self)
    }

    fn queue_depth(&self) -> Option<u32> {
        self.render_buffer_length().and_then(|d| u32::try_from(d).ok())
    }

    fn set_color(
        &self,
        meta: Option<&punktfunk_core::quic::HdrMeta>,
        color: punktfunk_core::quic::ColorInfo,
    ) -> Result<()> {
        self.set_color_info(meta, color)
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        Some(self.as_ref())
    }

    fn audio_plane(&self) -> Option<std::sync::Arc<dyn AudioPlane>> {
        self.has_audio_plane()
            .then(|| Self::clone(self) as std::sync::Arc<dyn AudioPlane>)
    }
}
