//! Persistence: the `settings.json` document, plus the client identity PEMs beside it.
//!
//! - [`load`] / [`save`] read and write the whole [`Persisted`] document.
//! - [`StateWriter`] is what the app actually saves through: off-thread, coalescing.
//! - [`load_or_create_identity`] handles the PEM pair, which stays outside the document.
// The shell's `SettingsStore`. Gated with pf-console-ui itself (see Cargo.toml).
#[cfg(target_os = "linux")]
pub mod console;
mod identity;
mod legacy;
mod legacy_settings;
pub mod shared;
mod writer;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

pub use crate::core::model::{
    new_host_collections, upsert_known_host, AudioRoutePref, CodecPref, Collection, ExitAction, GamepadType, KnownHost,
    LogLevelOverride, Persisted, DESKTOP_PIN_ID,
};
pub use crate::core::settings::TvSettings;
pub use crate::services::paths::app_dir;
pub use identity::load_or_create_identity;
pub use pf_client_core::trust::Settings;
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
        Some(doc) if doc.get("settings").is_some() => from_document(doc),
        Some(doc) => legacy::migrate(legacy::upgrade(&serde_json::from_value(doc).unwrap_or_default())),
        None => legacy::migrate(crate::core::settings::default_document()),
    };
    apply_launch_overrides(&mut state);
    let new_build = stamp_version(&mut state);
    migrate_collections(&mut state);
    // A document written on a more capable TV can hold HEVC, HDR and 7.1 on a device with none
    // of them — leaving a *set* value whose row the UI hides.
    state.settings.clamp_to_caps();
    Loaded { state, new_build }
}

/// The version this build writes into the document.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Brings [`Persisted::version`] up to this build's, and returns whether it had to — which is
/// how the UI knows this release is running here for the first time.
///
/// Written synchronously so it happens once — `StateWriter`'s baseline is taken after this,
/// and an unstamped document that never gets saved would stamp again on every launch.
fn stamp_version(state: &mut Persisted) -> bool {
    if state.version.as_deref() == Some(VERSION) {
        return false;
    }
    state.version = Some(VERSION.to_string());
    if let Err(e) = save(state) {
        tracing::warn!("could not stamp document version: {e:#}");
        return true;
    }
    tracing::info!("stamped settings.json with version {VERSION}");
    true
}

/// Gives every host that predates collections the vector it now needs: its old pins, in pin
/// order, as one "Pinned" collection, then the dynamic Library entry — which is exactly the
/// grid it was already drawing. A host with no pins gets Library alone.
///
/// The last reader of the legacy pin. One-shot in practice, since the first save writes `collections` and stops serializing
/// `pin` — but idempotent regardless: a host that already has the vector is skipped.
fn migrate_collections(state: &mut Persisted) {
    for host in &mut state.known_hosts {
        if !host.needs_migration() {
            continue;
        }
        let mut pinned: Vec<(u32, &str)> = host
            .games
            .iter()
            .filter_map(|(id, g)| g.legacy_pin.map(|p| (p, id.as_str())))
            .collect();
        pinned.sort_unstable();
        let mut collections = Vec::with_capacity(2);
        if !pinned.is_empty() {
            let mut collection = Collection::new(crate::core::model::PINNED_COLLECTION);
            collection.games = pinned.into_iter().map(|(_, id)| id.to_string()).collect();
            tracing::info!(
                "migrated {} pins on {} into a collection",
                collection.games.len(),
                host.name
            );
            collections.push(collection);
        }
        collections.push(Collection::library());
        host.set_collections(collections);
    }
}

/// The document, with its `settings` object read out of the shared schema into this client's
/// own shape (see [`shared`]). A pre-shared object is converted here once; the next [`save`]
/// writes it back in the shared schema.
fn from_document(mut doc: Value) -> Persisted {
    let object = doc.get("settings").cloned().unwrap_or(Value::Null);
    if legacy::legacy_shape(&object) {
        tracing::info!("settings.json predates the shared schema — converting on this load");
        let upgraded = legacy::upgrade(&serde_json::from_value(object).unwrap_or_default());
        match serde_json::to_value(upgraded) {
            Ok(v) => doc["settings"] = v,
            Err(e) => tracing::warn!("could not re-encode settings: {e:#}"),
        }
    }
    serde_json::from_value(doc).unwrap_or_default()
}

pub fn save(state: &Persisted) -> Result<()> {
    let doc = serde_json::to_value(state).context("serialize app state")?;
    let json = serde_json::to_string_pretty(&doc).context("render app state")?;
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
    // Both shapes: the shared schema keeps this client's own rows under a `webos.` prefix, and
    // a document written before the move still has it bare. Reading only one of them would
    // silently start every launch at the default level instead of the chosen one.
    settings
        .get("webos.log_level_override")
        .or_else(|| settings.get("log_level_override"))
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
        state.settings.set_log_level_override(level);
    }
}
