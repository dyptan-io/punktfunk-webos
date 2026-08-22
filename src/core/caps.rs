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
//! Two facts are stored: the NDL baseline (fixed for the run) and whether the pick is SMP (changes
//! while the app runs, see [`set_backend`]). Everything else — the active caps, the codecs worth
//! offering, whether a backend choice exists at all — derives from those.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::core::model::{CodecPref, VideoBackend};

/// Video capabilities of the backend this run will use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCaps {
    pub h265: bool,
    /// HDR (10-bit + mastering metadata). Implies [`Self::h265`].
    pub hdr: bool,
    /// Highest audio channel count this client can decode and present through the SOFTWARE
    /// route — the decoder-wide ceiling.
    pub max_channels: u8,
    /// Highest audio channel count NDL's audio plane can put on a speaker here
    /// (`ndl::audio_plane_max_channels`). A second, narrower ceiling that applies only to the
    /// routes that ride the plane — see `core::model::AudioRoutePref::max_channels`.
    pub plane_max_channels: u8,
}

impl VideoCaps {
    /// NDL `DirectMedia` v2 on webOS 5+, and SMP on any release — what every
    /// currently-working device gets, and the default until the platform installs something
    /// narrower.
    pub const FULL: Self = Self {
        h265: true,
        hdr: true,
        max_channels: 8,
        // Widest mode NDL's plane has. The platform installs the TV's real answer, which is
        // this or stereo; the constant only covers host builds and tests.
        plane_max_channels: 6,
    };

    /// NDL `DirectMedia` v1 on webOS 3.5-4.x. Stereo because its audio path is unused and
    /// 5.1/7.1 on a TV of that generation isn't worth offering.
    pub const H264_SDR: Self = Self {
        h265: false,
        hdr: false,
        max_channels: 2,
        // v1 has no audio type at all, so no route can ride a plane here.
        plane_max_channels: 2,
    };

    /// The codec preferences worth offering here, in display order — the one place the codec set
    /// is spelled, so the Settings dropdown, the persisted-document clamp and the advertised wire
    /// set can't disagree. Without HEVC only one codec is decodable, so `Automatic` would resolve
    /// to it anyway and the list collapses to a single entry — which is what hides the row (see
    /// `ui::settings`'s `row_shown`).
    pub fn codec_prefs(self) -> &'static [CodecPref] {
        if self.h265 {
            &[CodecPref::Auto, CodecPref::H264, CodecPref::Hevc]
        } else {
            &[CodecPref::H264]
        }
    }
}

/// What NDL can do on this TV — the platform's answer, fixed for the run. Unset reads as
/// [`VideoCaps::FULL`] (see the module docs).
static NDL_BASELINE: OnceLock<VideoCaps> = OnceLock::new();
/// Whether the current pick is SMP, i.e. whether the baseline is widened.
static SMP_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Publish the detected NDL caps. Call once, before settings load or any UI is built; later calls
/// are ignored.
pub fn install(ndl_caps: VideoCaps) {
    if NDL_BASELINE.set(ndl_caps).is_err() {
        tracing::warn!("video caps already installed — ignoring {ndl_caps:?}");
    }
}

/// Point the active caps at `backend`. SMP drives the same silicon through a richer front-end,
/// which does have HEVC and HDR on the releases NDL v1 does not — so the pick widens what this
/// client advertises, and the row/wire clamps follow from here.
pub fn set_backend(backend: VideoBackend) {
    let smp = effective_backend(backend) == VideoBackend::Smp;
    if SMP_ACTIVE.swap(smp, Ordering::Relaxed) != smp {
        tracing::info!("video caps now {:?} ({backend:?})", video_caps());
    }
}

/// The active caps: the NDL baseline, widened to [`VideoCaps::FULL`] while SMP is the pick.
pub fn video_caps() -> VideoCaps {
    if SMP_ACTIVE.load(Ordering::Relaxed) {
        VideoCaps::FULL
    } else {
        NDL_BASELINE.get().copied().unwrap_or(VideoCaps::FULL)
    }
}

/// Whether SMP is offerable — i.e. whether NDL here is the narrow v1 generation, the whole reason
/// to have a second backend. Matched against the named baseline rather than tested for "not FULL",
/// so a future profile that is merely narrow doesn't silently turn the row on. Not gated on SMP
/// actually loading: that is only knowable at load time, and a load failure falls back to NDL
/// (`session::connect`) rather than costing the user the choice.
pub fn smp_selectable() -> bool {
    NDL_BASELINE.get() == Some(&VideoCaps::H264_SDR)
}

pub fn effective_backend(pick: VideoBackend) -> VideoBackend {
    if pick == VideoBackend::Smp && !smp_selectable() {
        VideoBackend::Ndl
    } else {
        pick
    }
}
