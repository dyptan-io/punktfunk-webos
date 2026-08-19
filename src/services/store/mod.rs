//! Persistence: the `settings.json` document, plus the client identity PEMs beside it.
//!
//! - [`load`] / [`save`] read and write the whole [`Persisted`] document.
//! - [`StateWriter`] is what the app actually saves through: off-thread, coalescing.
//! - [`load_or_create_identity`] handles the PEM pair, which stays outside the document.
mod identity;
mod legacy;
mod writer;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

pub use crate::core::model::{
    desktop_capture_override, new_host_games, upsert_known_host, CodecPref, GamepadType, KnownHost, LogLevelOverride,
    OverrideField, Persisted, Settings, SettingsOverride, VideoBackend, DESKTOP_PIN_ID,
};
pub use crate::services::paths::app_dir;
pub use identity::load_or_create_identity;
pub use writer::StateWriter;

fn path() -> PathBuf {
    app_dir().join("settings.json")
}

fn read_document() -> Option<Value> {
    let text = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Loads the whole persisted document. Absent, unreadable and unparseable all answer with
/// defaults — a torn file must not take the app down (`services::atomic` is what prevents one).
///
/// Migrates the pre-consolidation layout in place, so callers never see the old shape.
/// [`load`]'s result: the document, plus whether this build is the first to write its version
/// into it — the one-shot signal the UI uses to introduce what the release added.
pub struct Loaded {
    pub state: Persisted,
    pub new_build: bool,
}

pub fn load() -> Loaded {
    // A nested `settings` key means the current shape. Otherwise the fields sit at the top level
    // — or the file is missing entirely, which still needs migrating, since pairing a host wrote
    // `known-hosts.json` without ever writing settings.
    let mut state = match read_document() {
        Some(doc) if doc.get("settings").is_some() => serde_json::from_value(doc).unwrap_or_default(),
        Some(doc) => legacy::migrate(serde_json::from_value(doc).unwrap_or_default()),
        None => legacy::migrate(Settings::default()),
    };
    apply_launch_overrides(&mut state);
    let new_build = stamp_version(&mut state);
    // A document written on a more capable TV can hold HEVC, HDR and 7.1 on a device with none
    // of them — leaving a *set* value whose row the UI hides.
    state.settings.clamp_to_caps();
    Loaded { state, new_build }
}

/// Just the persisted backend pick, for `core::caps` at startup. Not [`load`]: that clamps
/// against the caps this very value decides, so it has to be read first. Reads either
/// document shape (same reasoning as [`persisted_log_level`]).
pub fn persisted_video_backend() -> VideoBackend {
    let Some(doc) = read_document() else {
        return VideoBackend::default();
    };
    let settings = doc.get("settings").unwrap_or(&doc);
    settings
        .get("video_backend")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// The version this build writes into the document.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Brings [`Persisted::version`] up to this build's, and returns whether it had to — which is
/// how the UI knows this release is running here for the first time.
///
/// A document with *no* version predates the field, so it also gets
/// [`desktop_capture_override`] bootstrapped onto its hosts; one that carries any version was
/// written by a build that applied that default when each host was added.
///
/// Written synchronously so it happens once — `StateWriter`'s baseline is taken after this, and
/// an unstamped document that never gets saved would seed again on every launch.
fn stamp_version(state: &mut Persisted) -> bool {
    if state.version.as_deref() == Some(VERSION) {
        return false;
    }
    if state.version.is_none() {
        seed_desktop_capture(state);
    }
    state.version = Some(VERSION.to_string());
    if let Err(e) = save(state) {
        tracing::warn!("could not stamp document version: {e:#}");
        return true;
    }
    tracing::info!("stamped settings.json with version {VERSION}");
    true
}

/// Puts [`desktop_capture_override`] on every host whose Desktop card is pinned and
/// which carries no overrides yet. A host that already overrides something is left alone — the
/// user has found the feature and this must not talk over them.
fn seed_desktop_capture(state: &mut Persisted) {
    let Some(capture) = desktop_capture_override(&state.settings) else {
        return;
    };
    for host in &mut state.known_hosts {
        let untouched = host.games.values().all(|g| g.over.is_empty());
        if !untouched || !host.is_pinned(DESKTOP_PIN_ID) {
            continue;
        }
        host.edit_overrides(DESKTOP_PIN_ID, |over| over.cursor_capture = Some(capture));
        tracing::info!("seeded Desktop cursor-capture override on {}", host.name);
    }
}

pub fn save(state: &Persisted) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("serialize app state")?;
    crate::services::atomic::write(&path(), &json, "settings.json")
}

/// Just the persisted log level, for `logger`'s startup filter. Not [`load`]: that migrates, and
/// this runs before the subscriber exists, so the migration's log lines would be dropped. Reads
/// either document shape.
pub fn persisted_log_level() -> LogLevelOverride {
    if let Some(level) = crate::logger::launch_level_override() {
        return level;
    }
    let Some(doc) = read_document() else {
        return LogLevelOverride::default();
    };
    let settings = doc.get("settings").unwrap_or(&doc);
    settings
        .get("log_level_override")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// `task deploy TELEMETRY=...` dev convenience: `TELEMETRY_LEVEL` picks the level this launch
/// starts at (and what Diagnostics displays), overriding whatever was last persisted — see
/// `logger::launch_level_override`. Absent, the persisted value stands (Info on a fresh install).
///
/// [`StateWriter`]'s baseline is taken after this, so a launch that only overrides the level writes
/// nothing. It is NOT otherwise held out of the document, though: the override lands in `App`'s
/// [`Settings`], so the next unrelated save persists it and a later plain launch starts at the
/// overridden level. Pre-dates the consolidation; fixing it means separating the level the logger
/// runs at from the one Diagnostics displays and saves.
fn apply_launch_overrides(state: &mut Persisted) {
    if let Some(level) = crate::logger::launch_level_override() {
        state.settings.log_level_override = level;
    }
}
