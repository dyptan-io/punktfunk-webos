//! Runtime device capability detection.
//!
//! This client ships one binary to an open-ended set of TVs — a 2020 CX on webOS 5.6 and a
//! 2025 G5 on webOS 10.3 are both targets, and neither is the last one. Anything that
//! differs per model therefore has to be decided **at runtime**, from what the device
//! actually reports, rather than baked in.
//!
//! One thing deliberately **not** here: CPU codegen. `-C target-cpu` is a compile-time
//! flag, so a single `.ipk` cannot vary it per device — the baseline stays at the oldest
//! supported model and that is simply the cost of one binary. What *can* vary is
//! behaviour, and that is what this module feeds.
//!
//! **Detection is preferred by attempt, not by lookup table.** A table of model names is
//! wrong the day a TV ships that isn't in it. Where a capability can be probed by trying
//! it and handling failure (see `ndl::NdlVideo::load`'s audio fallback), that is always
//! the better mechanism; the facts here are for the decisions that can't be probed
//! cheaply, and for the log line that makes a bug report from an unknown model useful.
//!
//! **NDL generation.** The same runtime library ships v2 on webOS 5+ and a PTS-less,
//! H.264-only v1 on 3.5-4.x. [`ndl_generation`] only encodes the version ranges ss4s declares
//! for its `ndl-webos4`/`ndl-webos5` modules; whether the symbols are there is decided by the
//! `dlsym` probe in `ndl` (the second, decisive gate).
use std::ffi::CString;
use std::sync::OnceLock;

use crate::core::caps::VideoCaps;

/// TV capabilities detected at runtime (best-effort; missing sources fall back safely).
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// CPU cores (drives off-main-thread work before contention).
    pub cores: usize,
    /// Major webOS release (5, 6, … 10), when it can be determined.
    pub webos_major: Option<u32>,
    /// Marketing model string, e.g. `OLED65G58LW.DEUQLJP`. Diagnostics only — never
    /// branch on this, see the module docs.
    pub model: Option<String>,
    /// webOS TV SDK version (major, minor) — see [`sdk_version`]. Every ss4s module
    /// constraint (and this client's NDL generation choice) is written against this
    /// field, not `webos_major`.
    pub sdk_version: Option<(u32, u32)>,
    /// `otaId` from `getSystemInfo`, e.g. `HE_DTV_W19H_...`. Display-only.
    pub ota_id: Option<String>,
    /// SoC/board codename from `/etc/prefs/properties/machineName`, e.g. `m16p`, `k5lp`.
    pub machine_name: Option<String>,
}

/// webOS publishes these as plain JSON. Readable from a Dev-Mode shell; whether the
/// jailed app can read them varies, so every read is optional.
const OS_INFO: &str = "/var/run/nyx/os_info.json";
const DEVICE_INFO: &str = "/var/run/nyx/device_info.json";
const MACHINE_NAME_FILE: &str = "/etc/prefs/properties/machineName";
/// Present only when the `k5lp`/`k3lp` jailer config is sane — see [`jail_config_broken`].
const RTKMEM_DEVICE: &str = "/dev/rtkmem";

/// Extract JSON field without parser (avoids serde on filesystem source).
fn json_str_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let rest = text.split_once(&needle)?.1;
    let rest = rest.split_once(':')?.1;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let (value, _) = rest.split_once('"')?;
    Some(value.to_string())
}

/// Parses `"4.3.0"` / `"5.2.0"` into `(major, minor)`. Extra segments (patch) are ignored —
/// every ss4s `os_version` constraint compares major, and minor only where it appears here.
fn parse_sdk_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// One cached `getSystemInfo` payload for every field read from it: the Luna round-trip is on
/// the startup path and both readers want the same reply.
fn system_info() -> &'static str {
    static PAYLOAD: OnceLock<String> = OnceLock::new();
    PAYLOAD.get_or_init(|| {
        if !super::luna::available() {
            return String::new();
        }
        super::luna::call_capture(
            "luna://com.webos.service.tv.systemproperty/getSystemInfo",
            r#"{"keys":["sdkVersion","otaId"]}"#,
            super::luna::CALL_TIMEOUT,
        )
        .unwrap_or_default()
    })
}

/// The webOS TV SDK version (`sdkVersion`, not the Open webOS release) — the field every ss4s
/// module constraint and [`ndl_generation`] is written against. Resolved once, in precedence
/// order: launch-param override, Luna `getSystemInfo`, then `os_info.json`'s `webos_release`.
pub fn sdk_version() -> Option<(u32, u32)> {
    static SDK_VERSION: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *SDK_VERSION.get_or_init(|| {
        if let Some(over) = crate::logger::webos_sdk_override() {
            if let Some(v) = parse_sdk_version(over) {
                tracing::info!("webOS SDK version overridden at launch: {over}");
                return Some(v);
            }
            tracing::warn!("webos_sdk launch param {over:?} unparseable — ignoring");
        }
        if let Some(v) = json_str_field(system_info(), "sdkVersion").and_then(|s| parse_sdk_version(&s)) {
            return Some(v);
        }
        // Not the same field: `webos_release` is the OS release. Majors agree in practice,
        // which is all `ndl_generation` reads — but say so, since `sdk=` is then a guess.
        let os = std::fs::read_to_string(OS_INFO).unwrap_or_default();
        let major = json_str_field(&os, "webos_release")
            .and_then(|v| v.split('.').next().and_then(|m| m.parse().ok()))?;
        tracing::info!("no sdkVersion from Luna — assuming major {major} from webos_release");
        Some((major, 0))
    })
}

/// `otaId` from Luna's `getSystemInfo`, e.g. `HE_DTV_W19H_...`. Display-only.
fn ota_id() -> Option<String> {
    json_str_field(system_info(), "otaId")
}

/// SoC/board codename (`/etc/prefs/properties/machineName`, e.g. `m16p`, `k5lp`), as ss4s uses
/// for its per-board workarounds (see [`jail_config_broken`]). Never a lookup key for anything
/// beyond that narrow set of on-device-verified quirks — see the module docs.
pub fn machine_name() -> Option<String> {
    static MACHINE_NAME: OnceLock<Option<String>> = OnceLock::new();
    MACHINE_NAME
        .get_or_init(|| {
            std::fs::read_to_string(MACHINE_NAME_FILE)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .clone()
}

/// Whether this machine's jailer config is known-broken for direct-media access — from ss4s
/// `jail_check.c`: on `k5lp`/`k3lp`, a broken config leaves `/dev/rtkmem` unreadable and NDL v1
/// cannot reach the decoder.
pub fn jail_config_broken() -> bool {
    let Some(name) = machine_name() else {
        return false;
    };
    if name != "k5lp" && name != "k3lp" {
        return false;
    }
    let Ok(path) = CString::new(RTKMEM_DEVICE) else {
        return false;
    };
    // SAFETY: `path` is a valid, NUL-terminated C string for the duration of the call.
    unsafe { libc::access(path.as_ptr(), libc::R_OK) != 0 }
}

/// Which NDL `DirectMedia` generation to *try*, from the version ranges ss4s declares
/// (`ndl-webos5` `>=5`, `ndl-webos4` `>=3.5,<5`). Unknown defaults to `V2` — today's behaviour,
/// kept for every working device, including ones where Luna is unavailable. No fallback between
/// the two; `ndl`'s module docs say why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NdlGeneration {
    /// webOS 3.5-4.x: `NDL_DirectVideoOpen/SetCallback/SetArea/PlayWithCallback/Close`.
    /// H.264-only, SDR, no PTS input.
    V1,
    /// webOS 5+: `NDL_DirectVideoPlay(buffer, size, pts)` and friends. Today's behaviour.
    V2,
}

pub fn ndl_generation() -> NdlGeneration {
    static GENERATION: OnceLock<NdlGeneration> = OnceLock::new();
    *GENERATION.get_or_init(|| match sdk_version() {
        Some((major, _)) if major < 5 => NdlGeneration::V1,
        _ => NdlGeneration::V2,
    })
}

/// What this device may offer, from [`ndl_generation`]. Published once through
/// `core::caps::install` so the wire, the UI and settings load can't disagree.
pub fn video_caps() -> VideoCaps {
    match ndl_generation() {
        NdlGeneration::V2 => VideoCaps::FULL,
        NdlGeneration::V1 => VideoCaps::H264_SDR,
    }
}

impl DeviceInfo {
    pub fn detect() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let os = std::fs::read_to_string(OS_INFO).unwrap_or_default();
        let webos_major =
            json_str_field(&os, "webos_release").and_then(|v| v.split('.').next().and_then(|m| m.parse().ok()));
        let model = std::fs::read_to_string(DEVICE_INFO)
            .ok()
            .and_then(|t| json_str_field(&t, "product_id"));
        Self {
            cores,
            webos_major,
            model,
            sdk_version: sdk_version(),
            ota_id: ota_id(),
            machine_name: machine_name(),
        }
    }

    /// Log device details at startup — the primary triage artifact for a report from a model
    /// neither developer owns: which webOS, which `SoC`, which NDL generation, jail check ok.
    pub fn log(&self) {
        tracing::info!(
            "device: cores={} webos={} model={} sdk={} ota={} machine={} ndl_gen={:?} jail_broken={}",
            self.cores,
            self.webos_major
                .map_or_else(|| "unknown".to_string(), |v| v.to_string()),
            self.model.as_deref().unwrap_or("unknown"),
            self.sdk_version
                .map_or_else(|| "unknown".to_string(), |(maj, min)| format!("{maj}.{min}")),
            self.ota_id.as_deref().unwrap_or("unknown"),
            self.machine_name.as_deref().unwrap_or("unknown"),
            ndl_generation(),
            jail_config_broken(),
        );
    }
}
