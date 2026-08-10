//! The streaming loop's view of a live session.
//!
//! Inherent methods on [`Connected`] for the handful of figures the loop needs (stats, overlay,
//! teardown), kept here rather than in `session` because every one of them is shaped by what the
//! loop asks for, not by how the session works.

use std::sync::Arc;

use punktfunk_core::input::InputEvent;

use crate::platform::webos::audio::AudioFeed;
use crate::session::{self, Connected, StreamStats};

/// The input path, cloned for a thread that sends off the main loop (the HID-mouse reader).
#[derive(Clone)]
pub(crate) struct InputSender(Arc<punktfunk_core::client::NativeClient>);

impl InputSender {
    pub(crate) fn send(&self, ev: &InputEvent) {
        let _ = session::send_input(&self.0, ev);
    }
}

impl Connected {
    pub(crate) fn input(&self) -> InputSender {
        InputSender(self.client.clone())
    }

    pub(crate) fn send_input(&self, ev: &InputEvent) {
        let _ = session::send_input(&self.client, ev);
    }

    pub(crate) fn stats(&self) -> &Arc<StreamStats> {
        &self.stats
    }

    /// Whether HDR is being applied, for the Game-mode picture pick.
    pub(crate) fn hdr(&self) -> bool {
        self.hdr
    }

    /// Channels to open the SDL audio device with, or `None` when the session needs no local
    /// device (NDL audio offload took the stream).
    pub(crate) fn audio_channels(&self) -> Option<u8> {
        if self.audio_offloaded {
            None
        } else {
            Some(self.client.audio_channels)
        }
    }

    /// The cells the A/V sync loop trades through — handed to the audio player at construction.
    pub(crate) fn sync_cells(&self) -> crate::platform::webos::audio::SyncCells {
        crate::platform::webos::audio::SyncCells {
            clock_offset: self.client.clock_offset_shared(),
            video_e2e: self.client.video_e2e_shared(),
            av_offset_ms: self.client.audio_av_offset_shared(),
            buffer_ms: self.client.audio_buffer_ms_shared(),
        }
    }

    /// Audio's two HUD figures: ring depth in ms, and the smoothed A/V offset in ms (positive =
    /// audio playing behind the picture). Both are `0` until the sync loop has evidence.
    pub(crate) fn audio_stats(&self) -> (u32, i64) {
        (self.client.audio_buffer_ms(), self.client.audio_av_offset_ms())
    }

    /// Starts the audio decode/feed thread. It exits on the session's stop flag, or when the
    /// transport's audio plane closes.
    pub(crate) fn spawn_audio_feed(&self, feed: AudioFeed) -> anyhow::Result<std::thread::JoinHandle<()>> {
        session::spawn_audio_feed(self.client.clone(), feed, self.stop.clone())
    }

    /// Signals the audio feed thread to stop and joins it, bounded. Sets the session's stop flag,
    /// which `shutdown()` sets moments later anyway — doing it here just means the ring stops being
    /// fed before its device is dropped.
    pub(crate) fn stop_audio_feed(&self, handle: std::thread::JoinHandle<()>) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        session::join_audio_feed(handle);
    }

    /// Drains the host→client pad feedback planes.
    pub(crate) fn pump_feedback_once(
        &self,
        controller: Option<&mut sdl2::controller::GameController>,
        feedback: Option<&mut crate::platform::webos::dualsense::Feedback>,
    ) {
        session::pump_feedback_once(&self.client, controller, feedback);
    }

    pub(crate) fn is_session_ended(&self) -> bool {
        self.client.is_session_ended()
    }

    /// The sentence for the menu toast when the session ended on its own. Nothing here separates
    /// a graceful close from a drop, so one sentence covers both.
    pub(crate) fn end_message(&self) -> String {
        "The host closed the connection".to_string()
    }

    pub(crate) fn disconnect_quit(&self) {
        self.client.disconnect_quit();
    }

    /// The stats overlay's figures. Grouped into one call so the overlay block has one lookup
    /// rather than six.
    pub(crate) fn overlay_info(&self) -> OverlayInfo {
        let client = &self.client;
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

/// See [`Connected::overlay_info`].
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
