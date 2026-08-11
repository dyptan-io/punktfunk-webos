//! webOS SMP (Starfish Media Pipeline) video, through the `libplayerAPIs_C.so` wrapper
//! ([`ffi`]) — the vendor's own C++ class is still named `StarfishMediaAPIs`, which is why that
//! spelling survives in the symbol names and nowhere else.
//!
//! Offered as a **selectable** backend on webOS 3.5-4.x only (`core::caps::smp_selectable`):
//! NDL there is the v1 surface — H.264, SDR, no PTS, fixed 1080p plane (see [`super::ndl::v1`]) —
//! while SMP is the same silicon through a richer front-end, so HEVC, HDR metadata and
//! `pauseAtDecodeTime` pacing are only reachable this way. webOS 5+ keeps NDL v2.
//!
//! Ported from ss4s `modules/webos/smp`, which is also where the load payload, the feed PTS
//! domain and the [`sink`] split come from — deviating from it is how you get a pipeline that
//! loads, accepts every frame, and shows nothing. A failed load is a plain `Err`;
//! `session::connect` falls back to NDL.
//!
//! Audio is never SMP's (`needAudio: false`) — this client decodes Opus itself
//! (`platform::webos::audio`), so `smp_audio.c` has no counterpart here.
mod ffi;
mod sink;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use punktfunk_core::quic;

use self::sink::Sink;
use super::ndl::NdlCodec;
use crate::platform::webos::device;

/// Whether SMP could load on this TV at all: the wrapper `.so` resolves (it is packaged, and the
/// vendor library behind it exists) and the jailer config isn't the known-broken one. Probed by
/// `runtime` at startup and published through `core::caps` — see [`SmpVideo::load`] for why the
/// answer has to be known *before* the handshake rather than at load time.
pub fn available() -> bool {
    ffi::fns().is_ok() && !device::jail_config_broken()
}

/// How long to wait for `LOADCOMPLETED`. The host is already streaming by then, so waiting longer
/// only piles up frames — a load that completes does so in well under a second.
const LOAD_TIMEOUT: Duration = Duration::from_secs(2);
const LOAD_POLL: Duration = Duration::from_millis(10);

/// Shared with the SMP load callback, which runs on a vendor thread.
struct Shared {
    play: unsafe extern "C" fn(*mut c_void) -> bool,
    api: *mut c_void,
    loaded: AtomicBool,
    /// Whether this session applies HDR — decides if `hdrType` is injected into the video-info
    /// payload the ACB sink gets (ss4s `SetMediaVideoData`).
    hdr: AtomicBool,
    /// The sink, reached from both the callback (`LOADCOMPLETED`, video info) and the feed path
    /// (first-frame `PLAYING`), hence the mutex.
    sink: Mutex<Sink>,
}

// SAFETY: `api` is only ever handed back to SMP, which owns the pointee's synchronization.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    fn media_id(&self, fns: &ffi::Fns) -> CString {
        // SAFETY: the wrapper returns a NUL-terminated pointer owned by the live C++ object.
        unsafe { CStr::from_ptr((fns.media_id)(self.api)) }.to_owned()
    }
}

/// SMP's load callback. Only three events matter here; ss4s logs the rest, which the vendor
/// library already does through its own logging.
unsafe extern "C" fn on_event(event: c_int, _num: i64, str_value: *const c_char, data: *mut c_void) {
    if data.is_null() {
        return;
    }
    let shared = &*(data as *const Shared);
    match event {
        ffi::EVENT_LOADCOMPLETED => {
            let Ok(fns) = ffi::fns() else { return };
            let media_id = shared.media_id(fns);
            shared.lock_sink().load_completed(&media_id);
            (shared.play)(shared.api);
            shared.loaded.store(true, Ordering::Release);
        }
        // The pipeline's own description of the video it's decoding — the ACB needs it to
        // configure the plane (and to be told the stream is HDR).
        ffi::EVENT_STR_VIDEO_INFO if !str_value.is_null() => {
            let info = CStr::from_ptr(str_value).to_string_lossy().into_owned();
            shared
                .lock_sink()
                .set_media_video_data(&info, shared.hdr.load(Ordering::Relaxed));
        }
        _ => {}
    }
}

impl Shared {
    fn lock_sink(&self) -> std::sync::MutexGuard<'_, Sink> {
        self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One active SMP decode+present session. Dropping it unloads the pipeline and releases the sink.
pub struct SmpVideo {
    fns: &'static ffi::Fns,
    shared: Box<Shared>,
    /// SMP's PTS domain: nanoseconds since this load, not the host's clock (ss4s feeds
    /// `now - openTime`). [`Self::elapsed_ns`] is what maps a host PTS onto it.
    opened: Instant,
    /// Set once a frame has been accepted — the ACB sink is told "playing" exactly once, on that
    /// frame, because it won't show the plane before (ss4s `StarfishPlayerFeed`).
    playing: AtomicBool,
}

impl SmpVideo {
    /// Open SMP for a `width`x`height`@`fps` stream and wait for the load to complete.
    ///
    /// The request is ss4s's `MakeLoadPayload` verbatim — the only shape this client has ever seen
    /// complete a load. An earlier cut also tried a trimmed variant derived from the platform's
    /// own NDL binaries; it never loaded anywhere and cost a timeout per session to find out.
    /// Don't add a second shape without a device that loads with it.
    pub fn load(app_id: &str, width: i32, height: i32, fps: u32, codec: NdlCodec) -> Result<Self> {
        // ss4s's SMP module refuses to load at all on these boards (`SS4S_JAIL_CHECK`), same as
        // NDL v1 — a broken jailer config leaves the decoder unreachable.
        if device::jail_config_broken() {
            bail!(
                "this TV's jailer config leaves /dev/rtkmem unreadable (machine {}), so SMP cannot \
                 reach the decoder",
                device::machine_name().unwrap_or_else(|| "unknown".into()),
            );
        }
        let fns = ffi::fns()?;
        // SAFETY: NULL uid is what ss4s passes; the handle is checked before use.
        let api = unsafe { (fns.create)(std::ptr::null()) };
        if api.is_null() {
            bail!("StarfishMediaAPIs_create returned null");
        }
        let sink = match Sink::create(app_id) {
            Ok(sink) => sink,
            Err(e) => {
                // SAFETY: nothing owns `api` yet — no `Self` exists to drop it.
                unsafe { (fns.destroy)(api) };
                return Err(e);
            }
        };
        let sf = Self {
            fns,
            shared: Box::new(Shared {
                play: fns.play,
                api,
                loaded: AtomicBool::new(false),
                hdr: AtomicBool::new(false),
                sink: Mutex::new(sink),
            }),
            opened: Instant::now(),
            playing: AtomicBool::new(false),
        };
        // From here on every exit path tears down through `Drop`.

        // SAFETY: live handle; the media id is bound to the sink before the load, as ss4s does.
        unsafe { (fns.notify_fg)(api) };
        let media_id = sf.shared.media_id(fns);
        sf.shared.lock_sink().set_media_id(&media_id);

        // Shared with NDL: `runtime` uncovers the video plane on the first frame whatever decoded
        // it, so a previous session's frames must not satisfy this one's reveal.
        super::ndl::arm_frame_gate();
        let payload = CString::new(sf.load_payload(app_id, width, height, fps, codec))?;
        tracing::info!("SMP load payload: {payload:?}");
        let shared_ptr = std::ptr::from_ref(&*sf.shared) as *mut c_void;
        // SAFETY: `payload` outlives the call, and `shared_ptr` points into `sf`'s own `shared`,
        // which lives as long as SMP can deliver events.
        if !unsafe { (fns.load)(api, payload.as_ptr(), Some(on_event), shared_ptr) } {
            bail!("StarfishMediaAPIs_load returned false");
        }
        sf.shared.lock_sink().post_load(width, height);

        let deadline = Instant::now() + LOAD_TIMEOUT;
        while !sf.shared.loaded.load(Ordering::Acquire) {
            if Instant::now() > deadline {
                bail!("SMP load timed out — LOADCOMPLETED never arrived");
            }
            std::thread::sleep(LOAD_POLL);
        }
        Ok(sf)
    }

    fn load_payload(&self, app_id: &str, width: i32, height: i32, fps: u32, codec: NdlCodec) -> String {
        let mut option = serde_json::json!({
            "appId": app_id,
            "externalStreamingInfo": {
                "contents": {
                    "codec": {"video": match codec {
                        NdlCodec::H264 => "H264",
                        NdlCodec::H265 => "H265",
                    }},
                    "esInfo": {"pauseAtDecodeTime": true, "ptsToDecode": 0, "seperatedPTS": true},
                    "format": "RAW",
                    "provider": "Chrome"
                },
                "streamQualityInfo": true,
                "audioSync": true,
                "streamQualityInfoCorruptedFrame": true,
                "streamQualityInfoNonFlushable": true,
                "restartStreaming": false,
                // Pipeline appsrc levels; a low maximum only makes it discard buffers.
                "bufferingCtrInfo": {
                    "bufferMaxLevel": 0,
                    "bufferMinLevel": 0,
                    "preBufferByte": 0,
                    "qBufferLevelAudio": 0,
                    "qBufferLevelVideo": 0,
                    "srcBufferLevelAudio": {"minimum": 1, "maximum": 32768},
                    "srcBufferLevelVideo": {"minimum": 1, "maximum": 1048576}
                }
            },
            // WEBRTC cuts video latency significantly; LIVE would favour audio sync instead.
            "transmission": {"contentsType": "WEBRTC"},
            "needAudio": false,
            // With queryPosition true, SMP stops sending FRAMEREADY.
            "queryPosition": false,
            "lowDelayMode": true,
            "adaptiveStreaming": {
                "audioOnly": false,
                "maxWidth": width,
                "maxHeight": height,
                "maxFrameRate": f64::from(fps)
            }
        });
        // Only the webOS 5+ sink has a window id; the ACB path carries none (see `sink`).
        let window_id = self.shared.lock_sink().window_id().to_owned();
        if !window_id.is_empty() {
            option["windowId"] = window_id.into();
        }
        serde_json::json!({"args": [{"mediaTransportType": "BUFFERSTREAM", "option": option}]}).to_string()
    }

    /// Nanoseconds since the load — SMP's PTS domain (see [`Self::opened`]).
    pub fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.opened.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Feed one access unit. `pts_ns` must already be in SMP's domain ([`Self::elapsed_ns`]).
    pub fn play(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        let payload = CString::new(format!(
            r#"{{"bufferAddr":"{:p}","bufferSize":{},"pts":{pts_ns},"esData":1}}"#,
            au.as_ptr(),
            au.len(),
        ))?;
        let mut buf = [0u8; 128];
        // SAFETY: every argument is valid for the call; the wrapper copies SMP's `std::string`
        // result into `buf` (NUL-terminated, truncated) before it destructs.
        unsafe {
            (self.fns.feed)(self.api(), payload.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len());
        }
        // Substring matching, as ss4s does: the result is usually `{"returnValue":"Ok"}` but the
        // vendor is not consistent about the wrapper, and a parse failure must not read as a
        // decode failure.
        let result = String::from_utf8_lossy(&buf);
        let result = result.trim_matches(['\0', ' ']);
        if !result.contains("Ok") {
            bail!("StarfishMediaAPIs_feed: {result}");
        }
        // First accepted frame: the ACB sink won't composite until the app reports PLAYING, and
        // `runtime`'s reveal is waiting on the same event through NDL's gate.
        if !self.playing.swap(true, Ordering::Relaxed) {
            self.shared.lock_sink().start_playing();
            super::ndl::mark_frame_fed();
            tracing::info!("SMP first frame accepted {:?} after open", self.opened.elapsed());
        }
        Ok(())
    }

    /// Apply the negotiated colorimetry, with HDR10 mastering metadata when `meta` is present.
    pub fn set_color_info(&self, meta: Option<&quic::HdrMeta>, color: quic::ColorInfo) -> Result<()> {
        self.shared.hdr.store(meta.is_some(), Ordering::Relaxed);
        // The stream's own range flag, not a hardcoded value.
        let vui = serde_json::json!({
            "transferCharacteristics": color.transfer,
            "colorPrimaries": color.primaries,
            "matrixCoeffs": color.matrix,
            "videoFullRangeFlag": color.full_range != 0
        });
        let payload = match meta {
            // G/B/R order per ST.2086 convention (same as `ndl::v2`).
            Some(m) => {
                let [g, b, r] = m.display_primaries;
                serde_json::json!({
                    "hdrType": "HDR10",
                    "sei": {
                        "displayPrimariesX0": g[0], "displayPrimariesY0": g[1],
                        "displayPrimariesX1": b[0], "displayPrimariesY1": b[1],
                        "displayPrimariesX2": r[0], "displayPrimariesY2": r[1],
                        "whitePointX": m.white_point[0],
                        "whitePointY": m.white_point[1],
                        "minDisplayMasteringLuminance": m.min_display_mastering_luminance,
                        "maxDisplayMasteringLuminance": m.max_display_mastering_luminance,
                        "maxContentLightLevel": m.max_cll,
                        "maxPicAverageLightLevel": m.max_fall
                    },
                    "vui": vui
                })
            }
            None => serde_json::json!({"hdrType": "none", "vui": vui}),
        };
        let payload = CString::new(payload.to_string())?;
        // SAFETY: `payload` is valid for the duration of the call.
        if !unsafe { (self.fns.set_hdr_info)(self.api(), payload.as_ptr()) } {
            bail!("StarfishMediaAPIs_setHdrInfo failed");
        }
        Ok(())
    }

    fn api(&self) -> *mut c_void {
        self.shared.api
    }
}

impl Drop for SmpVideo {
    fn drop(&mut self) {
        // Re-arm so the reveal gate stops reporting the session being torn down here.
        super::ndl::arm_frame_gate();
        // SAFETY: `api` is valid for the lifetime of `Self`; best-effort teardown, so results are
        // ignored (`Drop` can't propagate). `pushEOS` only applies once frames have flowed, as in
        // ss4s's unload path. The sink's own `Drop` reports UNLOADED and releases the plane after.
        unsafe {
            if self.playing.load(Ordering::Relaxed) {
                (self.fns.push_eos)(self.api());
            }
            (self.fns.unload)(self.api());
            (self.fns.destroy)(self.api());
        }
    }
}
