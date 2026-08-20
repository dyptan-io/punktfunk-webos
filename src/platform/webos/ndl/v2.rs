//! NDL `DirectMedia` **v2** (webOS 5+): `NDL_DirectMediaLoad` plus
//! `NDL_DirectVideoPlay(buffer, size, pts)`, a render-buffer query, a flush and HDR mastering
//! metadata. The path every currently-working TV takes.
//!
//! Never calls `NDL_DirectVideoSetArea` — stutters above 1080p, and v2 sizes its own
//! punch-through plane (v1 can't; see [`super::v1`]).
use std::ffi::{c_int, c_longlong, c_uint, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

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
/// this), so the first `ensure_loaded` always feeds and [`NotLoadedYet`] is never constructed.
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

/// [`NdlVideo::play`] refusing a frame because `LOADCOMPLETED` hasn't landed. A distinct type
/// because the caller must NOT respond to it the way it responds to a decode error: the usual
/// answer is `NDL_DirectVideoFlushRenderBuffer`, and issuing that against a pipeline NDL has not
/// finished loading takes the session's audio out for good (see `session::sink`).
#[derive(Debug)]
pub struct NotLoadedYet;

impl std::fmt::Display for NotLoadedYet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NDL pipeline not loaded yet — holding")
    }
}

impl std::error::Error for NotLoadedYet {}

/// [`NdlVideo::pts_offset_ns`] before the video plane has published one. Not 0 — a genuine
/// offset of 0 ns is possible, and "unset" must be distinguishable from it.
const NO_PTS_OFFSET: i64 = i64::MIN;

/// Opus audio config for NDL. Stereo only (no multistream/surround support).
#[derive(Clone, Copy)]
pub struct NdlAudioConfig {
    pub channels: i32,
    /// kHz, not Hz — NDL's own unit.
    pub sample_rate: f64,
}

impl NdlAudioConfig {
    fn to_union(self) -> ffi::AudioUnion {
        ffi::AudioOpusInfo {
            kind: 3, // NDL_AUDIO_TYPE_OPUS
            unknown1: 0,
            channels: self.channels as c_int,
            unknown2: 0,
            sample_rate: self.sample_rate,
            stream_header: std::ptr::null(),
            _padding: [0; 4],
        }
        .to_union()
    }
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
    /// Whether the load asked for — and confirmed — an audio plane. What RIDES that plane (the
    /// real Opus stream, or [`Self::run_clock_plane`]'s metronome) is the caller's choice.
    has_audio_plane: bool,
    /// Host-PTS → NDL-player-clock offset in ns, republished by the video plane on every fed
    /// frame (`session::sink`) and read by [`Self::play_audio`] so both planes land in ONE
    /// timeline. [`NO_PTS_OFFSET`] until the first frame is fed.
    pts_offset_ns: AtomicI64,
    /// Highest audio stamp fed so far (ms), so [`Self::play_audio`] can never hand NDL a
    /// timestamp going backwards. Never reset — the ceiling has to survive a re-latch, which is
    /// exactly the case that would otherwise rewind it.
    last_audio_pts_ms: AtomicI64,
    /// Constant added to every mapped real-audio stamp, re-derived on each latch
    /// ([`Self::derive_audio_skew`]) so a resumed run lands above [`Self::last_audio_pts_ms`]
    /// rather than flooring onto it — see [`Self::play_audio`].
    audio_skew_ms: AtomicI64,
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
    pub fn load(app_id: &str, width: i32, height: i32, codec: NdlCodec, audio: Option<NdlAudioConfig>) -> Result<Self> {
        ensure_not_poisoned()?;
        let fns = ffi::v2()?;
        ensure_init(app_id, true)?;
        let video = ffi::VideoInfo {
            width,
            height,
            kind: codec.ndl_type(),
            unknown1: 0,
        };
        if let Some(audio) = audio {
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
            match Self::try_load(fns, video, audio.to_union(), true) {
                Ok(loaded) if loaded.load_confirmed.load(Ordering::Relaxed) => return Ok(loaded),
                Ok(_) => tracing::warn!("NDL audio-enabled load failed (no LOADCOMPLETED) — retrying video-only"),
                Err(e) => tracing::warn!("NDL audio-enabled load failed ({e:#}) — retrying video-only"),
            }
            // SAFETY: no arguments; best-effort cleanup of the rejected load.
            let _ = unsafe { (fns.unload)() };
            // The rejected load's callbacks are indistinguishable from the retry's, so let them
            // land BEFORE arming below rather than racing them.
            settle_before_retry(unloads_before);
        }
        Self::try_load(fns, video, ffi::AudioUnion::SILENT, false)
    }

    /// One `NDL_DirectMediaLoad` attempt, waited out to `LOADCOMPLETED` — priming the audio plane
    /// through the wait when the load asked for one (see [`Self::prime_audio`]).
    fn try_load(
        fns: &'static ffi::V2,
        video: ffi::VideoInfo,
        audio: ffi::AudioUnion,
        with_audio: bool,
    ) -> Result<Self> {
        let mut info = ffi::DataInfo { video, audio };
        arm_load();
        let load_requested = Instant::now();
        // SAFETY: `info` is valid for the duration of this call.
        let ret = unsafe { (fns.load)(&mut info, Some(super::on_load_state)) };
        if ret != 0 {
            bail!("NDL_DirectMediaLoad failed: ret={ret} error={}", ffi::last_error());
        }
        // `ret == 0` is "request accepted", not "pipeline ready" — the first feed still needs
        // LOADCOMPLETED, and an audio-enabled load will not report it until its audio plane has
        // seen a packet, which is what the prime supplies.
        let (primed_pts_ms, confirmed) = if with_audio {
            Self::prime_audio(fns)
        } else {
            (0, wait_load_completed())
        };
        Ok(Self {
            fns,
            load_instant: Instant::now(),
            load_requested,
            has_audio_plane: with_audio,
            pts_offset_ns: AtomicI64::new(NO_PTS_OFFSET),
            last_audio_pts_ms: AtomicI64::new(primed_pts_ms),
            audio_skew_ms: AtomicI64::new(0),
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
                    // SAFETY: NDL reads `size` bytes synchronously and does not retain the pointer.
                    let ret = unsafe {
                        (fns.audio_play)(
                            OPUS_SILENCE.as_ptr() as *mut c_void,
                            OPUS_SILENCE.len() as c_uint,
                            pts_ms as c_longlong,
                        )
                    };
                    if ret != 0 {
                        tracing::warn!(
                            "NDL audio prime rejected at {pts_ms}ms: ret={ret} error={}",
                            ffi::last_error()
                        );
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
        self.has_audio_plane
    }

    /// Latch the video plane's host-PTS → player-clock offset so [`play_audio`](Self::play_audio)
    /// can stamp audio in the same timeline. Called per fed frame from `session::sink`, but takes
    /// **only the first** value after each [`clear_pts_offset`](Self::clear_pts_offset).
    ///
    /// Latched, not republished per frame: the offset is a mapping between two clocks, and it is
    /// stable only while the video plane's own anchor is. Re-deriving it every frame lets any jump
    /// in the video timeline — a receive-backlog flush jumping to live drops frames, so host PTS
    /// leaps forward while the player clock does not — drag the audio stamp *backwards* by the
    /// size of the jump. NDL takes that as a rewind and stops playing audio for the rest of the
    /// session (observed on CX: audio worked in a session with no flush, gone in one with).
    /// `clear_pts_offset` at the sink's anchor resets is what re-derives it.
    ///
    /// `base_ns` is the same frame's mapped stamp, i.e. where the resumed audio will land: this
    /// edge is also where [`Self::derive_audio_skew`] runs, so the audio thread's steady state
    /// stays a single relaxed load.
    pub(crate) fn latch_pts_offset(&self, offset_ns: i64, base_ns: u64) {
        // The steady state is "already latched", and this runs per fed frame — keep the exclusive
        // access off the video thread's hot path once the offset is set.
        if self.pts_offset_ns.load(Ordering::Relaxed) != NO_PTS_OFFSET {
            return;
        }
        if self
            .pts_offset_ns
            .compare_exchange(NO_PTS_OFFSET, offset_ns, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.derive_audio_skew((base_ns / 1_000_000) as i64);
        }
    }

    /// Place the run that starts at `base_ms` one packet above the audio ceiling, so the resumed
    /// stream advances instead of flooring onto it — see [`Self::play_audio`].
    ///
    /// Under `lock_ffi` because the clock plane raises that ceiling from its own thread; the CAS
    /// above makes this the once-per-latch path, not the per-frame one, so the video thread pays
    /// for the guard only on a re-anchor.
    fn derive_audio_skew(&self, base_ms: i64) {
        let skew = {
            let _ffi = lock_ffi();
            // Never negative: a run already above the ceiling needs no help, and pulling it DOWN
            // to meet one is the rewind NDL mutes on.
            let skew = (self.last_audio_pts_ms.load(Ordering::Relaxed) + PRIME_PACKET_MS - base_ms).max(0);
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

    /// Drop the latched offset — the two timelines just decoupled (the sink reset its anchor
    /// after a freeze-until-reanchor hold).
    /// [`play_audio`](Self::play_audio) holds packets until the video plane latches a fresh one,
    /// which is also where the skew that carries them is re-derived.
    pub(crate) fn clear_pts_offset(&self) {
        self.pts_offset_ns.store(NO_PTS_OFFSET, Ordering::Relaxed);
    }

    /// Feed one Opus packet to NDL (only when the real stream rides the plane). `host_pts_ns` is
    /// the packet's own host capture timestamp, NOT arrival time.
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
        if self.pts_offset_ns.load(Ordering::Relaxed) == NO_PTS_OFFSET {
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
        }
        // Map, skew and floor under the guard, never before it: `lock_ffi` is the audio plane's
        // only feed path, so this is what serialises against `burst_silence` — outside it a packet
        // can read a ceiling the filler has already moved past and hand NDL a stale stamp, i.e. the
        // rewind this whole path exists to avoid. The offset is re-read here for the same reason:
        // the check above is only a fast path, and a clear/re-latch landing between the two would
        // pair the OLD offset with the NEW skew — a stamp the size of the video plane's jump above
        // the ceiling, onto which every later packet then floors.
        let ret = {
            let _ffi = lock_ffi();
            let offset_ns = self.pts_offset_ns.load(Ordering::Relaxed);
            if offset_ns == NO_PTS_OFFSET {
                return Ok(());
            }
            let raw_ms = ((host_pts_ns as i64).saturating_add(offset_ns).max(0) / 1_000_000)
                .saturating_add(self.audio_skew_ms.load(Ordering::Relaxed));
            let pts_ms = self.last_audio_pts_ms.fetch_max(raw_ms, Ordering::Relaxed).max(raw_ms);
            // SAFETY: NDL reads `size` bytes synchronously and does not retain the pointer.
            unsafe {
                (self.fns.audio_play)(
                    packet.as_ptr() as *mut c_void,
                    packet.len() as c_uint,
                    pts_ms as c_longlong,
                )
            }
        };
        if ret != 0 {
            bail!("NDL_DirectAudioPlay failed: ret={ret} error={}", ffi::last_error());
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
        let _ffi = lock_ffi();
        let mut pts_ms = from_ms.max(self.last_audio_pts_ms.load(Ordering::Relaxed));
        while pts_ms < target_ms {
            pts_ms += PRIME_PACKET_MS;
            // SAFETY: NDL reads `size` bytes synchronously and does not retain the pointer.
            let ret = unsafe {
                (self.fns.audio_play)(
                    OPUS_SILENCE.as_ptr() as *mut c_void,
                    OPUS_SILENCE.len() as c_uint,
                    pts_ms as c_longlong,
                )
            };
            if ret != 0 {
                // Publish before unwinding: this burst has already handed NDL stamps above the old
                // ceiling, and leaving it stale lets the next real packet floor below them — a
                // rewind, which mutes the session for good.
                self.last_audio_pts_ms.fetch_max(pts_ms, Ordering::Relaxed);
                bail!("NDL_DirectAudioPlay failed: ret={ret} error={}", ffi::last_error());
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
            match self.burst_silence(pts_ms, base_ms + now_ms + PRIME_LEAD * PRIME_PACKET_MS) {
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
            return Err(NotLoadedYet.into());
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
        // SAFETY: passed by value; no pointers or aliasing.
        let ret = unsafe { (self.fns.set_hdr_info)(info) };
        if ret != 0 {
            bail!(
                "NDL_DirectVideoSetHDRInfo failed: ret={ret} error={}",
                ffi::last_error()
            );
        }
        Ok(())
    }

    /// Feed one access unit at `pts_ns` (ns since `load()`), truncated to ms for NDL.
    /// Pass the host-anchored base (`session::timeline::HostPtsAnchor`), not raw `elapsed_ns()`,
    /// so video and offloaded audio share one timeline.
    pub fn play(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.ensure_loaded()?;
        let pts_ms = (pts_ns / 1_000_000) as c_longlong;
        let first_frame = {
            let _ffi = lock_ffi();
            // SAFETY: NDL reads `size` bytes from `buffer` synchronously and does not
            // retain the pointer.
            let ret = unsafe { (self.fns.video_play)(au.as_ptr() as *mut c_void, au.len() as c_uint, pts_ms) };
            if ret != 0 {
                bail!("NDL_DirectVideoPlay failed: ret={ret} error={}", ffi::last_error());
            }
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
        let mut length: c_int = 0;
        let _ffi = lock_ffi();
        // SAFETY: `length` is a valid, writable `c_int` for the duration of the call.
        let ret = unsafe { (self.fns.get_render_buffer_length)(&mut length) };
        (ret == 0).then_some(length)
    }

    pub fn flush(&self) -> Result<()> {
        // Never against a pipeline that hasn't reported LOADCOMPLETED: the flush silently kills
        // the session's audio plane for the rest of the load (see [`NotLoadedYet`]), and nothing
        // has been fed yet, so there is no render buffer to discard either. The sink's loss path
        // flushes before it ever calls `play`, so the guard has to live here.
        if !self.load_confirmed.load(Ordering::Relaxed) && !LOAD_COMPLETED.fired() {
            return Ok(());
        }
        let _ffi = lock_ffi();
        // SAFETY: no arguments.
        let ret = unsafe { (self.fns.flush_render_buffer)() };
        if ret != 0 {
            bail!(
                "NDL_DirectVideoFlushRenderBuffer failed: ret={ret} error={}",
                ffi::last_error()
            );
        }
        Ok(())
    }
}

impl Drop for NdlVideo {
    fn drop(&mut self) {
        // Re-arm so `playing()` stops reporting the load being torn down here.
        arm_load();
        // SAFETY: best-effort teardown; error ignored (Drop can't propagate a Result).
        let _ = unsafe { (self.fns.unload)() };
    }
}
