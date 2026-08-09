//! One live stream.
//!
//! A wrapper around [`Connected`] rather than the session type itself, so the streaming loop has
//! one place to reach for the handful of figures it needs (stats, overlay, teardown) instead of
//! peering into the session's internals at each site. Verbs are named after the punktfunk ones
//! the loop already called (`disconnect_quit`, `is_session_ended`, `shutdown`).

use std::sync::Arc;

use punktfunk_core::input::InputEvent;

use crate::platform::webos::audio::AudioPlayer;
use crate::session::{self, Connected, StreamStats};

pub(crate) struct StreamHandle(pub(crate) Connected);

/// The input path, cloned for a thread that sends off the main loop (the HID-mouse reader).
#[derive(Clone)]
pub(crate) struct InputSender(Arc<punktfunk_core::client::NativeClient>);

impl InputSender {
    pub(crate) fn send(&self, ev: &InputEvent) {
        let _ = session::send_input(&self.0, ev);
    }
}

impl StreamHandle {
    pub(crate) fn input(&self) -> InputSender {
        InputSender(self.0.client.clone())
    }

    pub(crate) fn send_input(&self, ev: &InputEvent) {
        let _ = session::send_input(&self.0.client, ev);
    }

    pub(crate) fn stats(&self) -> &Arc<StreamStats> {
        &self.0.stats
    }

    /// Whether HDR is being applied, for the Game-mode picture pick.
    pub(crate) fn hdr(&self) -> bool {
        self.0.hdr
    }

    /// Channels to open the SDL audio device with, or `None` when the session needs no local
    /// device (NDL audio offload took the stream).
    pub(crate) fn audio_channels(&self) -> Option<u8> {
        if self.0.audio_offloaded {
            None
        } else {
            Some(self.0.client.audio_channels)
        }
    }

    /// The cells the A/V sync loop trades through — handed to the audio player at construction.
    pub(crate) fn sync_cells(&self) -> crate::platform::webos::audio::SyncCells {
        crate::platform::webos::audio::SyncCells {
            clock_offset: self.0.client.clock_offset_shared(),
            video_e2e: self.0.client.video_e2e_shared(),
            av_offset_ms: self.0.client.audio_av_offset_shared(),
            buffer_ms: self.0.client.audio_buffer_ms_shared(),
        }
    }

    /// Audio's two HUD figures: ring depth in ms, and the smoothed A/V offset in ms (positive =
    /// audio playing behind the picture). Both are `0` until the sync loop has evidence.
    pub(crate) fn audio_stats(&self) -> (u32, i64) {
        (self.0.client.audio_buffer_ms(), self.0.client.audio_av_offset_ms())
    }

    /// Drains decoded audio into the device. Call once per tick.
    pub(crate) fn pump_audio_once(&self, audio: &mut AudioPlayer) {
        session::pump_audio_once(&self.0.client, audio);
    }

    /// Drains the host→client pad feedback planes.
    pub(crate) fn pump_feedback_once(
        &self,
        controller: Option<&mut sdl2::controller::GameController>,
        feedback: Option<&mut crate::platform::webos::dualsense::Feedback>,
    ) {
        session::pump_feedback_once(&self.0.client, controller, feedback);
    }

    pub(crate) fn is_session_ended(&self) -> bool {
        self.0.client.is_session_ended()
    }

    /// The sentence for the menu toast when the session ended on its own. Nothing here separates
    /// a graceful close from a drop, so one sentence covers both.
    pub(crate) fn end_message(&self) -> String {
        "The host closed the connection".to_string()
    }

    pub(crate) fn disconnect_quit(&self) {
        self.0.client.disconnect_quit();
    }

    /// `false` = teardown timed out, so the caller must skip `ndl::quit()`.
    pub(crate) fn shutdown(self) -> bool {
        self.0.shutdown()
    }

    /// The stats overlay's figures. Grouped into one call so the overlay block has one lookup
    /// rather than six.
    pub(crate) fn overlay_info(&self) -> OverlayInfo {
        let client = &self.0.client;
        let mode = client.mode();
        OverlayInfo {
            width: mode.width,
            height: mode.height,
            refresh_hz: mode.refresh_hz,
            codec: session::codec_name(client.codec).to_string(),
            hdr: client.color.is_hdr(),
            frames_dropped: Some(client.frames_dropped()),
            fec_recovered: Some(client.fec_recovered_shards()),
            // The encoder's CURRENT target, not the session-start negotiation: on Automatic the
            // ABR re-targets mid-session. `0` = a host too old to report.
            target_kbps: match client.current_bitrate_kbps() {
                0 => client.resolved_bitrate_kbps,
                live => live,
            },
        }
    }
}

/// See [`StreamHandle::overlay_info`].
pub(crate) struct OverlayInfo {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub codec: String,
    pub hdr: bool,
    pub frames_dropped: Option<u64>,
    pub fec_recovered: Option<u64>,
    pub target_kbps: u32,
}
