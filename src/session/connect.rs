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
use crate::session::pump::{ndl_audio_pump, ndl_pcm_audio_pump, video_pump};
use crate::session::sink::{NdlSink, SinkConfig, VideoPlayer};
use crate::session::StreamStats;

pub struct Connected {
    pub client: Arc<NativeClient>,
    pub stop: Arc<AtomicBool>,
    /// Live pump counters for stats overlay; see `StreamStats`.
    pub stats: Arc<StreamStats>,
    /// Kept alive so `shutdown()` can join and ensure QUIC close frame is sent before exit.
    video_thread: std::thread::JoinHandle<()>,
    /// Forwards the real Opus stream onto NDL's audio plane. `Some` only on the offloaded path.
    audio_thread: Option<std::thread::JoinHandle<()>>,
    /// `NdlVideo::run_clock_plane`, on every V2 load that got a plane. `None` only when the load
    /// has no audio plane at all (V1, SMP, or a rejected audio load).
    clock_thread: Option<std::thread::JoinHandle<()>>,
    /// Where this session's audio actually goes — see [`AudioRoute`].
    pub audio_route: AudioRoute,
    /// Whether HDR mastering metadata is being applied this session (negotiated codec is
    /// HEVC *and* the host signalled HDR). Drives which Game picture mode the runtime asks
    /// the TV for — `game` vs `hdrGame` (see `platform::webos::game_mode`).
    pub hdr: bool,
}

/// Widest layout NDL's audio plane has a mode for — mirrors `platform::webos::audio`'s own
/// constant, and the reason a 7.1 session loads a 6-channel plane.
const PLANE_MAX_CHANNELS: u8 = 6;

/// Which of the three audio paths a session settled on.
///
/// All three exist because they trade differently, and the trade is measured in latency: the SDL
/// ring is the only one this client can steer (a de-jitter policy, concealment, any layout), and
/// the two NDL routes give that up for a shorter path onto the panel's own clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioRoute {
    /// Software Opus → a bare SDL device, with NDL's clock plane on its metronome — **the
    /// default**, and also what any load with no audio plane falls back to (an audio-enabled load
    /// the set refused, NDL v1, SMP). The longest of the three paths, and the only one whose
    /// pacing behaviour is proven on hardware.
    Software,
    /// The wire's Opus, decoded by the TV on its audio plane (`Settings::audio_route`).
    /// No local decode at all, and no SDL device.
    NdlOpus,
    /// Software Opus, then straight onto NDL's PCM plane. No SDL device; decode, concealment and
    /// layout stay local, and the samples land on the picture's own clock. Selectable, not the
    /// default — see [`AudioRoute::pick`].
    NdlPcm,
}

impl AudioRoute {
    /// Whether the real stream rides NDL's audio plane — i.e. no SDL device is opened, and the
    /// clock plane is a standby filler rather than the only feed.
    pub fn on_ndl_plane(self) -> bool {
        self != Self::Software
    }

    /// Picks the route a session WANTS. The load happens next and decides whether the plane exists
    /// at all — [`AudioRoute::on_plane`] applies that answer, and [`Self::Software`] is what is left
    /// when it doesn't.
    ///
    /// **Software is the default.** It is the only shape whose pacing is known good: NDL paces the
    /// picture against a FED audio plane, and a plane fed from the network inherits the stream's
    /// arrival jitter — which is the stutter the silent clock plane was introduced to cure. The
    /// two plane routes are shorter and are kept selectable for exactly that comparison; until one
    /// of them is measured better on real hardware, the metronome keeps the plane and the audio
    /// takes the longer path.
    fn pick(params: &ConnectParams, channels: u8) -> Self {
        use crate::services::store::AudioRoutePref;
        match params.audio_route {
            AudioRoutePref::NdlPcm => Self::NdlPcm,
            // Stereo or nothing: `Settings::clamp` already holds the document to it, and a
            // session the host resolved wider must not silently land on a plane that would read
            // the interleave at the wrong stride — it falls back to software instead.
            AudioRoutePref::NdlOpus if channels == 2 => Self::NdlOpus,
            AudioRoutePref::Software | AudioRoutePref::NdlOpus => Self::Software,
        }
    }

    /// How the stats overlay names this route. Which decoder ran leads the line: the paths fail
    /// differently, and reading the numbers without knowing which produced them has already cost
    /// real debugging time.
    pub fn overlay_tag(self) -> &'static str {
        match self {
            Self::Software => "Opus SW",
            Self::NdlOpus => "Opus HW",
            Self::NdlPcm => "PCM HW",
        }
    }

    /// Downgrades to software when the load came back with no audio plane — a rejected
    /// audio-enabled load (`NdlVideo::load` falls back to video-only), V1, or SMP.
    fn on_plane(self, has_plane: bool) -> Self {
        if has_plane {
            self
        } else {
            Self::Software
        }
    }

    /// The plane format this route needs at load time. Every V2 load asks for a plane whatever the
    /// route — NDL only paces the picture against a fed one — so the software route still loads
    /// Opus, for `run_clock_plane`'s metronome to ride.
    fn plane_config(self, channels: u8) -> NdlAudioConfig {
        match self {
            // The plane's widest mode is 6 channels, so a 7.1 session loads 5.1 and `PcmFeed`
            // folds the sides in. Both sides must agree on that width or NDL reads the interleave
            // at the wrong stride.
            Self::NdlPcm => NdlAudioConfig::Pcm {
                channels: i32::from(channels.min(PLANE_MAX_CHANNELS)),
            },
            Self::Software | Self::NdlOpus => NdlAudioConfig::Opus {
                // Stereo either way: the silent frame's TOC declares stereo, and a software-decode
                // session's plane never sees the real stream.
                channels: 2,
                // kHz, not Hz — NDL's own unit, and what ss4s passes
                // (`info->sampleRate / 1000.0`). punktfunk's audio plane is fixed at 48 kHz.
                sample_rate_khz: 48.0,
            },
        }
    }
}

impl Connected {
    /// Stop and join threads, then drop `NativeClient`. Call `disconnect_quit()` first for
    /// graceful shutdown. Returns `false` if any step didn't finish within
    /// `SHUTDOWN_JOIN_TIMEOUT` — the caller must then skip `ndl::quit()`, since the thread
    /// still running may still be inside an NDL FFI call that a concurrent unload would race. A
    /// wedged video/audio/clock join additionally refuses new loads until it finishes — those three
    /// are the threads that touch NDL.
    pub fn shutdown(self) -> bool {
        use crate::platform::webos::ndl::poison;
        self.stop.store(true, Ordering::Relaxed);
        let mut clean = join_with_timeout(self.video_thread, SHUTDOWN_JOIN_TIMEOUT, "video", poison);
        if let Some(audio) = self.audio_thread {
            clean &= join_with_timeout(audio, SHUTDOWN_JOIN_TIMEOUT, "audio", poison);
        }
        if let Some(clock) = self.clock_thread {
            clean &= join_with_timeout(clock, SHUTDOWN_JOIN_TIMEOUT, "clock", poison);
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
    pub audio_route: crate::services::store::AudioRoutePref,
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
    /// Whether to ask the host for slice-progressive AU prefixes — on wherever the backend can
    /// take them.
    frame_parts: bool,
}

impl Negotiated {
    /// **The authoritative capability gate.** Codec, colour path and channel count are settled by
    /// the handshake, BEFORE any decoder opens, so a document carried over from a more capable TV
    /// must be clamped here and not merely hidden in the UI: HEVC negotiated onto an H.264-only
    /// decoder is a frozen black stream with no second chance once `Welcome` has resolved.
    fn clamp(params: &ConnectParams) -> Self {
        let caps = video_caps();
        // The route is settled before the handshake because it decides the widest layout worth
        // ASKING the host to encode — channels this session's sink cannot output are airlink and
        // host CPU spent on silence. Nothing is folded down later; see `AudioRoute::max_channels`.
        let route_max = params.audio_route.max_channels(caps);
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
            audio_channels: params.audio_channels.min(caps.max_channels).min(route_max),
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
            frame_parts: device::ndl_generation() == NdlGeneration::V2
                && crate::core::caps::effective_backend(params.video_backend) != VideoBackend::Smp,
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
        // Slice-progressive delivery: AU prefixes reach the decoder while the rest is still on the
        // wire, so a frame no longer waits for its own last datagram (`session::pump`'s `AuParts`).
        // On wherever it can be — NDL v2 only, per `Negotiated::clamp`: v1's feed has no timestamp
        // to repeat across pieces and SMP's load shape is fragile enough without them.
        negotiated.frame_parts,
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
fn load_player(params: &ConnectParams, client: &NativeClient, route: AudioRoute) -> Result<(VideoPlayer, bool)> {
    let resolved_mode = client.mode();
    let fps = resolved_mode.refresh_hz.max(1);
    let codec =
        NdlCodec::from_wire(client.codec).with_context(|| format!("unsupported codec 0x{:02x}", client.codec))?;
    let app_id = crate::platform::webos::ndl::app_id();
    let (width, height) = (resolved_mode.width as i32, resolved_mode.height as i32);
    // Every V2 load asks for a plane: a fed one is what makes NDL pace the picture at all
    // (docs/NOTES.md § "NDL's audio plane"). What rides it — the real stream, this client's own
    // PCM, or `run_clock_plane`'s metronome — is the route's business; the load only needs the
    // format. A set that refuses the load falls back to video-only in `NdlVideo::load`, and gives
    // up pacing with it.
    let ndl_audio = route.plane_config(client.audio_channels);
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
fn audio_path_label(player: &VideoPlayer, route: AudioRoute, has_plane: bool) -> &'static str {
    match (route, has_plane, player) {
        (AudioRoute::NdlOpus, ..) => "NDL hardware Opus decode (+ clock plane standing by)",
        (AudioRoute::NdlPcm, ..) => "software Opus decode -> NDL PCM plane (+ clock plane standing by)",
        // A plane the real stream is not using is the pacing metronome — see
        // `NdlVideo::run_clock_plane`.
        (_, true, _) => "software Opus decode -> SDL2 + NDL clock plane",
        (_, false, VideoPlayer::V1(_)) => "software Opus decode -> SDL2, no clock plane (NDL v1 has no audio type)",
        (_, false, VideoPlayer::Smp(_)) => "software Opus decode -> SDL2, no clock plane (SMP loads video-only)",
        // No plane on a V2 load means the audio-enabled attempt did not confirm and `load()` fell
        // back to video-only, so this session has no pacing reference either.
        (_, false, VideoPlayer::V2(_)) => "software Opus decode -> SDL2, no clock plane (NDL rejected the audio load)",
    }
}

/// Connects to a punktfunk host and starts the video pump thread.
///
/// Blocks until the handshake completes or `params.timeout` elapses. NDL manages its own
/// punch-through area natively (see [`crate::platform::webos::ndl`]'s module docs), so no
/// display geometry is needed here.
pub fn connect(params: &ConnectParams) -> Result<Connected> {
    // Fails before touching the network: a full handshake would only end in `NdlVideo::load()`
    // rejecting the same gate, pointlessly holding the host's pending-session slot for `timeout`.
    crate::platform::webos::ndl::ensure_not_poisoned()?;
    let negotiated = Negotiated::clamp(params);
    let client = Arc::new(dial(params, &negotiated)?);
    log_handshake(&client, &negotiated);

    // Picked BEFORE the load, because it decides the plane's FORMAT, then re-checked against the
    // plane the load actually produced — a rejected audio-enabled load leaves no plane to ride.
    let wanted = AudioRoute::pick(params, client.audio_channels);
    let (player, is_hdr) = load_player(params, &client, wanted)?;
    let ndl_audio = player.ndl_audio_handle();
    let route = wanted.on_plane(ndl_audio.is_some());
    tracing::info!(
        "audio path: {} (host resolved {} channel(s))",
        audio_path_label(&player, route, ndl_audio.is_some()),
        client.audio_channels,
    );

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(StreamStats::default());
    let video_thread = spawn_video_thread(&client, player, &stop, &stats, is_hdr)?;
    // Failing here after the video thread is already up would otherwise detach it: `Connected` is
    // never built, so nothing ever sets `stop`, and a thread still feeding NDL outlives the error
    // the caller sees — which then races the `ndl::quit()` the failed connect leads to.
    let (audio_thread, clock_thread) = match spawn_plane_threads(&client, ndl_audio, &stop, route) {
        Ok(handles) => handles,
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
        clock_thread,
        audio_route: route,
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

/// `(audio pump, clock plane)`.
type PlaneThreads = (Option<std::thread::JoinHandle<()>>, Option<std::thread::JoinHandle<()>>);

/// The threads on NDL's audio plane, if this load got one: the clock plane always, plus the real
/// Opus stream's pump when it is offloaded.
///
/// The clock plane runs on EVERY session with a plane — NDL paces the picture against a fed plane
/// regardless of which pump the audio path uses. Offloaded, it yields to the real stream and only
/// fills in once the host stops sending.
fn spawn_plane_threads(
    client: &Arc<NativeClient>,
    ndl_audio: Option<Arc<NdlVideo>>,
    stop: &Arc<AtomicBool>,
    route: AudioRoute,
) -> Result<PlaneThreads> {
    let Some(ndl) = ndl_audio else {
        return Ok((None, None));
    };
    let clock_thread = {
        let (ndl, stop) = (ndl.clone(), stop.clone());
        std::thread::Builder::new()
            .name("punktfunk-webos-clock".into())
            .spawn(move || ndl.run_clock_plane(&stop, route.on_ndl_plane()))
            .context("spawn clock plane thread")?
    };
    let audio_thread = match route {
        AudioRoute::Software => return Ok((None, Some(clock_thread))),
        AudioRoute::NdlOpus => {
            let (client, stop) = (client.clone(), stop.clone());
            std::thread::Builder::new()
                .name("punktfunk-webos-audio".into())
                .spawn(move || ndl_audio_pump(&client, &ndl, &stop))
        }
        AudioRoute::NdlPcm => {
            let (client, stop) = (client.clone(), stop.clone());
            // Built here rather than inside the thread, so a decoder this layout can't open is an
            // error the caller reports instead of a thread that dies silently and leaves the plane
            // on its metronome. Folded into the spawn's own error type to keep ONE failure path:
            // an early `?` here would return before the clock thread below is joined, detaching a
            // thread that is still feeding NDL.
            match crate::platform::webos::audio::PcmFeed::new(client.audio_channels) {
                Ok(mut feed) => {
                    // Logged because it is the one place the fold is visible: a 7.1 session says
                    // 6 here, and "did my 5.1 actually reach the plane" is otherwise unanswerable
                    // from a report.
                    tracing::info!(
                        "NDL PCM plane: {} channel(s) from a {}-channel stream",
                        feed.plane_channels(),
                        client.audio_channels,
                    );
                    std::thread::Builder::new()
                        .name("punktfunk-webos-audio".into())
                        .spawn(move || ndl_pcm_audio_pump(&client, &ndl, &stop, &mut feed))
                }
                Err(e) => Err(std::io::Error::other(format!("PCM plane decoder: {e:#}"))),
            }
        }
    };
    match audio_thread {
        Ok(handle) => Ok((Some(handle), Some(clock_thread))),
        // Same reason `connect` unwinds its video thread on failure: a detached thread still
        // feeding NDL would outlive the error and race the `ndl::quit()` that follows it.
        Err(e) => {
            stop.store(true, Ordering::Relaxed);
            join_with_timeout(
                clock_thread,
                SHUTDOWN_JOIN_TIMEOUT,
                "clock",
                crate::platform::webos::ndl::poison,
            );
            Err(e).context("spawn audio pump thread")
        }
    }
}
