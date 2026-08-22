//! Connects to a punktfunk host and drives the video/audio hardware pipelines.
//!
//! Video runs on a dedicated thread (`pump`'s video pump), which pulls access units off the
//! transport and hands them to `stage`'s `VideoStage` — PTS anchoring, freeze-until-reanchor and
//! backlog metering, all of it written against `core::media`'s `VideoSink` rather than against
//! any one backend.
//!
//! Audio takes one of two paths, and each has a thread of its own: software-decoded audio is
//! decoded by `pump`'s audio feed thread into the playback ring SDL's audio callback
//! drains (`platform::webos::audio`), and the NDL-offloaded path hands raw Opus straight to NDL
//! from `pump`'s NDL audio pump. Neither shares the main loop, which carries the UI's software
//! rasterizer.
//!
//! The module is split by phase: `connect` brings a session up, `pump` keeps it fed,
//! and `probe` holds the two handshake-only connections (pairing, speed test). `stage`,
//! `timeline`, `stats`, `priority` and `join` are the shared pieces underneath.
//!
//! Nothing here touches SDL: the pad-feedback drain, which does, lives with the loop that owns
//! the SDL objects (`runtime::session_ext`).
mod connect;
mod join;
mod priority;
pub mod probe;
mod pump;
mod stage;
mod stats;
mod timeline;

pub use connect::{connect, ConnectParams, Connected};
pub use pump::{join_audio_feed, spawn_audio_feed};
pub use stats::{codec_name, StreamStats};
