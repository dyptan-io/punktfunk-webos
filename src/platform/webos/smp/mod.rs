//! webOS SMP (Starfish Media Pipeline) video, through the `libplayerAPIs_C.so` wrapper
//! ([`ffi`]) — the vendor's own C++ class is still named `StarfishMediaAPIs`, which is why that
//! spelling survives in the symbol names and nowhere else.
//!
//! Offered as a **selectable** backend on webOS 3.5-4.x only (`core::caps::smp_selectable`):
//! NDL there is the v1 surface — H.264, SDR, no PTS (see [`super::ndl::v1`]) —
//! while SMP is the same silicon through a richer front-end, so HEVC, HDR metadata and
//! `pauseAtDecodeTime` pacing are only reachable this way. webOS 5+ keeps NDL v2.
//!
//! The load payload, the feed PTS domain and the [`sink`] split are all load-bearing — deviating
//! from them is how you get a pipeline that loads, accepts every frame, and shows nothing. A failed
//! load is a plain `Err`; `session::connect` falls back to NDL.
//!
//! Audio is never SMP's (`needAudio: false`) — this client decodes Opus itself
//! (`platform::webos::audio`).
mod ffi;
mod sink;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::core::media::{MediaClock, VideoSink, VideoSinkCaps};
use punktfunk_core::quic;

use self::sink::Sink;
use super::ndl::NdlCodec;
use crate::platform::webos::device;

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
    /// payload the ACB sink gets.
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

/// SMP's load callback. Only three events matter here; the vendor library already logs the rest.
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
    /// SMP's PTS domain: nanoseconds since this load (`now - openTime`), not the host's clock.
    /// [`Self::elapsed_ns`] is what maps a host PTS onto it.
    opened: Instant,
    /// Set once a frame has been accepted — the ACB sink is told "playing" exactly once, on that
    /// frame, because it won't show the plane before.
    playing: AtomicBool,
}

impl SmpVideo {
    /// Open SMP for a `width`x`height`@`fps` stream and wait for the load to complete.
    ///
    /// The request shape is the only one this client has ever seen complete a load. An earlier cut
    /// also tried a trimmed variant derived from the platform's own NDL binaries; it never loaded
    /// anywhere and cost a timeout per session to find out. Don't add a second shape without a
    /// device that loads with it.
    pub fn load(app_id: &str, width: i32, height: i32, fps: u32, codec: NdlCodec) -> Result<Self> {
        // Refuse to load on these boards, same as NDL v1 — a broken jailer config leaves the
        // decoder unreachable.
        if device::jail_config_broken() {
            bail!(
                "this TV's jailer config leaves /dev/rtkmem unreadable (machine {}), so SMP cannot \
                 reach the decoder",
                device::machine_name().unwrap_or_else(|| "unknown".into()),
            );
        }
        let fns = ffi::fns()?;
        // SAFETY: a NULL uid is accepted; the handle is checked before use.
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

        // SAFETY: live handle; the media id must be bound to the sink before the load.
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
        // Per-frame path: stack scratch so a feed costs no allocation; only overflow hits the heap.
        let mut scratch = [0u8; 128];
        let owned;
        let payload: &CStr = match write_feed_payload(&mut scratch, au, pts_ns) {
            Some(p) => p,
            None => {
                owned = CString::new(feed_payload_string(au, pts_ns))?;
                &owned
            }
        };
        let mut buf = [0u8; 128];
        // SAFETY: every argument is valid for the call; the wrapper copies SMP's `std::string`
        // result into `buf` (NUL-terminated, truncated) before it destructs.
        unsafe {
            (self.fns.feed)(self.api(), payload.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len());
        }
        // Substring matching: the result is usually `{"returnValue":"Ok"}` but the
        // vendor is not consistent about the wrapper, and a parse failure must not read as a
        // decode failure.
        let result = CStr::from_bytes_until_nul(&buf)
            .map(CStr::to_string_lossy)
            .unwrap_or_default();
        let result = result.trim();
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

/// The one feed-payload shape, written through either `fmt::Write` or `io::Write` — the two paths
/// below must never drift apart.
macro_rules! feed_payload {
    ($dst:expr, $au:expr, $pts_ns:expr) => {
        write!(
            $dst,
            r#"{{"bufferAddr":"{:p}","bufferSize":{},"pts":{},"esData":1}}"#,
            $au.as_ptr(),
            $au.len(),
            $pts_ns,
        )
    };
}

fn feed_payload_string(au: &[u8], pts_ns: u64) -> String {
    use std::fmt::Write;

    let mut s = String::new();
    let _ = feed_payload!(&mut s, au, pts_ns);
    s
}

/// The feed payload written into caller-owned scratch, NUL included. `None` if it doesn't fit, so
/// the caller allocates rather than feeding a truncated request.
fn write_feed_payload<'a>(scratch: &'a mut [u8; 128], au: &[u8], pts_ns: u64) -> Option<&'a CStr> {
    use std::io::Write;

    // Last byte is reserved for the NUL a `CStr` needs.
    let mut cursor: &mut [u8] = &mut scratch[..127];
    feed_payload!(cursor, au, pts_ns).ok()?;
    let len = 127 - cursor.len();
    scratch[len] = 0;
    CStr::from_bytes_with_nul(&scratch[..=len]).ok()
}

impl Drop for SmpVideo {
    fn drop(&mut self) {
        // Re-arm so the reveal gate stops reporting the session being torn down here.
        super::ndl::arm_frame_gate();
        // SAFETY: `api` is valid for the lifetime of `Self`; best-effort teardown, so results are
        // ignored (`Drop` can't propagate). `pushEOS` only applies once frames have flowed. The
        // sink's own `Drop` reports UNLOADED and releases the plane after.
        unsafe {
            if self.playing.load(Ordering::Relaxed) {
                (self.fns.push_eos)(self.api());
            }
            (self.fns.unload)(self.api());
            (self.fns.destroy)(self.api());
        }
    }
}

impl MediaClock for SmpVideo {
    fn now_ns(&self) -> u64 {
        self.elapsed_ns()
    }
}

/// SMP takes a timestamp and drains its own buffer: no flush, no depth query, and AU pieces are
/// not offered — its load shape is fragile enough without them.
impl VideoSink for SmpVideo {
    fn name(&self) -> &'static str {
        "SMP"
    }

    fn caps(&self) -> VideoSinkCaps {
        VideoSinkCaps {
            pts: true,
            ..VideoSinkCaps::FEED_ONLY
        }
    }

    fn feed(&self, au: &[u8], pts_ns: u64) -> Result<()> {
        self.play(au, pts_ns)
    }

    fn set_color(&self, meta: Option<&quic::HdrMeta>, color: quic::ColorInfo) -> Result<()> {
        self.set_color_info(meta, color)
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        Some(self)
    }
}
