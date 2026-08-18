//! Connects to a punktfunk host and drives the video/audio hardware pipelines.
//!
//! Video runs on a dedicated thread ([`video_pump`]), which pulls access units off the
//! transport and hands them to a [`sink::NdlSink`] — everything from PTS anchoring down to
//! the NDL `DirectMedia` backend (the sole video backend) lives behind that seam.
//!
//! Audio takes one of two paths, and each has a thread of its own: software-decoded audio is
//! decoded by [`audio_feed_pump`] into the playback ring SDL's audio callback drains
//! (`platform::webos::audio`), and the NDL-offloaded path hands raw Opus straight to NDL from
//! [`ndl_audio_pump`]. Neither shares the main loop, which carries the UI's software rasterizer.
pub mod sink;
pub mod timeline;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::client::{NativeClient, ProbeOutcome};
use punktfunk_core::config::{CompositorPref, Mode};
use punktfunk_core::input::InputEvent;
use punktfunk_core::packet::{FLAG_SOF, USER_FLAG_RECOVERY_ANCHOR};
use punktfunk_core::quic;

use crate::core::caps::video_caps;
use crate::platform::webos::device::{self, NdlGeneration};
use crate::platform::webos::ndl::v1::NdlV1Video;
use crate::platform::webos::ndl::{NdlCodec, NdlVideo};
use crate::services::store::{CodecPref, VideoBackend};
use crate::session::sink::{FrameFlags, NdlSink, SinkConfig, SinkResult, VideoPlayer};

pub struct Connected {
    pub client: Arc<NativeClient>,
    pub stop: Arc<AtomicBool>,
    /// Live pump counters for stats overlay; see `StreamStats`.
    pub stats: Arc<StreamStats>,
    /// Kept alive so `shutdown()` can join and ensure QUIC close frame is sent before exit.
    video_thread: std::thread::JoinHandle<()>,
    /// The NDL audio plane's pump, on every V2 load that got a plane: `ndl_audio_pump` when the
    /// real stream rides it (`audio_offloaded`), `NdlVideo::run_clock_plane`'s metronome otherwise.
    /// `None` only when the load has no audio plane at all (V1, SMP, or a rejected audio load).
    audio_thread: Option<std::thread::JoinHandle<()>>,
    /// True when the REAL Opus stream rides NDL's audio plane; prevents opening the SDL2 audio
    /// device. False on a clock-plane session, which still has a plane but decodes in software.
    pub audio_offloaded: bool,
    /// Whether HDR mastering metadata is being applied this session (negotiated codec is
    /// HEVC *and* the host signalled HDR). Drives which Game picture mode the runtime asks
    /// the TV for — `game` vs `hdrGame` (see `platform::webos::game_mode`).
    pub hdr: bool,
}

/// Live video-pump counters for stats overlay (read at ~2Hz); relaxed atomics written per frame.
#[derive(Default)]
pub struct StreamStats {
    pub frames: std::sync::atomic::AtomicU64,
    /// Bytes received; deltas give measured bitrate.
    pub bytes: std::sync::atomic::AtomicU64,
    /// Freeze-until-reanchor hold active.
    pub holding: AtomicBool,
    /// Most recent decoder feed duration (µs).
    pub feed_us: std::sync::atomic::AtomicU32,
    /// NDL render-buffer backlog or -1 if unavailable.
    pub render_backlog: std::sync::atomic::AtomicI32,
}

/// Short display name for a resolved wire codec id (the stats overlay's header).
pub fn codec_name(codec: u8) -> &'static str {
    match codec {
        c if c == quic::CODEC_HEVC => "HEVC",
        c if c == quic::CODEC_H264 => "H264",
        c if c == quic::CODEC_AV1 => "AV1",
        _ => "?",
    }
}

/// Process CPU time (user+sys clock ticks, see `clock_ticks_per_sec`) and resident
/// memory (bytes), for the stats overlay's CPU/RAM line. Plain `/proc/self` reads.
pub fn process_cpu_mem() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // `comm` (field 2) may contain spaces/parens, so split after the last ')'.
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;

    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64; // SAFETY: no pointers

    Some((utime + stime, rss_pages * page_size))
}

/// Clock ticks per second, for converting `process_cpu_mem`'s ticks to seconds.
pub fn clock_ticks_per_sec() -> u64 {
    (unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64).max(1) // SAFETY: no pointers
}

/// Ceiling on each teardown join below. The video/audio pumps re-check `stop` on a bounded
/// cadence, but the FFI calls they make between checks (NDL `play`/`play_audio`, and the
/// QUIC-close worker `NativeClient::drop` joins internally) have no timeout of their own — an
/// intermittently wedged vendor call must not freeze the whole app on the caller's thread.
/// Also the ceiling the stream teardown waits on (a different mechanism, same rationale).
pub const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Joins `handle` from a watcher thread so a hang inside it can't block the caller past
/// `timeout`. Returns `false` (and leaks the watcher, still waiting on the real join) if it
/// didn't finish in time.
///
/// On timeout, `on_wedged` returns a value the leaked watcher then holds until the real join lands
/// and drops on its way out — how the video/audio pumps keep NDL refused for exactly as long as a
/// wedged thread might still be inside it (`|| ndl::poison()`). The watcher already outlives the
/// timeout, so this needs no second thread; threads with nothing to hold pass `|| ()`. Not called
/// at all when the join lands in time.
fn join_with_timeout<T: Send + 'static, G: Send + 'static>(
    handle: std::thread::JoinHandle<T>,
    timeout: Duration,
    name: &str,
    on_wedged: impl FnOnce() -> G,
) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    // The watcher blocks on this after joining, so the guard can only be dropped once the wedged
    // thread has actually returned — and the send below cannot race that, because the watcher is
    // not listening yet when the timeout fires.
    let (wedged_tx, wedged_rx) = std::sync::mpsc::channel::<G>();
    let spawned = std::thread::Builder::new()
        .name(format!("punktfunk-webos-join-{name}"))
        .spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
            // Either a guard to release (we were declared wedged) or a dropped sender (joined in
            // time, nothing was ever taken out).
            drop(wedged_rx.recv());
        });
    let Ok(watcher) = spawned else {
        // Can't even start the watcher — fall back to a direct (unbounded) join rather
        // than leaking `handle` outright.
        return true;
    };
    if rx.recv_timeout(timeout).is_ok() {
        // BEFORE the join: the watcher is parked on `wedged_rx.recv()` and only this sender going
        // away releases it. Joining first would deadlock the teardown on a thread that finished.
        drop(wedged_tx);
        let _ = watcher.join();
        true
    } else {
        tracing::error!(
            "{name} thread did not finish within {timeout:?} — leaking it \
             (likely a wedged NDL/FFI or QUIC-close call)"
        );
        // Unbounded channel: never blocks, and the value stays queued for however long the wedged
        // thread takes. A failed send would mean the watcher died with the guard, which drops it —
        // correct either way, since a dead watcher means the join already returned.
        let _ = wedged_tx.send(on_wedged());
        false
    }
}

impl Connected {
    /// Stop and join threads, then drop `NativeClient`. Call `disconnect_quit()` first for
    /// graceful shutdown. Returns `false` if any step didn't finish within
    /// [`SHUTDOWN_JOIN_TIMEOUT`] — the caller must then skip `ndl::quit()`, since the thread
    /// still running may still be inside an NDL FFI call that a concurrent unload would race. A
    /// wedged video/audio join additionally refuses new loads until it finishes — those two are the
    /// threads that touch NDL.
    pub fn shutdown(self) -> bool {
        use crate::platform::webos::ndl::poison;
        self.stop.store(true, Ordering::Relaxed);
        let mut clean = join_with_timeout(self.video_thread, SHUTDOWN_JOIN_TIMEOUT, "video", poison);
        if let Some(audio) = self.audio_thread {
            clean &= join_with_timeout(audio, SHUTDOWN_JOIN_TIMEOUT, "audio", poison);
        }
        // `NativeClient::drop` joins its own QUIC-close worker thread internally — bound
        // that the same way, on its own thread, rather than blocking here directly. Doesn't
        // touch NDL, so a wedge here doesn't refuse it, only skips `ndl::quit()` this run.
        let client = self.client;
        clean &= join_with_timeout(
            std::thread::spawn(move || drop(client)),
            SHUTDOWN_JOIN_TIMEOUT,
            "client-drop",
            || (),
        );
        clean
    }
}

/// Default HDR10 mastering metadata for the LG CX OLED panel.
/// Sent in `Hello::display_hdr`; refined per-content by `next_hdr_meta`.
pub(crate) fn cx_display_hdr() -> quic::HdrMeta {
    quic::HdrMeta {
        // G, B, R order (ST.2086), 1/50000 chromaticity units — BT.2020 primaries.
        display_primaries: [[8_500, 39_850], [6_550, 2_300], [35_400, 14_600]],
        white_point: [15_635, 16_450], // D65
        max_display_mastering_luminance: 800 * 10_000,
        min_display_mastering_luminance: 5,
        max_cll: 800,
        max_fall: 150,
    }
}

/// SMP is only selectable where NDL is the narrow v1 generation (`core::caps::smp_selectable`), so
/// trying it can't displace the v2 path. A load that fails falls back to NDL, but only H.264
/// survives that — v1 decodes nothing else.
fn open_player(
    backend: VideoBackend,
    app_id: &str,
    width: i32,
    height: i32,
    fps: u32,
    codec: NdlCodec,
    ndl_audio: Option<crate::platform::webos::ndl::NdlAudioConfig>,
) -> Result<VideoPlayer> {
    if crate::core::caps::effective_backend(backend) == VideoBackend::Smp {
        match crate::platform::webos::smp::SmpVideo::load(app_id, width, height, fps, codec) {
            Ok(sf) => return Ok(VideoPlayer::Smp(sf)),
            Err(e) => tracing::warn!("SMP load failed ({e:#}) — falling back to NDL"),
        }
    }
    match device::ndl_generation() {
        NdlGeneration::V2 => Ok(VideoPlayer::V2(Arc::new(
            NdlVideo::load(app_id, width, height, codec, ndl_audio).context("NDL load")?,
        ))),
        NdlGeneration::V1 => Ok(VideoPlayer::V1(
            NdlV1Video::load(app_id, width, height, codec).context("NDL v1 load")?,
        )),
    }
}

/// Connects to a punktfunk host and starts the video pump thread.
///
/// Blocks until the handshake completes or `timeout` elapses. `pin` is the trusted
/// host fingerprint from a prior pairing (`None` = trust-on-first-use). NDL manages its
/// own punch-through area natively (see [`crate::platform::webos::ndl`]'s module docs),
/// so no display geometry is needed here.
#[allow(clippy::too_many_arguments)]
pub fn connect(
    host: &str,
    port: u16,
    mode: Mode,
    bitrate_kbps: u32,
    hdr_enabled: bool,
    audio_channels: u8,
    identity: (String, String),
    pin: Option<[u8; 32]>,
    launch: Option<String>,
    timeout: Duration,
    codec_pref: CodecPref,
    video_backend: VideoBackend,
    gamepad_type: crate::services::store::GamepadType,
    cursor_capture: bool,
    ndl_audio_offload: bool,
) -> Result<Connected> {
    // Fails before touching the network: a full handshake would only end in `NdlVideo::load()`
    // rejecting the same gate, pointlessly holding the host's pending-session slot for `timeout`.
    crate::platform::webos::ndl::ensure_not_poisoned()?;
    // **The authoritative capability gate.** Codec, colour path and channel count are settled by
    // the handshake, BEFORE any decoder opens, so a document carried over from a more capable TV
    // must be clamped here and not merely hidden in the UI: HEVC negotiated onto an H.264-only
    // decoder is a frozen black stream with no second chance once `Welcome` has resolved.
    let caps = video_caps();
    let codecs = caps.codec_prefs();
    let codec_pref = if codecs.contains(&codec_pref) {
        codec_pref
    } else {
        codecs[0]
    };
    let hdr_enabled = hdr_enabled && caps.hdr;
    let audio_channels = audio_channels.min(caps.max_channels);
    // HDR only ever applies to HEVC. An explicit H.264 pick disables it end to end
    // (the Settings toggle is hidden too — see `ui::settings`'s `row_shown`); on Automatic the
    // caps are still advertised and the host resolves the codec, with application gated
    // on the *negotiated* codec being HEVC further below.
    let hdr_enabled = hdr_enabled && codec_pref != CodecPref::H264;
    // VIDEO_CAP_CHACHA20: unconditional — armv7 has no hardware AES, so ChaCha20 is
    // faster. A ≥0.17.2 host picks it up; older hosts ignore the unknown bit.
    let video_caps = quic::VIDEO_CAP_CHACHA20
        | if hdr_enabled {
            quic::VIDEO_CAP_10BIT | quic::VIDEO_CAP_HDR
        } else {
            0
        };
    let display_hdr = hdr_enabled.then(cx_display_hdr);

    // Advertised decode set + soft preference, folded from the one codec list (`codec_prefs`) so
    // the host's precedence ladder can never auto-pick a path this client can't present.
    let video_codecs = codecs.iter().fold(0, |set, pref| {
        set | match pref {
            CodecPref::Auto => 0,
            CodecPref::H264 => quic::CODEC_H264,
            CodecPref::Hevc => quic::CODEC_HEVC,
        }
    });
    let preferred_codec = match codec_pref {
        CodecPref::Auto => 0,
        CodecPref::H264 => quic::CODEC_H264,
        CodecPref::Hevc => quic::CODEC_HEVC,
    };

    let client = NativeClient::connect(
        host,
        port,
        mode,
        CompositorPref::Auto,
        // Session-default pad kind. A per-pad `InputKind::GamepadArrival` could override this
        // for mixed setups, but this client drives one pad (index 0), for which the handshake
        // default is exactly equivalent — and it also reaches hosts too old to advertise
        // `HOST_CAP_GAMEPAD_STATE`.
        gamepad_type.to_core(),
        bitrate_kbps,
        video_caps,
        // Requested only — the host clamps to what it can capture, and
        // `AudioPlayer::new` is built from the RESOLVED `client.audio_channels`,
        // never from this.
        audio_channels,
        video_codecs,
        preferred_codec,
        display_hdr,
        // client_caps: see `store::Settings::cursor_capture` for the on/off split.
        if cursor_capture { 0 } else { quic::CLIENT_CAP_CURSOR },
        // frame_parts: NDL DirectMedia takes whole access units only — it has no
        // `PARTIAL_FRAME` equivalent, so slice-progressive prefixes would have nowhere to go.
        false,
        launch,
        // Device name for the host's pending-approval list. `None` keeps the host's
        // fingerprint-derived label ("device abcd1234"), i.e. exactly the behaviour before
        // core gained this parameter — sending a real TV name is a separate, user-visible
        // change and does not belong in a dependency bump.
        None,
        pin,
        Some(identity),
        timeout,
    )
    .context("connect")?;
    let client = Arc::new(client);

    let fp_hex = client.host_fingerprint.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    tracing::info!(
        "connected: codec={} (offered=0x{video_codecs:02x} preferred=0x{preferred_codec:02x}) \
         compositor={:?} audio_ch={} color={:?} bitrate_kbps={} \
         decode_latency={} caps=0x{video_caps:02x} fp={fp_hex}",
        client.codec,
        client.resolved_compositor,
        client.audio_channels,
        client.color,
        client.resolved_bitrate_kbps,
        client.wants_decode_latency(),
    );

    let resolved_mode = client.mode();
    let fps = resolved_mode.refresh_hz.max(1);
    let codec =
        NdlCodec::from_wire(client.codec).with_context(|| format!("unsupported codec 0x{:02x}", client.codec))?;
    let app_id = crate::platform::webos::ndl::app_id();
    let (width, height) = (resolved_mode.width as i32, resolved_mode.height as i32);
    let player = open_player(
        video_backend,
        &app_id,
        width,
        height,
        fps,
        codec,
        // Every V2 load asks for a plane: a fed one is what makes NDL pace the picture at all
        // (docs/NOTES.md § "NDL's audio plane"). What rides it is decided below, at the pump.
        // Stereo either way — the silent frame's TOC declares stereo, and a software-decode
        // session's plane never sees the real stream. A set that refuses the load falls back to
        // video-only in `NdlVideo::load`, and gives up pacing with it.
        Some(crate::platform::webos::ndl::NdlAudioConfig {
            channels: 2,
            // kHz, not Hz — NDL's own unit, and what ss4s passes (`info->sampleRate / 1000.0`).
            // punktfunk's audio plane is fixed at 48 kHz (see `audio.rs`'s SAMPLE_RATE).
            sample_rate: 48.0,
        }),
    )?;
    tracing::info!(
        "{} loaded ({codec:?} {}x{}@{fps}fps)",
        player.backend_name(),
        resolved_mode.width,
        resolved_mode.height,
    );

    // Forward the negotiated colorimetry to the decoder for BOTH HDR and SDR
    // streams. The SDR case is not optional: punktfunk encodes BT.709, but with
    // missing/"unspecified" VUI colour info in the bitstream this panel guesses
    // colorimetry from resolution — a 4K SDR stream then decodes as BT.2020,
    // which shows up as exactly the washed-out/desaturated picture reported
    // on-device. `client.color` arrives out-of-band in `Welcome` for precisely
    // this purpose; HDR streams additionally carry mastering metadata.
    // HDR mastering metadata is applied only when the *negotiated* codec is HEVC: the
    // `NdlHdrInfo`/`setHdrInfo` fields are HEVC SEI syntax, and no other codec carries
    // HDR on this platform. Colorimetry (the SDR washed-out fix) is still sent below for
    // every codec — only the mastering metadata is gated.
    let host_hdr = client.color.is_hdr();
    let is_hdr = host_hdr && matches!(codec, NdlCodec::H265);
    let initial_meta = is_hdr.then(cx_display_hdr);
    // What the host signalled in `Welcome`, before the SDR colorimetry fix below acts on it.
    tracing::info!(
        "host colour info: hdr={host_hdr} apply_hdr={is_hdr} codec={codec:?} transfer={} primaries={} matrix={}",
        client.color.transfer,
        client.color.primaries,
        client.color.matrix,
    );
    if let Err(e) = player.set_color_info(initial_meta.as_ref(), client.color) {
        tracing::warn!("NDL colour metadata failed: {e:#}");
    }

    let ndl_audio = player.ndl_audio_handle();
    // Whether the REAL stream rides the plane. `ndl_audio.is_some()` is a different question —
    // it only says the load HAS a plane, which every accepted V2 load does now.
    let audio_offloaded = ndl_audio.is_some() && ndl_audio_offload && client.audio_channels == 2;

    // Naming the REASON matters: "software Opus" is the correct outcome on four different
    // routes plus the user's own override, and a silent session looks identical on all of
    // them. Without this the first debugging question has no answer in the log.
    let path = match (&ndl_audio, &player) {
        _ if audio_offloaded => "NDL hardware Opus decode",
        // A plane the real stream is not using is the pacing metronome — see
        // `NdlVideo::run_clock_plane`.
        (Some(_), _) if !ndl_audio_offload => "software Opus decode -> SDL2 + NDL clock plane (offload not opted in)",
        (Some(_), _) => "software Opus decode -> SDL2 + NDL clock plane (NDL Opus is stereo-only)",
        (None, VideoPlayer::V1(_)) => "software Opus decode -> SDL2, no clock plane (NDL v1 has no Opus audio type)",
        (None, VideoPlayer::Smp(_)) => "software Opus decode -> SDL2, no clock plane (SMP loads video-only)",
        // No plane on a V2 load means the audio-enabled attempt did not confirm and `load()` fell
        // back to video-only, so this session has no pacing reference either.
        (None, VideoPlayer::V2(_)) => "software Opus decode -> SDL2, no clock plane (NDL rejected the audio load)",
    };
    tracing::info!(
        "audio path: {path} (host resolved {} channel(s))",
        client.audio_channels
    );

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(StreamStats::default());
    let video_client = client.clone();
    let video_stop = stop.clone();
    let sink_cfg = SinkConfig {
        stream_hz: resolved_mode.refresh_hz,
        report_decode_latency: client.wants_decode_latency(),
        clock_offset: client.clock_offset_shared(),
        video_e2e: client.video_e2e_shared(),
    };
    let video_stats = stats.clone();
    let video_thread = std::thread::Builder::new()
        .name("punktfunk-webos-video".into())
        .spawn(move || {
            // Built here, not on the caller's thread: the sink queries the panel refresh
            // rate through SDL on construction, and that stayed on the video thread before.
            let sink = NdlSink::new(player, video_stats.clone(), sink_cfg);
            video_pump(video_client, sink, video_stop, video_stats, is_hdr)
        })
        .context("spawn video thread")?;
    let audio_thread = match (ndl_audio, audio_offloaded) {
        (Some(ndl), true) => {
            let audio_client = client.clone();
            let audio_stop = stop.clone();
            Some(
                std::thread::Builder::new()
                    .name("punktfunk-webos-audio".into())
                    .spawn(move || ndl_audio_pump(&audio_client, &ndl, &audio_stop))
                    .context("spawn audio thread")?,
            )
        }
        // Software decode owns the speakers, so this plane is a metronome. Nothing is consumed
        // twice: the real packets still go to `audio_feed_pump`, and this pump generates its own
        // cadence off the player clock.
        (Some(ndl), false) => {
            let clock_stop = stop.clone();
            Some(
                std::thread::Builder::new()
                    .name("punktfunk-webos-clock".into())
                    .spawn(move || ndl.run_clock_plane(&clock_stop))
                    .context("spawn clock plane thread")?,
            )
        }
        (None, _) => None,
    };

    Ok(Connected {
        client,
        stop,
        stats,
        video_thread,
        audio_thread,
        audio_offloaded,
        hdr: is_hdr,
    })
}

/// The no-PIN "request access" trust step: open a trust-on-first-use connection
/// (`pin = None`) presenting our identity, which a host requiring pairing PARKS until
/// its operator approves this device, then return the host's now-verified fingerprint
/// to pin and tear the connection straight back down.
///
/// Uses [`NativeClient`] directly rather than [`connect`] above: no video backend
/// is loaded and no pump thread is spawned, so the video plane is never
/// touched — this only needs the handshake to reach `Welcome`, not a running stream. The
/// negotiated `mode`/codec are irrelevant here (immediately dropped); a small 720p H.264
/// request keeps the host from doing needless 4K/HEVC setup for a connection we close at
/// once. Blocks up to `timeout` (the operator-approval window).
pub fn request_access(host: &str, port: u16, identity: (String, String), timeout: Duration) -> Result<[u8; 32]> {
    let mode = Mode {
        width: 1280,
        height: 720,
        refresh_hz: 60,
    };
    let client = NativeClient::connect(
        host,
        port,
        mode,
        CompositorPref::Auto,
        punktfunk_core::config::GamepadPref::Auto,
        1_000, // minimal bitrate — connection is closed as soon as trust is established
        quic::VIDEO_CAP_CHACHA20,
        2,
        quic::CODEC_H264,
        0,
        None,  // no HDR display metadata
        0,     // client_caps: no local cursor rendering
        false, // frame_parts: whole AUs (see `connect`)
        None,  // no launch
        None,  // name: keep the host's fingerprint-derived label (see `connect`)
        None,  // pin = None → trust-on-first-use, host parks until operator approval
        Some(identity),
        timeout,
    )
    .context("request access connect")?;
    let fingerprint = client.host_fingerprint;
    // Deliberate teardown — the host should drop the parked/approved session now, not
    // linger for a stream that isn't coming. (Runs on a background thread — see
    // `App::try_request_access` — so no log handle here; the caller logs the outcome.)
    client.disconnect_quit();
    Ok(fingerprint)
}

/// What the host is asked to burst during a speed test, and for how long.
///
/// Deliberately **not** the other clients' 3 Gbps / 5 s, and not the 1 Gbps this first
/// shipped with either. Measured on a real CX over Wi-Fi against a 0.19.2 host: a 1 Gbps
/// request was honoured exactly (375 MB pushed in 3 s) while the TV received 87 MB —
/// **~80 % loss** — and in half the attempts the host's end-of-burst `ProbeResult`, which
/// travels over the QUIC control stream *through that same saturated path*, never arrived
/// at all. Overshooting capacity is how a probe finds a ceiling, but overshooting it
/// fourfold mostly measures the access point's drop policy and costs the measurement its
/// own result message.
///
/// The target is chosen against what the answer can actually be *used* for: the bitrate
/// slider caps at `menu::BITRATE_MAX_KBPS` (200 Mbps) and the recommendation is 70 % of
/// measured, so anything above ~285 Mbps already produces an identical clamped
/// recommendation. 320 Mbps stays above that — it can still detect any ceiling that
/// would change the advice — while keeping the overshoot bounded. Measured on a G5
/// (2026-07-24, warm data plane, sweep 260/280/320/400): delivered goodput is a flat
/// ~245 Mbps at every offered rate — the TV's own Wi-Fi radio (USB 2.0-attached) is the
/// ceiling, independently confirmed with a raw UDP flood — so a 400 Mbps burst just
/// raises the shed overshoot (51 % packet loss vs 38 % at 320) and with it the odds the
/// end-of-burst report is starved out, for zero extra information.
const PROBE_TARGET_KBPS: u32 = 320_000;
/// A pinned (non-zero) session rate for the probe connect — see the call site: its only
/// job is to keep `bitrate_kbps == 0` from arming core's own capacity probe against the
/// single shared `ProbeState`. Nothing decodes here, so the value itself is immaterial.
const PROBE_SESSION_BITRATE_KBPS: u32 = 20_000;
/// Below this many delivered bytes, a missing host report is a failure rather than
/// something to salvage — 1 MB over a 3 s burst is ~2.7 Mbps, far under anything worth
/// recommending a bitrate from.
const SALVAGE_MIN_BYTES: u64 = 1024 * 1024;
const PROBE_DURATION_MS: u32 = 3_000;
/// How long to wait for the data plane to prove itself live (first completed video frame)
/// before bursting. Observed on the G5 over Wi-Fi (2026-07-24): a NEW host→client UDP flow
/// is sometimes black-holed (AP/driver flow setup — even the session's own 20 Mbps video
/// is held, while QUIC control chats at ~1 ms RTT), then dumped all at once. Measured
/// holes ranged ~10-29 s, longer after longer idle. A burst fired into that window
/// measures the black hole, not the link. Waiting for the first delivered frame starts
/// every measurement on a proven-live plane; if nothing arrives within the cap, proceed
/// anyway — the burst then behaves exactly as before.
const PROBE_WARMUP_CAP: Duration = Duration::from_secs(35);
/// How long to keep polling for the host's end-of-burst report after the burst should
/// have finished before giving up. Generous: the report shares a link the burst has just
/// been hammering, so its first delivery attempt can well be lost and need a retransmit.
const PROBE_REPORT_GRACE: Duration = Duration::from_secs(12);

/// A finished speed test, and whether the host confirmed the figures.
pub struct SpeedProbeResult {
    pub outcome: ProbeOutcome,
    /// `true` when the host's end-of-burst report arrived. `false` means it never did and
    /// the throughput was derived from what this client actually received over the burst
    /// window it asked for — a real measurement, but with no host-side cross-check, so no
    /// loss figure and a conservative reading if the burst was cut short.
    pub confirmed: bool,
}

/// Runs one network speed test against `host` and returns the host's final measurement.
///
/// Like [`request_access`], this uses [`NativeClient`] directly rather than [`connect`]:
/// no video backend is loaded and no pump thread is spawned, so the punch-through plane
/// is never touched — the host builds a virtual output, but nothing is decoded or
/// presented. Blocks; run it on a worker thread.
///
/// **`video_caps` must advertise `VIDEO_CAP_CHACHA20` exactly as a real session does.**
/// `punktfunk-core` counts the delivered bytes this measurement is derived from *after*
/// AEAD decrypt, so a probe that negotiated AES-GCM would measure a ceiling this armv7
/// CPU can't reach with the cipher an actual stream uses — reporting a number no session
/// could ever deliver. See `docs/NOTES.md` on why `ChaCha20` exists on this client at all.
///
/// `progress` is called with each partial poll so the UI can show the figure climbing.
pub fn run_speed_probe(
    host: &str,
    port: u16,
    identity: (String, String),
    pin: Option<[u8; 32]>,
    timeout: Duration,
    mut progress: impl FnMut(ProbeOutcome),
) -> Result<SpeedProbeResult> {
    let mode = Mode {
        width: 1280,
        height: 720,
        refresh_hz: 60,
    };
    let client = NativeClient::connect(
        host,
        port,
        mode,
        CompositorPref::Auto,
        punktfunk_core::config::GamepadPref::Auto,
        // NOT 0. `bitrate_kbps == 0` is what arms punktfunk-core's OWN startup
        // link-capacity probe (`client/pump/data.rs`: 2 Gbps for 800ms, ~2s after
        // connect) — and core has exactly one `ProbeState` slot with no correlation id,
        // which our `request_probe` below would be sharing with it. Core defers its
        // probe while ours is active, but the reverse race (its probe landing just as
        // ours finishes and resetting the state we're about to read) is real. Pinning a
        // rate disarms core's probe entirely; the value is irrelevant since nothing is
        // decoded here.
        PROBE_SESSION_BITRATE_KBPS,
        quic::VIDEO_CAP_CHACHA20,
        2, // stereo baseline
        quic::CODEC_HEVC | quic::CODEC_H264,
        0,     // no preferred codec
        None,  // no HDR display metadata: nothing presents
        0,     // client_caps: nothing renders a cursor
        false, // frame_parts: whole AUs (see `connect`)
        None,  // no launch
        None,  // name: keep the host's fingerprint-derived label (see `connect`)
        pin,
        Some(identity),
        timeout,
    )
    .context("speed test connect")?;

    // The negotiated session, logged before the burst: if a measurement comes back
    // empty, this line is what says whether the connection itself was sane.
    tracing::info!(
        "speed test connected: codec={} audio_ch={} resolved_bitrate_kbps={} caps=0x{:02x}",
        client.codec,
        client.audio_channels,
        client.resolved_bitrate_kbps,
        quic::VIDEO_CAP_CHACHA20,
    );

    // Don't burst into a dead plane — see PROBE_WARMUP_CAP. `next_frame` drains the session's
    // decode-less video into the void; the first completed frame is the "plane is live" edge.
    let warmup = Instant::now();
    let mut warmed = false;
    while warmup.elapsed() < PROBE_WARMUP_CAP {
        if client.next_frame(Duration::from_millis(250)).is_ok() {
            warmed = true;
            break;
        }
    }
    tracing::info!(
        "speed test: data plane {} after {} ms",
        if warmed {
            "live"
        } else {
            "still silent (proceeding anyway)"
        },
        warmup.elapsed().as_millis(),
    );

    client
        .request_probe(PROBE_TARGET_KBPS, PROBE_DURATION_MS)
        .context("request_probe")?;
    // Flip the UI from "Connecting…" to "Measuring…" the moment the burst is requested —
    // with the warmup above, the first 250 ms poll is no longer the earliest signal.
    progress(client.probe_result());

    let deadline = Instant::now() + Duration::from_millis(u64::from(PROBE_DURATION_MS)) + PROBE_REPORT_GRACE;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let outcome = client.probe_result();
        if outcome.done {
            // Let the last in-flight UDP shards land before tearing the connection
            // down, so the delivered-bytes figure isn't cut short by our own exit.
            std::thread::sleep(Duration::from_millis(400));
            let final_outcome = client.probe_result();
            // Both sides of the measurement, separately. This is the line that tells a
            // host-side problem from a client-side one: `host_bytes == 0` means the host
            // never put filler on the wire (it ignored or couldn't serve the request),
            // whereas `host_bytes > 0` with `recv_bytes == 0` means it sent and we
            // received nothing usable — a network path or a decrypt mismatch, since
            // punktfunk-core counts bytes only AFTER a successful AEAD open.
            tracing::info!(
                "speed test result: recv_bytes={} recv_packets={} host_bytes={} host_packets={} \
                 elapsed_ms={} throughput_kbps={} loss_pct={:.2} host_drop_pct={:.2} \
                 wire_packets_sent={} send_dropped={}",
                final_outcome.recv_bytes,
                final_outcome.recv_packets,
                final_outcome.host_bytes,
                final_outcome.host_packets,
                final_outcome.elapsed_ms,
                final_outcome.throughput_kbps,
                final_outcome.loss_pct,
                final_outcome.host_drop_pct,
                final_outcome.wire_packets_sent,
                final_outcome.send_dropped,
            );
            client.disconnect_quit();
            return Ok(SpeedProbeResult {
                outcome: final_outcome,
                confirmed: true,
            });
        }
        progress(outcome);
        if Instant::now() > deadline {
            // The report never came — but `recv_bytes` is live during the burst (core
            // computes it as `rx_now - base`), so if a real amount of filler arrived the
            // measurement is not lost: divide it by the burst window we asked for. The
            // host honours that duration exactly when it does report (confirmed
            // on-device: a 3,000 ms request came back as `elapsed_ms=3000`), so this is
            // the same denominator, just not host-attested. Only the loss figure is
            // genuinely unavailable, since that needs the host's sent-packet count.
            let mut salvaged = client.probe_result();
            client.disconnect_quit();
            if salvaged.recv_bytes >= SALVAGE_MIN_BYTES {
                salvaged.elapsed_ms = PROBE_DURATION_MS;
                salvaged.throughput_kbps =
                    (salvaged.recv_bytes.saturating_mul(8) / u64::from(PROBE_DURATION_MS)) as u32;
                tracing::warn!(
                    "speed test: no host report; salvaged from {} received bytes over the {} ms \
                     burst window -> {} kbps (unconfirmed)",
                    salvaged.recv_bytes,
                    PROBE_DURATION_MS,
                    salvaged.throughput_kbps,
                );
                return Ok(SpeedProbeResult {
                    outcome: salvaged,
                    confirmed: false,
                });
            }
            anyhow::bail!(
                "the host never sent its result, and almost nothing arrived. The test burst can \
                 saturate the link the result has to come back over — try again, or move the TV \
                 closer to the access point."
            );
        }
    }
}

/// Suffix identifying a `GStreamer` pad-task thread (`"<element-name>:<pad-name>"`,
/// truncated to the kernel's 15-char `comm` limit) — the NDL vendor `.so` builds its
/// internal decode pipeline out of `GStreamer` elements, each with its own pad-task
/// thread spawned *inside our own process*. These are invisible to punktfunk-core's
/// hot-thread registry (that only covers threads this crate and punktfunk-core spawn
/// themselves) and sit at the default nice 0 despite doing real decode work — confirmed
/// via live `/proc/<pid>/task` sampling during an active NDL stream (its
/// `lxvideodec1:src`/`video-src:src` threads), a real contention cost against our own
/// already-boosted video-pump/data-pump threads on this `SoC`'s 3 cores. Matched by
/// suffix, not a fixed name list, so it covers whichever elements the pipeline uses.
const VENDOR_DECODE_THREAD_SUFFIX: &str = ":src";
/// How long a decode-thread scan may run with no new match before concluding the
/// backend's pipeline has finished spawning threads (typically well under this in
/// practice). Bounded separately by `VENDOR_DECODE_THREAD_SCAN_TIMEOUT` in case a
/// backend never produces a matching thread at all.
const VENDOR_DECODE_THREAD_QUIET_PERIOD: Duration = Duration::from_millis(500);
const VENDOR_DECODE_THREAD_SCAN_TIMEOUT: Duration = Duration::from_secs(5);

/// Renices the active backend's vendor-spawned `GStreamer` pad-task threads to -10, same
/// as this crate's own hot threads (see [`VENDOR_DECODE_THREAD_SUFFIX`]). Runs on its
/// own thread — these threads spawn asynchronously sometime after the decoder loads,
/// not synchronously within the load call, so this polls `/proc/self/task` rather than
/// scanning once, and must not block `video_pump` from starting to feed frames while it
/// does.
fn spawn_vendor_decode_thread_renicer() {
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut last_found = start;
        let mut failed: usize = 0;
        let mut reniced: std::collections::HashSet<i32> = std::collections::HashSet::new();
        loop {
            if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
                for entry in entries.flatten() {
                    let Ok(tid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                        continue;
                    };
                    if reniced.contains(&tid) {
                        continue;
                    }
                    let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
                        continue;
                    };
                    let comm = comm.trim();
                    if !comm.ends_with(VENDOR_DECODE_THREAD_SUFFIX) {
                        continue;
                    }
                    reniced.insert(tid);
                    last_found = Instant::now();
                    // SAFETY: plain syscall — tid and priority value only, no pointers.
                    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, -10) } != 0 {
                        failed += 1;
                        tracing::warn!(
                            "setpriority(vendor thread {comm}, tid={tid}) failed: {}",
                            std::io::Error::last_os_error()
                        );
                    } else {
                        tracing::debug!("reniced vendor decode thread {comm} (tid={tid}) to -10");
                    }
                }
            }
            let now = Instant::now();
            let quiet = !reniced.is_empty() && now.duration_since(last_found) >= VENDOR_DECODE_THREAD_QUIET_PERIOD;
            if quiet || now.duration_since(start) >= VENDOR_DECODE_THREAD_SCAN_TIMEOUT {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // One summarizing line for the same reason as the hot-thread summary in
        // `video_pump`: whether the boost applied at all is the install-mode question
        // a session log has to answer.
        tracing::info!(
            "vendor decode threads: {} found, {} boosted",
            reniced.len(),
            reniced.len().saturating_sub(failed),
        );
    });
}

fn video_pump(
    client: Arc<NativeClient>,
    mut sink: NdlSink,
    stop: Arc<AtomicBool>,
    stats: Arc<StreamStats>,
    is_hdr: bool,
) {
    client.register_hot_thread();
    // Summarized at info, not left as per-tid debug lines: whether these renices work at
    // all is install-mode-dependent (they need CAP_SYS_NICE or a nonzero RLIMIT_NICE —
    // present on a rooted install, absent under a plain Dev-Mode SAM jail), and a session
    // log that doesn't answer "did the priority boost actually apply here" hides the
    // difference between the two contention regimes docs/NOTES.md's renice findings were
    // measured under.
    let (mut reniced, mut failed) = (0u32, 0u32);
    for tid in client.hot_thread_ids() {
        // SAFETY: plain syscall — tid and priority value only, no pointers.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, -10) } == 0 {
            reniced += 1;
        } else {
            failed += 1;
            tracing::debug!("setpriority(tid={tid}) failed: {}", std::io::Error::last_os_error());
        }
    }
    tracing::info!(
        "hot-thread renice: {reniced} boosted, {failed} failed{}",
        if failed > 0 {
            " (no CAP_SYS_NICE — priorities unchanged)"
        } else {
            ""
        },
    );
    spawn_vendor_decode_thread_renicer();

    let mut last_dropped_seen = client.frames_dropped();
    let mut frames_received: u64 = 0;
    let mut last_heartbeat = Instant::now();
    let mut last_video_log = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        match client.next_frame(Duration::from_millis(500)) {
            Ok(frame) => {
                frames_received += 1;
                stats.frames.store(frames_received, Ordering::Relaxed);
                stats.bytes.fetch_add(frame.data.len() as u64, Ordering::Relaxed);
                if last_heartbeat.elapsed() >= Duration::from_secs(2) {
                    last_heartbeat = Instant::now();
                    let backlog = sink.poll_backlog_depth();
                    stats.render_backlog.store(backlog.unwrap_or(-1), Ordering::Relaxed);
                    // `backlog` separates "the decoder is behind" from "frames are
                    // arriving late" — indistinguishable before this, since play()
                    // decodes and presents in one opaque call. Logged on its own slower
                    // cadence: the overlay wants a fresh depth, the log does not.
                    //
                    // DEBUG, so it costs a telemetry listener or `TELEMETRY_LEVEL=debug` to
                    // see — the on-device file sink is INFO-only (`logger::resolved_level`).
                    // 15s: the line is a trend ("still draining, still not holding"), and one
                    // every couple of seconds buried the rest of the log saying nothing new.
                    if last_video_log.elapsed() >= Duration::from_secs(15) {
                        last_video_log = Instant::now();
                        tracing::debug!(
                            "video: {frames_received} frames, holding={}, dropped={}, backlog={}",
                            sink.holding(),
                            client.frames_dropped(),
                            backlog.map_or_else(|| "n/a".to_string(), |b| b.to_string()),
                        );
                    }
                }

                // From core v0.28 this returns the gap WIDTH (0 = contiguous) where it used to
                // return a bare "was there a gap" bool; `> 0` is the same predicate. Keep the
                // width for the log line — how many frames the hole swallowed is the number
                // worth having when reading a freeze report, not merely that one existed.
                let gap_width = client.note_frame_index(frame.frame_index);
                let gap = gap_width > 0;
                let dropped_now = client.frames_dropped();
                let dropped = dropped_now > last_dropped_seen;
                if dropped {
                    last_dropped_seen = dropped_now;
                }
                if (gap || dropped) && !sink.holding() {
                    // Logged alongside the freeze the sink reports next: a sequence hole and a
                    // frame the transport itself gave up on point at different faults.
                    tracing::warn!("loss: gap={gap_width} dropped={dropped} (frame {})", frame.frame_index);
                }
                let flags = FrameFlags {
                    reanchor: frame.flags & u32::from(FLAG_SOF) != 0 || frame.flags & USER_FLAG_RECOVERY_ANCHOR != 0,
                    loss: gap || dropped,
                    index: u64::from(frame.frame_index),
                };
                match sink.submit(&frame.data, frame.pts_ns, flags) {
                    SinkResult::Presented { decode_us } => {
                        if let Some(us) = decode_us {
                            client.report_decode_us(us);
                        }
                    }
                    SinkResult::Held => {}
                    SinkResult::NeedKeyframe => {
                        if let Err(e) = client.request_keyframe() {
                            tracing::warn!("request_keyframe: {e:#}");
                        }
                    }
                }
            }
            Err(punktfunk_core::PunktfunkError::NoFrame) => {
                if last_heartbeat.elapsed() >= Duration::from_secs(2) {
                    last_heartbeat = Instant::now();
                    // INFO for the same reason as the main heartbeat above — and this
                    // arm is the one that says "nothing is arriving at all", which is a
                    // different fault from "arriving but not presenting".
                    tracing::info!("video: {frames_received} frames (idle)");
                }
            }
            // A teardown the user asked for reaches both pumps as `Closed`, so it is not an
            // error in either — the audio pump already logged it at INFO.
            Err(punktfunk_core::PunktfunkError::Closed) => {
                tracing::info!("video pump ending: session closed");
                break;
            }
            Err(e) => {
                tracing::error!("video pump: {e:#}");
                break;
            }
        }

        if is_hdr {
            // Freshly *received* is not the same as changed: the host re-sends unchanged
            // mastering metadata (three identical packets inside 10 ms on a CX), so the on-change
            // filter has to run against the last value applied. The player does that.
            if let Ok(meta) = client.next_hdr_meta(Duration::ZERO) {
                tracing::info!(
                    "HDR metadata received: primaries={:?} white={:?} max_dml={} min_dml={} max_cll={} max_fall={}",
                    meta.display_primaries,
                    meta.white_point,
                    meta.max_display_mastering_luminance,
                    meta.min_display_mastering_luminance,
                    meta.max_cll,
                    meta.max_fall,
                );
                if let Err(e) = sink.set_color_info(Some(&meta), client.color) {
                    tracing::warn!("NDL set_color_info: {e:#}");
                }
            }
        }
    }
}

/// Drains raw Opus packets straight into NDL on a dedicated thread, for the offloaded
/// path. (No main-thread constraint applies here — that's `sdl2::audio::AudioQueue`
/// being `!Send`, and there is no `AudioQueue` on this path.)
///
/// A dedicated thread, not a drain bolted onto the video pump loop (where this first
/// lived): there, audio only drained after a `next_frame` call that blocks up to
/// 500 ms, so a video drought — an encoder stall on the host, a loss hold — chopped
/// audio into ≤500 ms stalls *with packets already waiting*, and in normal flow
/// packets drained in per-video-frame clumps that all took the same drain-time PTS.
/// Core's `next_audio` docs ask for exactly this thread ("packets arrive every 5 ms"),
/// and its pull methods are one-thread-per-plane safe by contract.
///
/// Teardown safety: this thread holds one of the two `Arc<NdlVideo>` owners, so the
/// process-global NDL unload in `NdlVideo::drop` cannot run until this thread has
/// exited — `NDL_DirectAudioPlay` can never race the unload, whichever thread
/// `Connected::shutdown` happens to join first.
fn ndl_audio_pump(client: &NativeClient, ndl: &NdlVideo, stop: &AtomicBool) {
    // Same boost the video pump requests for itself — 5 ms packets are the most
    // latency-sensitive cadence in the session. Best-effort, like every renice here.
    // SAFETY: plain syscall — tid 0 (self) and priority value only, no pointers.
    let _ = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, -10) };
    while !stop.load(Ordering::Relaxed) {
        match client.next_audio(Duration::from_millis(100)) {
            Ok(packet) => {
                if let Err(e) = ndl.play_audio(&packet.data, packet.pts_ns) {
                    tracing::warn!("NDL audio error (seq {}): {e:#}", packet.seq);
                }
            }
            Err(punktfunk_core::PunktfunkError::NoFrame) => {}
            Err(e) => {
                tracing::info!("audio pump ending: {e:#}");
                break;
            }
        }
    }
}

/// Spawns the dedicated audio decode/feed thread and returns its handle.
///
/// A thread of its own, not a drain bolted onto the main loop. That is where this lived, forced by
/// `sdl2::audio::AudioQueue` being `!Send` — which put the session's 5 ms audio cadence behind the
/// UI's software rasterizer on a 2-3 core panel, and `docs/NOTES.md` already named the 500 ms
/// stats-overlay raster as an underrun source because of it. The offloaded path
/// ([`ndl_audio_pump`]) worked this way for the same reason, and core's `next_audio` docs ask for
/// exactly this thread ("packets arrive every 5 ms").
pub fn spawn_audio_feed(
    client: Arc<NativeClient>,
    mut feed: crate::platform::webos::audio::AudioFeed,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("punktfunk-webos-audio".into())
        .spawn(move || audio_feed_pump(&client, &mut feed, &stop))
        .context("spawn audio feed thread")
}

/// Joins the audio feed thread, bounded by the same timeout every other teardown join uses — a
/// thread wedged in an Opus decode must not hold the whole app on the way back to the menu.
/// Software Opus → SDL2, not NDL, so a wedge here needs no `ndl::poison()`.
pub fn join_audio_feed(handle: std::thread::JoinHandle<()>) -> bool {
    join_with_timeout(handle, SHUTDOWN_JOIN_TIMEOUT, "audio-feed", || ())
}

/// Pulls Opus packets off the transport, decodes them, and hands the PCM to the playback ring.
fn audio_feed_pump(client: &NativeClient, feed: &mut crate::platform::webos::audio::AudioFeed, stop: &AtomicBool) {
    // Same boost the video pump requests for itself — 5 ms packets are the most latency-sensitive
    // cadence in the session. Best-effort, like every renice here.
    // SAFETY: plain syscall — tid 0 (self) and priority value only, no pointers.
    let _ = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, -10) };
    let mut packets: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        match client.next_audio(Duration::from_millis(100)) {
            Ok(packet) => match feed.play(packet.seq, packet.pts_ns, &packet.data) {
                Ok(peak) => {
                    packets = packets.wrapping_add(1);
                    // ~15s, matching the video heartbeat (packets are 5ms each).
                    if packets % 3_000 == 0 {
                        tracing::debug!("audio peak: {peak:.4}");
                    }
                }
                // Underruns and drift sheds are reported by the ring itself, which is the only
                // side that knows the depth — see `platform::webos::audio`'s callback.
                Err(e) => tracing::warn!("audio error (seq {}): {e:#}", packet.seq),
            },
            Err(punktfunk_core::PunktfunkError::NoFrame) => {}
            Err(e) => {
                tracing::info!("audio feed ending: {e:#}");
                break;
            }
        }
    }
}

/// Sends one input event to the host.
pub fn send_input(client: &NativeClient, ev: &InputEvent) -> Result<()> {
    client.send_input(ev).context("send_input")
}

/// Sends one rich-input report (pad touchpad contacts) to the host.
pub fn send_rich_input(client: &NativeClient, input: quic::RichInput) -> Result<()> {
    client.send_rich_input(input).context("send_rich_input")
}

/// Ceiling on feedback events handled per tick.
///
/// Both planes are human-paced (a rumble change, a weapon swap), so this is never reached in
/// normal play — it exists so a host that floods, or a plane that backed up while a modal was
/// open, cannot starve rendering and input for a tick.
const FEEDBACK_DRAIN_BUDGET: usize = 32;

/// Drains the host→client gamepad feedback planes (non-blocking) and applies them to the
/// physical pad. Call once per main-loop tick.
///
/// The two planes go to different places, because each has one route that works for every
/// controller rather than only one:
///   * **rumble** → SDL's evdev force feedback (`GameController::set_rumble`, plus
///     `set_rumble_triggers` for the impulse-trigger motors on pads that have them), which works
///     on any pad the TV has bound, `DualSense` included;
///   * **`DualSense` HID feedback** (adaptive triggers, lightbar, player LEDs) → the Bluetooth
///     service, since SDL's own `DualSense` path needs a hidraw node the app's jail doesn't have
///     (see [`crate::platform::webos::dualsense`]).
///
/// Both drains run even when their sink is absent: the planes are bounded queues, and leaving
/// one unread would let it fill and then discard the *newest* events — including, for rumble,
/// the zero that stops a motor.
pub fn pump_feedback_once(
    client: &NativeClient,
    mut controller: Option<&mut sdl2::controller::GameController>,
    mut feedback: Option<&mut crate::platform::webos::dualsense::Feedback>,
) {
    // `next_rumble_command` is the policy-engine API: it already resolves lease expiry, stale
    // legacy hosts and close-drain zeros, so commands apply verbatim — all-zero stops now.
    //
    // Queried once per tick, not per command: SDL walks its joystick list for this, and a hotplug
    // arrives as a new `GameController` rather than changing this answer mid-drain.
    let has_triggers = controller
        .as_deref()
        .is_some_and(sdl2::controller::GameController::has_rumble_triggers);
    let mut budget = FEEDBACK_DRAIN_BUDGET;
    while budget > 0 {
        let Ok(cmd) = client.next_rumble_command(Duration::ZERO) else {
            break; // NoFrame (empty) or Closed (session over)
        };
        budget -= 1;
        if let Some(pad) = controller.as_deref_mut() {
            // `backstop_ms` passes straight through, including 0: SDL2 reads a zero duration as
            // "no expiration" (`rumble_expiration = 0`, run until changed), not "stop now", which
            // is exactly the semantics wanted here — the policy engine guarantees an explicit
            // zero-level command at every stop, so a self-expiring effect would only risk
            // cutting a held rumble short. Don't "fix" this into a floor.
            //
            // Errors here are the common "this pad has no rumble motors" case, not a fault:
            // logging per command would spam a tick loop, and there is no recovery to attempt.
            let _ = pad.set_rumble(cmd.low, cmd.high, cmd.backstop_ms);
            // Dropping the trigger pair on a pad without those motors is the correct degrade;
            // folding it into the handles would turn a racing title's continuous trigger stream
            // into a handle motor droning flat-out for the whole race.
            if has_triggers {
                let _ = pad.set_rumble_triggers(cmd.left_trigger, cmd.right_trigger, cmd.backstop_ms);
            }
        }
    }

    let mut budget = FEEDBACK_DRAIN_BUDGET;
    while budget > 0 {
        let Ok(event) = client.next_hidout(Duration::ZERO) else {
            break;
        };
        budget -= 1;
        if let Some(fb) = feedback.as_deref_mut() {
            fb.apply(&event);
        }
    }
}
