//! NDL `DirectMedia` **v2** (webOS 5+): `NDL_DirectMediaLoad` plus
//! `NDL_DirectVideoPlay(buffer, size, pts)`, a render-buffer query, a flush and HDR mastering
//! metadata. The path every currently-working TV takes.
//!
//! Never calls `NDL_DirectVideoSetArea` — stutters above 1080p, and v2 sizes its own
//! punch-through plane (v1 can't; see [`super::v1`]).
use std::ffi::c_uint;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::core::media::{AudioFormat, AudioPlane, AudioSink, MediaClock, NotReady, Samples, VideoSink, VideoSinkCaps};

use super::{arm_load, ensure_init, ensure_not_poisoned, ffi, settle_before_retry, wait_load_completed};
use super::{lock_ffi, mark_frame_fed_logged, NdlCodec, AUDIO_LOAD_TIMEOUT, LOAD_COMPLETED, LOAD_COMPLETE_TIMEOUT};

/// How long past the `NDL_DirectMediaLoad` CALL [`NdlVideo::ensure_loaded`] holds frames while
/// `LOADCOMPLETED` is missing.
///
/// Measured from the load call, so it overlaps the load wait rather than stacking on it: a unit
/// whose callback never comes would otherwise eat `LOAD_COMPLETE_TIMEOUT` + this before the first
/// frame. Overlapped, the wait alone already spends the grace and the first feed goes straight
/// through.
///
/// **A backstop, not a live path.** `load()` already spends a whole load budget
/// ([`AUDIO_LOAD_TIMEOUT`] or [`LOAD_COMPLETE_TIMEOUT`], both longer than this), so the first
/// `ensure_loaded` always feeds and [`NotReady`] is never constructed. Kept because it is what
/// would make an early return from that wait safe.
const FEED_ANYWAY_AFTER: Duration = Duration::from_millis(1_000);

// The grace above is a backstop only while it stays under whichever load budget was spent first.
// Asserted in `const`, not `debug_assert`'d at the branch: CI and the TV both run release, where a
// debug assertion over three compile-time constants is never evaluated at all.
const _: () = assert!(
    FEED_ANYWAY_AFTER.as_millis() < LOAD_COMPLETE_TIMEOUT.as_millis()
        && FEED_ANYWAY_AFTER.as_millis() < AUDIO_LOAD_TIMEOUT.as_millis()
);

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
/// left holding — [`PRIME_LEAD`] packets, and what the prime's ceiling already sits at. The sole
/// feed on the software route holds [`METRONOME_LEAD_MS`] instead, which is deeper.
///
/// **This is not a sync tweak, it is what keeps the picture paced.** NDL regulates the video plane
/// against its audio renderer, and the renderer's clock only advances smoothly while it has data
/// queued ahead of it. Fed straight off the wire the real stream stamps at ≈ the player clock (a
/// packet arrives *after* the frame it was captured with), so the renderer runs at the edge of
/// underrun and the picture stutters on network jitter — the exact failure the clock plane was
/// introduced to fix, back again the moment real audio displaced the metronome. Adding a constant
/// here restores the depth without interleaving silence into the real stream.
///
/// The clock plane targets the same figure WHEN IT SHARES THE PLANE with the real stream (the
/// offload route's fill), which is what lets the two feeders share [`NdlVideo::last_audio_pts_ms`]
/// without either driving it. As the sole feed it holds [`METRONOME_LEAD_MS`] instead.
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

/// Standing depth the SILENT METRONOME holds, i.e. the software route's cushion — deeper than
/// [`PLANE_LEAD_MS`], which is the real stream's.
///
/// **A preserved measurement, not a derivation.** The metronome used to re-add the prime's ceiling
/// as a base, so its stamp was `ceiling + now_old + PLANE_LEAD_MS`. Work that out in ONE domain
/// and the load duration D cancels: the ceiling was `D + PLANE_LEAD_MS` and the old clock itself
/// started D late, so the stamps sat `2 * PLANE_LEAD_MS` ahead of the load call on every unit —
/// NOT `PLANE_LEAD_MS + D`. (Reading it as `+ D` is what the old `plane_lead` reported, because
/// that figure was taken against the same lagging clock.) So this is exactly the depth the 4K120
/// 5.1 smoothness was confirmed on, now explicit and identical on every unit and mode instead of
/// falling out of how long a TV took to load.
///
/// Kept at parity deliberately, in both directions. Under it is the known stutter risk. Over it is
/// cheap — a silent plane costs nothing to hold, the real audio being on SDL, so unlike
/// [`PLANE_LEAD_MS`] depth here is not a lip-sync trade — but it is still an unmeasured change to
/// the one parameter high-refresh smoothness turns on, and this fix is not the place to make one.
/// It IS the knob for a TV that still stutters at high refresh: walk it UP against `plane_lead` in
/// the video heartbeat, which now reports the real depth.
const METRONOME_LEAD_MS: i64 = 2 * PLANE_LEAD_MS;

/// Gap between prime bursts. Polled through, not slept through — the callback lands mid-gap,
/// and this is launch-path time, i.e. black screen.
const PRIME_RETRY: Duration = Duration::from_millis(20);

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
    ///
    /// ⚠ **Stamped at the `NDL_DirectMediaLoad` CALL, not after the load wait**, so it shares its
    /// zero with [`Self::prime_audio`]'s stamps — which count from the same call, because that is
    /// where NDL's own PTS domain starts. Stamping it after the wait instead put the prime a whole
    /// load duration ahead of the player clock, and every consumer then had to correct for that
    /// gap: the metronome carried the prime's ceiling as a permanent base, the offload route
    /// floored its first load-duration of real packets onto a single stamp, and `plane_lead`
    /// reported the gap instead of the depth. One origin removes all three. It also means the
    /// player clock is already at the load duration when the session starts, which is what
    /// `last_real_feed_ms` is seeded against.
    load_instant: Instant,
    /// Whether this load got an audio plane. False on a video-only load, i.e. one whose
    /// audio-enabled attempt was rejected — and with it goes the picture's pacing reference.
    audio: bool,
    /// Highest audio stamp fed so far (ms), shared by [`Self::play_audio`] and the clock plane so
    /// neither can hand NDL a timestamp going backwards — NDL reads a rewind as a seek and mutes
    /// the rest of the session.
    ///
    /// It is a floor, never a driver: every feeder targets the player clock plus its route's lead
    /// ([`PLANE_LEAD_MS`], or [`METRONOME_LEAD_MS`] where the metronome is the only feed), so the
    /// ceiling can only ever be that one lead ahead of real time and cannot ratchet away from it.
    last_audio_pts_ms: AtomicI64,
    /// Player-clock ms at the last REAL packet fed by [`Self::play_audio`].
    /// [`Self::run_clock_plane`] reads it to stay off the plane while the real stream carries it.
    ///
    /// Seeded with the player clock at construction, NOT 0 and NOT a sentinel: `load_instant` is
    /// the load CALL, so the clock already reads the load's duration by the time the session
    /// starts, and a 0 here would read as that many ms since the last real packet — tripping
    /// [`REAL_FEED_GRACE_MS`] before the first one arrives and opening every offloaded session
    /// with a spurious "host capture is likely dead". Seeded, the grace runs from session start,
    /// so an offloaded session whose audio arrives normally never feeds a silent packet.
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
            let why = match Self::try_load(fns, video, true) {
                Ok(loaded) if loaded.load_confirmed.load(Ordering::Relaxed) => return Ok(loaded),
                Ok(_) => "no LOADCOMPLETED".to_string(),
                Err(e) => format!("{e:#}"),
            };
            // Loud, because a video-only load streams unpaced for the rest of the session
            // (issue #188) and every later symptom reads as something else.
            tracing::warn!(
                "NDL audio-enabled load failed ({why}) — retrying video-only, the picture will not be paced"
            );
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
        let load_instant = Instant::now();
        fns.load(&mut info, Some(super::on_load_state))?;
        // `ret == 0` is "request accepted", not "pipeline ready" — the first feed still needs
        // LOADCOMPLETED, and an audio-enabled load will not report it until its audio plane has
        // seen a packet, which is what the prime supplies.
        // Budget and waiter are picked at the same branch: what a load can afford to wait is a
        // property of the load, and the two differ by 3x.
        let (primed_pts_ms, confirmed) = if audio {
            Self::prime_audio(fns, load_instant, AUDIO_LOAD_TIMEOUT)
        } else {
            (0, wait_load_completed(LOAD_COMPLETE_TIMEOUT))
        };
        Ok(Self {
            fns,
            load_instant,
            audio,
            last_audio_pts_ms: AtomicI64::new(primed_pts_ms),
            last_real_feed_ms: AtomicI64::new(load_instant.elapsed().as_millis() as i64),
            load_confirmed: AtomicBool::new(confirmed),
            pending_hdr: Mutex::new(None),
            applied_hdr: Mutex::new(None),
        })
    }

    /// Feed silent Opus packets until the audio-enabled load reports `LOADCOMPLETED`, bounded by
    /// `budget` — longer than the video-only wait, because the fallback this wait gives up to is
    /// an unpaced session rather than a few held frames. Returns the highest stamp fed and whether
    /// the load confirmed.
    ///
    /// `load_instant` is the caller's, not re-derived here: these stamps ARE the player clock's
    /// domain, and handing the origin down is what makes that a value rather than a coincidence of
    /// two adjacent `Instant::now()` calls (see the field).
    ///
    /// An audio-enabled load will not report until its audio plane has received data, but the
    /// pumps that would supply it don't spawn until `session::connect` returns — i.e. until this
    /// wait is over. That deadlock is the whole black-picture-with-sound bug, so the load window
    /// feeds itself.
    ///
    /// A burst at a time, because a packet fed before the plane exists may be dropped silently
    /// (`NDL_DirectAudioPlay` reports success either way).
    ///
    /// The highest stamp is handed to `last_audio_pts_ms` as the floor, without which the first
    /// real packet would read as a rewind — which mutes the session permanently (see
    /// [`Self::play_audio`]). Because `load_instant` is the load call, these stamps are already in
    /// the player-clock domain: the ceiling sits exactly [`PRIME_LEAD`] packets above the clock,
    /// the same lead every later feeder targets, so the clock overtakes it within one lead however
    /// long the load took.
    fn prime_audio(fns: &'static ffi::V2, load_instant: Instant, budget: Duration) -> (i64, bool) {
        let silence = &OPUS_SILENCE[..];
        let mut pts_ms = 0;
        while !LOAD_COMPLETED.fired() {
            // A reported fatal state is the one answer that will not change by waiting, so the
            // long budget above is spent only while the load is still plausibly coming.
            if super::fatal() {
                tracing::warn!("NDL load reported a fatal state after {pts_ms}ms of silence");
                return (pts_ms, false);
            }
            if load_instant.elapsed() >= budget {
                tracing::warn!("NDL load: no LOADCOMPLETED within {budget:?} of priming {pts_ms}ms of silence");
                return (pts_ms, false);
            }
            // Stamps track wall-clock, topped up to PRIME_LEAD packets ahead of it — so the burst
            // per gap is however many 5 ms packets that gap consumed, and the ceiling stays a
            // fixed lead over real time. That ceiling is the floor real audio is pinned to.
            let target_ms = load_instant.elapsed().as_millis() as i64 + PRIME_LEAD * PRIME_PACKET_MS;
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
            load_instant.elapsed()
        );
        (pts_ms, true)
    }

    /// Whether this load has an audio plane. It always does when the load confirmed, since NDL
    /// only paces the picture against a fed audio plane — see [`Self::run_clock_plane`]. False means
    /// the audio-enabled load was rejected and `load()` fell back to video-only.
    pub fn has_audio_plane(&self) -> bool {
        self.audio
    }

    /// Feed one Opus packet to the audio plane, stamped on the PLAYER clock.
    ///
    /// **The host's capture PTS is deliberately ignored.** Deriving the audio stamp from it (via
    /// the session clock, plus a skew re-derived on every re-anchor) ratchets: a video freeze
    /// stalls the mapped timeline while packets keep arriving, the run resumes below the ceiling
    /// it already reached, and the only monotonic repair is to add lead — which nothing in the
    /// session can ever pay back. Measured on a CX: five re-anchors inside four seconds walked the
    /// plane from 78 ms to 124 ms of lead and the session went silent for good, with healthy depth
    /// and sane per-latch skew the whole way. `mariotaku/ss4s` removed the same apparatus in
    /// `ef0c0ae` and stamps both planes off `CLOCK_MONOTONIC` since load; moonlight-tv#493 is the
    /// unfixed version of this failure.
    ///
    /// A wall clock cannot ratchet. It advances at the same rate whatever the host PTS does across
    /// a freeze, so a resumed run lands where an uninterrupted one would have, and the ceiling
    /// below is left with nothing to do but absorb reordering.
    ///
    /// Every stamp carries [`PLANE_LEAD_MS`] on top of it, so NDL always holds that much audio
    /// ahead of its renderer — read that constant before touching this arithmetic. The clock
    /// plane's FILL targets the same figure, which is what lets the two feeders share the ceiling
    /// without either pushing it; the metronome, which never shares a plane with this, does not.
    ///
    /// **Both planes are still in one time base** — NDL synchronises them against each other, and
    /// the video plane's own stamps are the player clock too (`session::timeline::Pacing` maps the
    /// host PTS onto it). What changed is that audio no longer rides the mapping's jumps.
    pub fn play_audio(&self, packet: &[u8]) -> Result<()> {
        // The plane's start gate. Feeding a pipeline NDL has not finished loading costs the
        // session its audio outright, and unlike the video feed this path has no `ensure_loaded`
        // of its own — the prime is what carries the plane through the load window.
        if !self.load_confirmed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let now_ms = (self.elapsed_ns() / 1_000_000) as i64;
        let target_ms = now_ms + PLANE_LEAD_MS;
        {
            let _ffi = lock_ffi();
            // Floor only: `target_ms` is already ahead of anything the clock plane can have fed,
            // so this bites solely on a packet arriving out of order or inside the same
            // millisecond as its predecessor.
            let pts_ms = self
                .last_audio_pts_ms
                .fetch_max(target_ms, Ordering::Relaxed)
                .max(target_ms);
            self.fns.audio_play(packet, pts_ms)?;
        }
        // Player clock, not the packet's domain: the reader asks "how long since a packet ARRIVED".
        self.last_real_feed_ms.store(now_ms, Ordering::Relaxed);
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
        // A fill on the offload route must target exactly what [`Self::play_audio`] targets, or
        // the real packets that resume are floored onto the fill's higher ceiling and pinned to
        // one stamp for the length of it. The metronome is the only feed on its route and answers
        // to nothing else, so it holds the deeper [`METRONOME_LEAD_MS`] cushion. Neither carries a
        // base of its own — see the `load_instant` field for why one is no longer needed.
        let lead_ms = if yields_to_real {
            PLANE_LEAD_MS
        } else {
            METRONOME_LEAD_MS
        };
        let mut pts_ms = self.last_audio_pts_ms.load(Ordering::Relaxed);
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
                tracing::warn!(
                    "NDL clock plane: no host audio for {REAL_FEED_GRACE_MS}ms — filling silence \
                     to keep the picture paced (host capture is likely dead)"
                );
                filling = true;
            }
            match self.burst_silence(pts_ms, now_ms + lead_ms) {
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
    /// depth NDL paces the picture on, and the only observable proxy for it. Reads the ceiling, so
    /// it reports whichever feed last raised it. Sagging towards zero under real audio is the
    /// stutter signature.
    ///
    /// ⚠ **Which target it should read depends on the route**: [`METRONOME_LEAD_MS`] (80) on the
    /// software route, where the silent metronome is the only feed, and [`PLANE_LEAD_MS`] (40) on
    /// offload, where the real stream owns the plane. A software session reading 40 is as wrong as
    /// an offloaded one reading 80.
    pub fn audio_plane_lead_ms(&self) -> i64 {
        self.last_audio_pts_ms.load(Ordering::Relaxed) - (self.elapsed_ns() / 1_000_000) as i64
    }

    /// Nanoseconds since `load()` (NDL PTS domain). The sink anchors the host PTS onto this
    /// (`session::timeline::Pacing`) — NDL has no PTS clock of its own.
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
        let elapsed = self.load_instant.elapsed();
        if LOAD_COMPLETED.fired() {
            tracing::info!("NDL LOADCOMPLETED landed {elapsed:?} after load");
        } else if elapsed >= FEED_ANYWAY_AFTER {
            // Real elapsed, not the constant — `load()` has already spent a whole load budget by
            // the time a frame gets here.
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
    /// Pass the mapped base (`session::timeline::Pacing`), not raw `elapsed_ns()`,
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

    /// `host_pts_ns` is ignored — the plane stamps off the player clock, which is the whole point
    /// of [`NdlVideo::play_audio`].
    fn feed(&self, samples: Samples<'_>, _host_pts_ns: u64) -> Result<()> {
        let Samples::Opus(packet) = samples else {
            // The plane decodes; decoded samples are the SDL device's shape and never reach here.
            bail!("NDL audio plane takes Opus packets only");
        };
        self.play_audio(packet)
    }

    fn depth_ms(&self) -> Option<i64> {
        Some(self.audio_plane_lead_ms())
    }
}

impl AudioPlane for NdlVideo {
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

    fn is_dead(&self) -> bool {
        super::fatal()
    }
}
