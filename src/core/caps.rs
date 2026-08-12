//! What the active video backend can present — one fact, read by everything that must agree
//! about it: `session::connect` (what's advertised — authoritative, the codec is negotiated
//! before any decoder opens), `ui::settings` (what's offerable), `Settings::clamp_to_caps`
//! (normalising a document written on a more capable TV).
//!
//! In `core`, not `platform`, because `ui`/`services` can't depend on `platform::webos`. Hence
//! the install-once global: the platform layer publishes at startup, every layer reads. **Unset
//! reads as [`VideoCaps::FULL`]** — today's webOS 5+ behaviour, so host builds, tests and any
//! pre-install path see exactly what shipped before this existed.
//!
//! Stored, not derived: the NDL baseline (fixed for the run) and whether the pick is SMP (changes
//! while the app runs, see [`set_backend`]). [`video_caps`] is a function of the two.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::core::model::VideoBackend;

/// Video capabilities of the backend this run will use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCaps {
    pub h265: bool,
    /// HDR (10-bit + mastering metadata). Implies [`Self::h265`].
    pub hdr: bool,
    /// Highest audio channel count worth requesting from the host.
    pub max_channels: u8,
}

impl VideoCaps {
    /// NDL `DirectMedia` v2 on webOS 5+, and SMP on any release — what every
    /// currently-working device gets, and the default until the platform installs something
    /// narrower.
    pub const FULL: Self = Self {
        h265: true,
        hdr: true,
        max_channels: 8,
    };

    /// NDL `DirectMedia` v1 on webOS 3.5-4.x. Stereo because its audio path is unused and
    /// 5.1/7.1 on a TV of that generation isn't worth offering.
    pub const H264_SDR: Self = Self {
        h265: false,
        hdr: false,
        max_channels: 2,
    };
}

/// What NDL can do on this TV — the platform's answer, fixed for the run.
static NDL_BASELINE: OnceLock<VideoCaps> = OnceLock::new();
/// Whether the current pick is SMP, i.e. whether the baseline is widened.
static SMP_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Whether SMP is a legal pick at all — true only when NDL on this TV is the narrow v1
/// generation (the whole reason to offer another backend) *and* SMP can actually load. The
/// second half is not cosmetic: the pick widens what the handshake advertises, so offering a
/// backend that will fail to load buys an HEVC session no fallback can decode.
static SMP_SELECTABLE: AtomicBool = AtomicBool::new(false);

/// Publish the detected NDL caps and whether SMP is selectable — the caller owns that gate in full
/// (it has to, to skip the `dlopen` probe where the row can't be offered), so it is stored verbatim.
/// Call once, before settings load or any UI is built; later calls are ignored.
pub fn install(ndl_caps: VideoCaps, smp_available: bool) {
    if NDL_BASELINE.set(ndl_caps).is_err() {
        tracing::warn!("video caps already installed — ignoring {ndl_caps:?}");
        return;
    }
    SMP_SELECTABLE.store(smp_available, Ordering::Relaxed);
}

/// Point the active caps at `backend`. SMP drives the same silicon through a richer front-end,
/// which does have HEVC and HDR on the releases NDL v1 does not — so the pick widens what this
/// client advertises, and the row/wire clamps follow from here.
pub fn set_backend(backend: VideoBackend) {
    let smp = backend == VideoBackend::Smp && smp_selectable();
    if SMP_ACTIVE.swap(smp, Ordering::Relaxed) != smp {
        tracing::info!("video caps now {:?} ({backend:?})", video_caps());
    }
}

/// The NDL caps the platform installed, or [`VideoCaps::FULL`] if nothing was installed.
fn ndl_baseline() -> VideoCaps {
    NDL_BASELINE.get().copied().unwrap_or(VideoCaps::FULL)
}

/// Whether the backend row is worth offering — see [`SMP_SELECTABLE`].
pub fn smp_selectable() -> bool {
    SMP_SELECTABLE.load(Ordering::Relaxed)
}

/// The active caps (see the module docs).
pub fn video_caps() -> VideoCaps {
    if SMP_ACTIVE.load(Ordering::Relaxed) {
        VideoCaps::FULL
    } else {
        ndl_baseline()
    }
}
