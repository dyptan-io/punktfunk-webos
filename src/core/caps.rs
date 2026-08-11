//! What the active video backend can present — one fact, read by everything that must agree
//! about it: `session::connect` (what's advertised — authoritative, the codec is negotiated
//! before any decoder opens), `ui::settings` (what's offerable), `store::load` (normalising a
//! document written on a more capable TV).
//!
//! In `core`, not `platform`, because `ui`/`services` can't depend on `platform::webos`. Hence
//! the install-once global: the platform layer publishes at startup, every layer reads. **Unset
//! reads as [`VideoCaps::FULL`]** — today's webOS 5+ behaviour, so host builds, tests and any
//! pre-install path see exactly what shipped before this existed.
use std::sync::OnceLock;

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
    /// NDL `DirectMedia` v2 on webOS 5+ — what every currently-working device gets, and the
    /// default until the platform layer installs something narrower.
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

static CAPS: OnceLock<VideoCaps> = OnceLock::new();

/// Publish the detected caps. Call once, before settings load or any UI is built; later calls
/// are ignored (the first install is what consumers have already read).
pub fn install(caps: VideoCaps) {
    if CAPS.set(caps).is_err() {
        tracing::warn!("video caps already installed — ignoring {caps:?}");
    }
}

/// The installed caps, or [`VideoCaps::FULL`] if nothing was installed (see the module docs).
pub fn video_caps() -> VideoCaps {
    CAPS.get().copied().unwrap_or(VideoCaps::FULL)
}
