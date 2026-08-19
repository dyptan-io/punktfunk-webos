//! Bringing up a streaming session: capability negotiation, the decoder load, and the
//! pump threads that carry it.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use punktfunk_core::client::NativeClient;
use punktfunk_core::config::{CompositorPref, Mode};
use punktfunk_core::quic;

use crate::core::caps::video_caps;
use crate::platform::webos::device::{self, NdlGeneration};
use crate::platform::webos::ndl::v1::NdlV1Video;
use crate::platform::webos::ndl::{NdlAudioConfig, NdlCodec, NdlVideo};
use crate::services::store::{CodecPref, GamepadType, VideoBackend};
use crate::session::join::{join_with_timeout, SHUTDOWN_JOIN_TIMEOUT};
use crate::session::pump::{ndl_audio_pump, video_pump};
use crate::session::sink::{NdlSink, SinkConfig, VideoPlayer};
use crate::session::StreamStats;

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

impl Connected {
    /// Stop and join threads, then drop `NativeClient`. Call `disconnect_quit()` first for
    /// graceful shutdown. Returns `false` if any step didn't finish within
    /// `SHUTDOWN_JOIN_TIMEOUT` — the caller must then skip `ndl::quit()`, since the thread
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
fn cx_display_hdr() -> quic::HdrMeta {
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

/// Everything [`connect`] needs from the chosen target and the user's settings.
///
/// A struct rather than a parameter list: every field comes from exactly those two places, and a
/// positional list of fifteen mostly-scalar arguments is one where a swapped pair still compiles.
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    /// Requested capture mode; the host clamps it and echoes the result in `client.mode()`.
    pub mode: Mode,
    pub bitrate_kbps: u32,
    pub hdr_enabled: bool,
    pub audio_channels: u8,
    /// This client's TLS identity, `(cert_pem, key_pem)`.
    pub identity: (String, String),
    /// Trusted host fingerprint from a prior pairing; `None` = trust-on-first-use.
    pub pin: Option<[u8; 32]>,
    /// Game/app handle for the host to launch once the session is up.
    pub launch: Option<String>,
    /// Handshake budget.
    pub timeout: Duration,
    pub codec: CodecPref,
    pub video_backend: VideoBackend,
    pub gamepad_type: GamepadType,
    pub cursor_capture: bool,
    pub ndl_audio_offload: bool,
}

/// One `quic::CODEC_*` bit, or 0 where the preference names no single codec.
fn codec_bit(pref: CodecPref) -> u8 {
    match pref {
        CodecPref::Auto => 0,
        CodecPref::H264 => quic::CODEC_H264,
        CodecPref::Hevc => quic::CODEC_HEVC,
    }
}

/// What this client advertises on the wire, clamped to what the TV can actually decode.
struct Negotiated {
    audio_channels: u8,
    /// `quic::VIDEO_CAP_*` bitfield.
    video_caps: u8,
    /// `quic::CODEC_*` bitfield: every codec this client can present.
    video_codecs: u8,
    /// A single `quic::CODEC_*` bit, or 0 for auto.
    preferred_codec: u8,
    display_hdr: Option<quic::HdrMeta>,
}

impl Negotiated {
    /// **The authoritative capability gate.** Codec, colour path and channel count are settled by
    /// the handshake, BEFORE any decoder opens, so a document carried over from a more capable TV
    /// must be clamped here and not merely hidden in the UI: HEVC negotiated onto an H.264-only
    /// decoder is a frozen black stream with no second chance once `Welcome` has resolved.
    fn clamp(params: &ConnectParams) -> Self {
        let caps = video_caps();
        let codecs = caps.codec_prefs();
        let codec_pref = if codecs.contains(&params.codec) {
            params.codec
        } else {
            codecs[0]
        };
        // HDR only ever applies to HEVC. An explicit H.264 pick disables it end to end
        // (the Settings toggle is hidden too — see `ui::settings`'s `row_shown`); on Automatic the
        // caps are still advertised and the host resolves the codec, with application gated
        // on the *negotiated* codec being HEVC in `load_player`.
        let hdr = params.hdr_enabled && caps.hdr && codec_pref != CodecPref::H264;
        Self {
            audio_channels: params.audio_channels.min(caps.max_channels),
            // VIDEO_CAP_CHACHA20: unconditional — armv7 has no hardware AES, so ChaCha20 is
            // faster. A ≥0.17.2 host picks it up; older hosts ignore the unknown bit.
            video_caps: quic::VIDEO_CAP_CHACHA20
                | if hdr {
                    quic::VIDEO_CAP_10BIT | quic::VIDEO_CAP_HDR
                } else {
                    0
                },
            // Advertised decode set folded from the one codec list (`codec_prefs`) so the host's
            // precedence ladder can never auto-pick a path this client can't present.
            video_codecs: codecs.iter().fold(0, |set, &pref| set | codec_bit(pref)),
            preferred_codec: codec_bit(codec_pref),
            display_hdr: hdr.then(cx_display_hdr),
        }
    }
}

/// Runs the handshake. Everything wire-facing has already been clamped by [`Negotiated::clamp`].
fn dial(params: &ConnectParams, negotiated: &Negotiated) -> Result<NativeClient> {
    NativeClient::connect(
        &params.host,
        params.port,
        params.mode,
        CompositorPref::Auto,
        // Session-default pad kind. A per-pad `InputKind::GamepadArrival` could override this
        // for mixed setups, but this client drives one pad (index 0), for which the handshake
        // default is exactly equivalent — and it also reaches hosts too old to advertise
        // `HOST_CAP_GAMEPAD_STATE`.
        params.gamepad_type.to_core(),
        params.bitrate_kbps,
        negotiated.video_caps,
        // Requested only — the host clamps to what it can capture, and
        // `AudioPlayer::new` is built from the RESOLVED `client.audio_channels`,
        // never from this.
        negotiated.audio_channels,
        negotiated.video_codecs,
        negotiated.preferred_codec,
        negotiated.display_hdr,
        // client_caps: see `store::Settings::cursor_capture` for the on/off split.
        if params.cursor_capture {
            0
        } else {
            quic::CLIENT_CAP_CURSOR
        },
        // frame_parts: NDL DirectMedia takes whole access units only — it has no
        // `PARTIAL_FRAME` equivalent, so slice-progressive prefixes would have nowhere to go.
        false,
        params.launch.clone(),
        // Device name for the host's pending-approval list. `None` keeps the host's
        // fingerprint-derived label ("device abcd1234"), i.e. exactly the behaviour before
        // core gained this parameter — sending a real TV name is a separate, user-visible
        // change and does not belong in a dependency bump.
        None,
        params.pin,
        Some(params.identity.clone()),
        params.timeout,
    )
    .context("connect")
}

/// The one line that says what the handshake actually settled on.
fn log_handshake(client: &NativeClient, negotiated: &Negotiated) {
    let fp_hex = client.host_fingerprint.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    tracing::info!(
        "connected: codec={} (offered=0x{:02x} preferred=0x{:02x}) \
         compositor={:?} audio_ch={} color={:?} bitrate_kbps={} \
         decode_latency={} caps=0x{:02x} fp={fp_hex}",
        client.codec,
        negotiated.video_codecs,
        negotiated.preferred_codec,
        client.resolved_compositor,
        client.audio_channels,
        client.color,
        client.resolved_bitrate_kbps,
        client.wants_decode_latency(),
        negotiated.video_caps,
    );
}

/// Opens the decoder for the negotiated stream and hands it the colorimetry.
///
/// Returns the player and whether HDR mastering metadata is being applied — the answer the
/// video pump needs to know whether to forward per-content metadata at all.
fn load_player(params: &ConnectParams, client: &NativeClient) -> Result<(VideoPlayer, bool)> {
    let resolved_mode = client.mode();
    let fps = resolved_mode.refresh_hz.max(1);
    let codec =
        NdlCodec::from_wire(client.codec).with_context(|| format!("unsupported codec 0x{:02x}", client.codec))?;
    let app_id = crate::platform::webos::ndl::app_id();
    let (width, height) = (resolved_mode.width as i32, resolved_mode.height as i32);
    // Every V2 load asks for a plane: a fed one is what makes NDL pace the picture at all
    // (docs/NOTES.md § "NDL's audio plane"). What rides it is decided at the pump. Stereo either
    // way — the silent frame's TOC declares stereo, and a software-decode session's plane never
    // sees the real stream. A set that refuses the load falls back to video-only in
    // `NdlVideo::load`, and gives up pacing with it.
    let ndl_audio = NdlAudioConfig {
        channels: 2,
        // kHz, not Hz — NDL's own unit, and what ss4s passes (`info->sampleRate / 1000.0`).
        // punktfunk's audio plane is fixed at 48 kHz (see `audio.rs`'s SAMPLE_RATE).
        sample_rate: 48.0,
    };
    // SMP is only selectable where NDL is the narrow v1 generation (`core::caps::smp_selectable`),
    // so trying it can't displace the v2 path. A load that fails falls back to NDL, but only
    // H.264 survives that — v1 decodes nothing else.
    let smp = (crate::core::caps::effective_backend(params.video_backend) == VideoBackend::Smp)
        .then(|| crate::platform::webos::smp::SmpVideo::load(&app_id, width, height, fps, codec))
        .transpose()
        .unwrap_or_else(|e| {
            tracing::warn!("SMP load failed ({e:#}) — falling back to NDL");
            None
        });
    let player = match (smp, device::ndl_generation()) {
        (Some(sf), _) => VideoPlayer::Smp(sf),
        (None, NdlGeneration::V2) => VideoPlayer::V2(Arc::new(
            NdlVideo::load(&app_id, width, height, codec, Some(ndl_audio)).context("NDL load")?,
        )),
        (None, NdlGeneration::V1) => {
            VideoPlayer::V1(NdlV1Video::load(&app_id, width, height, codec).context("NDL v1 load")?)
        }
    };
    tracing::info!(
        "{} loaded ({codec:?} {}x{}@{fps}fps)",
        player.backend_name(),
        resolved_mode.width,
        resolved_mode.height,
    );

    // HDR mastering metadata is applied only when the *negotiated* codec is HEVC: the
    // `NdlHdrInfo`/`setHdrInfo` fields are HEVC SEI syntax, and no other codec carries
    // HDR on this platform.
    let host_hdr = client.color.is_hdr();
    let is_hdr = host_hdr && matches!(codec, NdlCodec::H265);
    // What the host signalled in `Welcome`, before the SDR colorimetry fix below acts on it.
    tracing::info!(
        "host colour info: hdr={host_hdr} apply_hdr={is_hdr} codec={codec:?} transfer={} primaries={} matrix={}",
        client.color.transfer,
        client.color.primaries,
        client.color.matrix,
    );
    // Forward the negotiated colorimetry to the decoder for BOTH HDR and SDR
    // streams. The SDR case is not optional: punktfunk encodes BT.709, but with
    // missing/"unspecified" VUI colour info in the bitstream this panel guesses
    // colorimetry from resolution — a 4K SDR stream then decodes as BT.2020,
    // which shows up as exactly the washed-out/desaturated picture reported
    // on-device. `client.color` arrives out-of-band in `Welcome` for precisely
    // this purpose; only the mastering metadata alongside it is HDR-gated.
    if let Err(e) = player.set_color_info(is_hdr.then(cx_display_hdr).as_ref(), client.color) {
        tracing::warn!("NDL colour metadata failed: {e:#}");
    }
    Ok((player, is_hdr))
}

/// Why this session's audio ended up on the path it did.
///
/// Naming the REASON matters: "software Opus" is the correct outcome on four different routes
/// plus the user's own override, and a silent session looks identical on all of them. Without
/// this the first debugging question has no answer in the log.
fn audio_path_label(player: &VideoPlayer, has_plane: bool, offload_opt_in: bool, offloaded: bool) -> &'static str {
    match (has_plane, player) {
        _ if offloaded => "NDL hardware Opus decode",
        // A plane the real stream is not using is the pacing metronome — see
        // `NdlVideo::run_clock_plane`.
        (true, _) if !offload_opt_in => "software Opus decode -> SDL2 + NDL clock plane (offload not opted in)",
        (true, _) => "software Opus decode -> SDL2 + NDL clock plane (NDL Opus is stereo-only)",
        (false, VideoPlayer::V1(_)) => "software Opus decode -> SDL2, no clock plane (NDL v1 has no Opus audio type)",
        (false, VideoPlayer::Smp(_)) => "software Opus decode -> SDL2, no clock plane (SMP loads video-only)",
        // No plane on a V2 load means the audio-enabled attempt did not confirm and `load()` fell
        // back to video-only, so this session has no pacing reference either.
        (false, VideoPlayer::V2(_)) => "software Opus decode -> SDL2, no clock plane (NDL rejected the audio load)",
    }
}

/// Connects to a punktfunk host and starts the video pump thread.
///
/// Blocks until the handshake completes or `params.timeout` elapses. NDL manages its own
/// punch-through area natively (see [`crate::platform::webos::ndl`]'s module docs), so no
/// display geometry is needed here.
pub fn connect(params: ConnectParams) -> Result<Connected> {
    // Fails before touching the network: a full handshake would only end in `NdlVideo::load()`
    // rejecting the same gate, pointlessly holding the host's pending-session slot for `timeout`.
    crate::platform::webos::ndl::ensure_not_poisoned()?;
    let negotiated = Negotiated::clamp(&params);
    let client = Arc::new(dial(&params, &negotiated)?);
    log_handshake(&client, &negotiated);

    let (player, is_hdr) = load_player(&params, &client)?;
    let ndl_audio = player.ndl_audio_handle();
    // Whether the REAL stream rides the plane. `ndl_audio.is_some()` is a different question —
    // it only says the load HAS a plane, which every accepted V2 load does now.
    let audio_offloaded = ndl_audio.is_some() && params.ndl_audio_offload && client.audio_channels == 2;
    tracing::info!(
        "audio path: {} (host resolved {} channel(s))",
        audio_path_label(&player, ndl_audio.is_some(), params.ndl_audio_offload, audio_offloaded),
        client.audio_channels,
    );

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(StreamStats::default());
    let video_thread = spawn_video_thread(&client, player, &stop, &stats, is_hdr)?;
    // Failing here after the video thread is already up would otherwise detach it: `Connected` is
    // never built, so nothing ever sets `stop`, and a thread still feeding NDL outlives the error
    // the caller sees — which then races the `ndl::quit()` the failed connect leads to.
    let audio_thread = match spawn_audio_thread(&client, ndl_audio, &stop, audio_offloaded) {
        Ok(handle) => handle,
        Err(e) => {
            stop.store(true, Ordering::Relaxed);
            join_with_timeout(
                video_thread,
                SHUTDOWN_JOIN_TIMEOUT,
                "video",
                crate::platform::webos::ndl::poison,
            );
            return Err(e);
        }
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

fn spawn_video_thread(
    client: &Arc<NativeClient>,
    player: VideoPlayer,
    stop: &Arc<AtomicBool>,
    stats: &Arc<StreamStats>,
    is_hdr: bool,
) -> Result<std::thread::JoinHandle<()>> {
    let cfg = SinkConfig {
        stream_hz: client.mode().refresh_hz,
        report_decode_latency: client.wants_decode_latency(),
        clock_offset: client.clock_offset_shared(),
        video_e2e: client.video_e2e_shared(),
    };
    let (client, stop, stats) = (client.clone(), stop.clone(), stats.clone());
    std::thread::Builder::new()
        .name("punktfunk-webos-video".into())
        .spawn(move || {
            // Built here, not on the caller's thread: the sink queries the panel refresh
            // rate through SDL on construction, and that stayed on the video thread before.
            let sink = NdlSink::new(player, stats.clone(), cfg);
            video_pump(client, sink, stop, stats, is_hdr);
        })
        .context("spawn video thread")
}

/// The thread on NDL's audio plane, if this load got one: the real Opus stream when it is
/// offloaded, the pacing metronome otherwise.
fn spawn_audio_thread(
    client: &Arc<NativeClient>,
    ndl_audio: Option<Arc<NdlVideo>>,
    stop: &Arc<AtomicBool>,
    audio_offloaded: bool,
) -> Result<Option<std::thread::JoinHandle<()>>> {
    let Some(ndl) = ndl_audio else {
        return Ok(None);
    };
    let stop = stop.clone();
    let handle = if audio_offloaded {
        let client = client.clone();
        std::thread::Builder::new()
            .name("punktfunk-webos-audio".into())
            .spawn(move || ndl_audio_pump(&client, &ndl, &stop))
    } else {
        // Software decode owns the speakers, so this plane is a metronome. Nothing is consumed
        // twice: the real packets still go to the audio feed pump, and this one generates its
        // own cadence off the player clock.
        std::thread::Builder::new()
            .name("punktfunk-webos-clock".into())
            .spawn(move || ndl.run_clock_plane(&stop))
    };
    handle.map(Some).context("spawn audio plane thread")
}
