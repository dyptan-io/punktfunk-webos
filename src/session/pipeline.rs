//! Assembling one session's media pipeline: which sinks this TV gets, which stages sit above
//! them, and the threads that drive the whole thing.
//!
//! Everything backend-specific about a session is decided here, once. `connect` runs the handshake
//! and hands the result over; the pumps and stages above are written against `core::media`'s
//! traits and never learn which decoder or which audio route they got.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use punktfunk_core::client::NativeClient;
use punktfunk_core::quic;

use crate::core::media::{AudioPlane, AudioSink, VideoSink};
use crate::platform::webos::device::{self, NdlGeneration};
use crate::platform::webos::ndl::v1::NdlV1Video;
use crate::platform::webos::ndl::{NdlAudioConfig, NdlCodec, NdlVideo};
use crate::services::store::{AudioRoutePref, VideoBackend};
use crate::session::audio::AudioStage;
use crate::session::connect::ConnectParams;
use crate::session::join::{join_with_timeout, SHUTDOWN_JOIN_TIMEOUT};
use crate::session::paced::PacedPlane;
use crate::session::pump::{spawn_audio_feed, video_pump};
use crate::session::stage::{SinkConfig, VideoStage};
use crate::session::StreamStats;

/// The threads driving one session's pipeline. Dropping this does NOT stop them — the session's
/// `stop` flag does, and [`Self::join`] waits them out.
pub struct MediaPipeline {
    video_thread: std::thread::JoinHandle<()>,
    /// The audio pump, on the routes that own their sink. `None` on the software route, where the
    /// SDL device belongs to whichever thread initialised SDL and the loop spawns it instead.
    audio_thread: Option<std::thread::JoinHandle<()>>,
    /// The audio plane's keep-alive, on every V2 load that got a plane. `None` only when the load
    /// has no plane at all (V1, SMP, or a rejected audio load).
    clock_thread: Option<std::thread::JoinHandle<()>>,
}

impl MediaPipeline {
    /// Load the decoder, pick the audio route, and start every thread the session needs.
    ///
    /// Unwinds itself on failure: a thread already started is stopped and joined before the error
    /// returns, because a detached thread still feeding NDL would outlive the error the caller
    /// sees and race the `ndl::quit()` that follows it.
    ///
    /// Returns the pipeline, the route it settled on, and whether HDR metadata is being applied.
    pub fn build(
        params: &ConnectParams,
        client: &Arc<NativeClient>,
        stop: &Arc<AtomicBool>,
        stats: &Arc<StreamStats>,
    ) -> Result<(Self, AudioRoutePref, bool)> {
        // Picked BEFORE the load, because it decides the plane's FORMAT, then re-checked against
        // the plane the load actually produced — a rejected audio-enabled load leaves no plane to
        // ride.
        let wanted = resolve_route(params.audio_route, client.audio_channels, None);
        let (player, is_hdr) = load_player(params, client, wanted)?;
        let plane = player.audio_plane();
        let route = resolve_route(wanted, client.audio_channels, Some(plane.is_some()));
        tracing::info!(
            "audio path: {} on {} (host resolved {} channel(s))",
            audio_path_label(route, plane.is_some()),
            player.name(),
            client.audio_channels,
        );
        let video_thread = spawn_video_thread(client, player, stop, stats, is_hdr)?;
        // Failing here after the video thread is already up would otherwise detach it.
        let (audio_thread, clock_thread) = match spawn_plane_threads(client, plane, stop, route) {
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
        Ok((
            Self {
                video_thread,
                audio_thread,
                clock_thread,
            },
            route,
            is_hdr,
        ))
    }

    /// Wait the threads out, bounded. `false` means one is still running — it may still be inside
    /// an NDL call, so the caller must skip `ndl::quit()`, and these three are the threads that
    /// touch NDL, so a wedge also refuses new loads until it finishes.
    pub fn join(self) -> bool {
        use crate::platform::webos::ndl::poison;
        let mut clean = join_with_timeout(self.video_thread, SHUTDOWN_JOIN_TIMEOUT, "video", poison);
        if let Some(audio) = self.audio_thread {
            clean &= join_with_timeout(audio, SHUTDOWN_JOIN_TIMEOUT, "audio", poison);
        }
        if let Some(clock) = self.clock_thread {
            clean &= join_with_timeout(clock, SHUTDOWN_JOIN_TIMEOUT, "clock", poison);
        }
        clean
    }
}

/// Picks the route a session WANTS, then downgrades it to what the load actually produced.
///
/// **Software is the default** ([`AudioRoutePref`]'s own default). It is the only shape whose
/// pacing is known good: NDL paces the picture against a FED audio plane, and a plane fed from the
/// network inherits the stream's arrival jitter — which is the stutter the silent clock plane was
/// introduced to cure. The two plane routes are shorter and are kept selectable for exactly that
/// comparison; until one of them is measured better on real hardware, the metronome keeps the plane
/// and the audio takes the longer path.
///
/// `has_plane` is `None` before the load: the route decides the plane's FORMAT, so it is picked
/// first and re-checked afterwards. A rejected audio-enabled load (`NdlVideo::load` falls back to
/// video-only), V1 or SMP leaves no plane to ride, and software is what is left.
fn resolve_route(pref: AudioRoutePref, channels: u8, has_plane: Option<bool>) -> AudioRoutePref {
    match pref {
        _ if has_plane == Some(false) => AudioRoutePref::Software,
        AudioRoutePref::NdlPcm => AudioRoutePref::NdlPcm,
        // Stereo or nothing: `Settings::clamp` already holds the document to it, and a session the
        // host resolved wider must not silently land on a plane that would read the interleave at
        // the wrong stride — it falls back to software instead.
        AudioRoutePref::NdlOpus if channels == 2 => AudioRoutePref::NdlOpus,
        AudioRoutePref::Software | AudioRoutePref::NdlOpus => AudioRoutePref::Software,
    }
}

/// The plane format this route needs at load time. Every V2 load asks for a plane whatever the
/// route — NDL only paces the picture against a fed one — so the software route still loads Opus,
/// for `run_clock_plane`'s metronome to ride.
fn plane_config(route: AudioRoutePref, channels: u8) -> NdlAudioConfig {
    match route {
        // Exactly what the session negotiated: the route's own ceiling already kept the handshake
        // from asking for a width this plane has no mode for (`AudioRoutePref::max_channels`), so
        // there is nothing to fold and nothing to clamp.
        AudioRoutePref::NdlPcm => NdlAudioConfig::Pcm {
            channels: i32::from(channels),
        },
        AudioRoutePref::Software | AudioRoutePref::NdlOpus => NdlAudioConfig::Opus {
            // Stereo either way: the silent frame's TOC declares stereo, and a software-decode
            // session's plane never sees the real stream.
            channels: 2,
            // kHz, not Hz — NDL's own unit, and what ss4s passes (`info->sampleRate / 1000.0`).
            // punktfunk's audio plane is fixed at 48 kHz.
            sample_rate_khz: 48.0,
        },
    }
}

/// Default HDR10 mastering metadata for the LG CX OLED panel.
/// Sent in `Hello::display_hdr`; refined per-content by `next_hdr_meta`.
pub(super) fn cx_display_hdr() -> quic::HdrMeta {
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

/// Opens the decoder for the negotiated stream and hands it the colorimetry.
///
/// Returns the player and whether HDR mastering metadata is being applied — the answer the
/// video pump needs to know whether to forward per-content metadata at all.
fn load_player(
    params: &ConnectParams,
    client: &NativeClient,
    route: AudioRoutePref,
) -> Result<(Box<dyn VideoSink>, bool)> {
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
    let ndl_audio = plane_config(route, client.audio_channels);
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
    let player: Box<dyn VideoSink> = match (smp, device::ndl_generation()) {
        (Some(sf), _) => Box::new(sf),
        (None, NdlGeneration::V2) => Box::new(Arc::new(
            NdlVideo::load(&app_id, width, height, codec, Some(ndl_audio)).context("NDL load")?,
        )),
        (None, NdlGeneration::V1) => Box::new(NdlV1Video::load(&app_id, width, height, codec).context("NDL v1 load")?),
    };
    tracing::info!(
        "{} loaded ({codec:?} {}x{}@{fps}fps)",
        player.name(),
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
    if let Err(e) = player.set_color(is_hdr.then(cx_display_hdr).as_ref(), client.color) {
        tracing::warn!("NDL colour metadata failed: {e:#}");
    }
    Ok((player, is_hdr))
}

/// Why this session's audio ended up on the path it did.
///
/// Naming the REASON matters: "software Opus" is the correct outcome on the software route, on a
/// route that asked for a plane and didn't get one, and on a backend that has no plane at all —
/// and a silent session looks identical on all of them. Without this the first debugging question
/// has no answer in the log.
fn audio_path_label(route: AudioRoutePref, has_plane: bool) -> &'static str {
    match (route, has_plane) {
        (AudioRoutePref::NdlOpus, _) => "NDL hardware Opus decode (+ clock plane standing by)",
        (AudioRoutePref::NdlPcm, _) => "software Opus decode -> paced ring -> NDL PCM plane",
        // A plane the real stream is not using is the pacing metronome — see
        // `NdlVideo::run_clock_plane`.
        (AudioRoutePref::Software, true) => "software Opus decode -> SDL2 + NDL clock plane",
        // No plane means no pacing reference either: this backend has none (NDL v1, SMP), or the
        // audio-enabled load did not confirm and `load()` fell back to video-only.
        (AudioRoutePref::Software, false) => "software Opus decode -> SDL2, no clock plane",
    }
}

fn spawn_video_thread(
    client: &Arc<NativeClient>,
    player: Box<dyn VideoSink>,
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
            let stage = VideoStage::new(player, stats.clone(), cfg);
            video_pump(client, stage, stop, stats, is_hdr);
        })
        .context("spawn video thread")
}

/// `(audio pump, clock plane)`.
type PlaneThreads = (Option<std::thread::JoinHandle<()>>, Option<std::thread::JoinHandle<()>>);

/// The threads on NDL's audio plane, if this load got one: the loop that holds the plane at depth,
/// plus the real stream's pump on the routes that ride it.
///
/// **Something feeds the plane on EVERY session that has one** — NDL paces the picture against a
/// fed plane regardless of where the audio goes. Which loop does it is the route's one structural
/// difference:
///
/// - `Software`: the metronome (`AudioPlane::run_keepalive`), silence only, and the audio goes to
///   the SDL device the runtime owns.
/// - `NdlOpus`: the metronome yielding to the real stream, which the TV's own decoder stamps.
/// - `NdlPcm`: `session::paced`'s feeder, which IS the keep-alive — it tops the plane up from a
///   ring on the same cadence and pads silence when that runs dry. Feeding decoded samples as they
///   arrived instead made the plane's depth a function of network jitter, and the picture with it.
fn spawn_plane_threads(
    client: &Arc<NativeClient>,
    ndl_audio: Option<Arc<dyn AudioPlane>>,
    stop: &Arc<AtomicBool>,
    route: AudioRoutePref,
) -> Result<PlaneThreads> {
    let Some(ndl) = ndl_audio else {
        return Ok((None, None));
    };
    // Built before the thread starts, so a ring this route can't size is an error the caller
    // reports rather than a thread that dies silently and leaves the plane on its metronome.
    let paced = (route == AudioRoutePref::NdlPcm).then(|| PacedPlane::new(ndl.clone(), client.audio_channels));
    // What the stage feeds: the ring on the paced route, the plane itself where the hardware
    // stamps, and nothing at all on the software route (the SDL device belongs to the runtime).
    let sink: Option<Arc<dyn AudioSink>> = match (&paced, route) {
        (Some((ring, _)), _) => Some(ring.clone()),
        (None, AudioRoutePref::Software) => None,
        (None, _) => Some(ndl.clone()),
    };
    let plane_loop: Box<dyn FnOnce(&AtomicBool) + Send> = match paced {
        Some((_, mut feeder)) => Box::new(move |stop| feeder.run(stop)),
        None => Box::new(move |stop| ndl.run_keepalive(stop, route.on_ndl_plane())),
    };
    let clock_thread = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("punktfunk-webos-clock".into())
            .spawn(move || plane_loop(&stop))
            .context("spawn clock plane thread")?
    };
    let Some(sink) = sink else {
        return Ok((None, Some(clock_thread)));
    };
    // Folded into the spawn's own error type to keep ONE failure path: an early `?` here would
    // return before the clock thread above is joined, detaching a thread still feeding NDL.
    let audio_thread = match AudioStage::new(sink, client.audio_channels) {
        Ok(stage) => {
            tracing::info!(
                "audio stage: {} channel(s) into {}",
                client.audio_channels,
                stage.sink_name()
            );
            spawn_audio_feed(client.clone(), stage, stop.clone())
        }
        Err(e) => Err(anyhow::anyhow!("audio stage: {e:#}")),
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
