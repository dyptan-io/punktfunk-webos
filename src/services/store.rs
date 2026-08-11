//! Persisted identity (PEMs), known hosts, and settings (JSON). Layout mirrors `pf-client-core::trust`.
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use crate::core::model::{
    CodecPref, GamepadType, KnownHost, LogLevelOverride, Persisted, Settings, DESKTOP_PIN_ID,
};
pub(crate) use crate::services::paths::app_dir;

fn identity_paths() -> (PathBuf, PathBuf) {
    let dir = app_dir();
    (dir.join("client-cert.pem"), dir.join("client-key.pem"))
}

/// Load or generate identity (on first run).
pub fn load_or_create_identity() -> Result<(String, String)> {
    let (cert_path, key_path) = identity_paths();
    if let (Ok(cert), Ok(key)) = (std::fs::read_to_string(&cert_path), std::fs::read_to_string(&key_path)) {
        return Ok((cert, key));
    }
    let identity = punktfunk_core::quic::endpoint::generate_identity().context("generate_identity")?;
    std::fs::write(&cert_path, &identity.0).context("write client-cert.pem")?;
    std::fs::write(&key_path, &identity.1).context("write client-key.pem")?;
    Ok(identity)
}

/// Write-then-rename, never truncate-in-place: `std::fs::write` truncates first,
/// so a kill/power-cut mid-write (this is a TV — losing power IS the off switch)
/// leaves a half-file, and the loaders' `.ok().unwrap_or_default()` would then
/// silently discard every paired host / all settings. A rename on the same
/// filesystem is atomic; readers see the old file or the new one, never a torn one.
fn write_atomic(path: std::path::PathBuf, contents: &str, what: &'static str) -> Result<()> {
    write_atomic_parts(&path, &[contents.as_bytes()], what)
}

/// Same discipline for byte payloads that arrive in pieces (a header plus a pixel buffer, say):
/// the parts are written in order, so nothing has to be concatenated into one allocation first.
///
/// `.tmp` is appended to the whole filename rather than replacing an extension, which would make
/// `id.raw` and `id` — two files the art cache keeps side by side — stage to the same path.
pub(crate) fn write_atomic_parts(path: &Path, parts: &[&[u8]], what: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = std::fs::File::create(&tmp).with_context(|| format!("create {what} (tmp)"))?;
    for part in parts {
        file.write_all(part).with_context(|| format!("write {what} (tmp)"))?;
    }
    drop(file);
    std::fs::rename(&tmp, path).with_context(|| format!("rename {what} into place"))
}

/// Upserts by `(host, port)`, keeping the existing fingerprint if the new record is unpaired
/// (a fresh mDNS discovery shouldn't clobber a paired host) — same reasoning for `mac`,
/// learned separately (see `App::drain_discovery`) and not necessarily known again at the
/// point something else re-upserts this host. `pinned` is *always* kept from the existing
/// record — only `KnownHost::toggle_pin` ever changes it, so no add/edit/re-pair flow may
/// clobber it.
pub fn upsert_known_host(hosts: &mut Vec<KnownHost>, mut new: KnownHost) {
    if let Some(existing) = hosts.iter_mut().find(|h| h.host == new.host && h.port == new.port) {
        if !new.is_paired() {
            new.fingerprint = existing.fingerprint;
        }
        if new.mac.is_empty() {
            new.mac.clone_from(&existing.mac);
        }
        new.pinned.clone_from(&existing.pinned);
        // Per-host preference, not something a re-pair/re-add should reset.
        new.wol_auto = existing.wol_auto;
        *existing = new;
    } else {
        hosts.push(new);
    }
}

fn state_path() -> PathBuf {
    app_dir().join("settings.json")
}

/// Pre-consolidation files, deleted by the migration in [`load_state`]. Only `known-hosts.json`
/// is carried over: a pairing is a fingerprint the host approved once, and losing it means
/// re-pairing. The other two are deleted unread — `selected-host` rewrites itself on the next
/// pick, and `av-trim-ms` fed a calibration this client no longer has.
const LEGACY_FILES: [&str; 3] = ["known-hosts.json", "selected-host.json", "av-trim-ms.conf"];

/// Loads the whole persisted document. Absent, unreadable and unparseable all answer with
/// defaults — a torn file must not take the app down ([`write_atomic`] is what prevents one).
///
/// Migrates the pre-consolidation layout in place, so callers never see the old shape.
pub fn load_state() -> Persisted {
    let doc = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    // A nested `settings` key means the current shape. Otherwise the fields sit at the top level
    // — or the file is missing entirely, which still needs migrating, since pairing a host wrote
    // `known-hosts.json` without ever writing settings.
    let legacy_settings = match doc {
        Some(doc) if doc.get("settings").is_some() => {
            let mut state: Persisted = serde_json::from_value(doc).unwrap_or_default();
            apply_launch_overrides(&mut state);
            return state;
        }
        Some(doc) => serde_json::from_value(doc).unwrap_or_default(),
        None => Settings::default(),
    };
    let mut state = migrate_legacy(legacy_settings);
    apply_launch_overrides(&mut state);
    state
}

/// Just the persisted log level, for `logger`'s startup filter. Not [`load_state`]: that migrates,
/// and this runs before the subscriber exists, so the migration's log lines would be dropped.
/// Reads either document shape.
pub fn persisted_log_level() -> LogLevelOverride {
    if let Some(level) = crate::logger::launch_level_override() {
        return level;
    }
    let Some(doc) = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
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
/// `StateWriter`'s baseline is taken after this, so a launch that only overrides the level writes
/// nothing. It is NOT otherwise held out of the document, though: the override lands in `App`'s
/// `Settings`, so the next unrelated save persists it and a later plain launch starts at the
/// overridden level. Pre-dates the consolidation; fixing it means separating the level the logger
/// runs at from the one Diagnostics displays and saves.
fn apply_launch_overrides(state: &mut Persisted) {
    if let Some(level) = crate::logger::launch_level_override() {
        state.settings.log_level_override = level;
    }
}

/// Folds the pre-consolidation host list into the document and writes it, once. Absent legacy
/// files are not an error: a fresh install has none, nor does one already migrated.
fn migrate_legacy(settings: Settings) -> Persisted {
    let present: Vec<&str> = LEGACY_FILES
        .into_iter()
        .filter(|f| app_dir().join(f).exists())
        .collect();
    let known_hosts: Vec<KnownHost> = std::fs::read_to_string(app_dir().join("known-hosts.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let state = Persisted {
        settings,
        known_hosts,
        selected_host: None,
    };
    if present.is_empty() {
        return state;
    }
    // Synchronous rather than through `StateWriter`: the write is what licenses deleting the old
    // files, and doing it here keeps the writer's baseline equal to what is on disk.
    match save_state(&state) {
        Ok(()) => {
            // Only once the document is in place, so a failed write leaves them to migrate from
            // again.
            for file in present.iter() {
                let _ = std::fs::remove_file(app_dir().join(file));
            }
            tracing::info!(
                "migrated {} host(s) into settings.json, removed {present:?}",
                state.known_hosts.len()
            );
        }
        Err(e) => tracing::warn!("legacy state migration failed, leaving old files in place: {e:#}"),
    }
    state
}

pub fn save_state(state: &Persisted) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("serialize app state")?;
    write_atomic(state_path(), &json, "settings.json")
}

/// Persists the document on a dedicated background thread instead of the caller's —
/// `save_state`'s write-then-rename blocks on real disk I/O (measured ~100-200ms
/// on-device), which is fine for the occasional save but was stalling the UI thread
/// on every single settings-row adjustment (bitrate slider steps, a toggle flip),
/// reading as input lag on the very controls someone expects to feel instant.
///
/// A single long-lived writer thread, not one spawn per save: rapid adjustments
/// (holding the bitrate slider) replace the pending value rather than queuing every
/// intermediate one, so a burst of changes costs one disk write of the final state,
/// not N — and, since one thread ever calls `save_state`, writes can't complete
/// out of order the way N independently-spawned threads racing the filesystem could.
///
/// It carries the whole document, so a host edit and a settings change can't race into
/// disagreeing files — whichever snapshot lands last came from the same in-memory state.
///
/// **Unchanged snapshots never reach the disk.** Callers hand over the whole document, and several
/// fire on events that usually change nothing (an mDNS reply repeating a known MAC, re-selecting
/// the active host, leaving Settings untouched). With `spawn`'s baseline being what was just
/// loaded, an unchanged launch writes zero times: durable state, not a scratch file.
pub struct StateWriter {
    pending: std::sync::Arc<(std::sync::Mutex<Option<Persisted>>, std::sync::Condvar)>,
    /// The last snapshot queued. Separate from `pending`, which the worker empties as it writes —
    /// the comparison has to outlive that.
    last: std::sync::Mutex<Persisted>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `None` only after `Drop` has taken and joined it.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StateWriter {
    /// `baseline` is the document as loaded from disk, so a save matching it is a no-op.
    pub fn spawn(baseline: Persisted) -> Self {
        let state = std::sync::Arc::new((std::sync::Mutex::new(None::<Persisted>), std::sync::Condvar::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let (lock, cvar) = &*worker_state;
            loop {
                let mut guard = lock.lock().expect("state-writer mutex poisoned");
                while guard.is_none() && !worker_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    guard = cvar.wait(guard).expect("state-writer mutex poisoned");
                }
                let Some(state) = guard.take() else {
                    return; // stopped with nothing pending
                };
                drop(guard);
                let _ = save_state(&state);
            }
        });
        Self {
            pending: state,
            last: std::sync::Mutex::new(baseline),
            stop,
            thread: Some(thread),
        }
    }

    /// Queues `state`, replacing any snapshot not yet written and dropping one equal to the last
    /// queued. Returns immediately — never writes on the calling thread.
    pub fn save(&self, state: Persisted) {
        let mut last = self.last.lock().expect("state-writer mutex poisoned");
        if *last == state {
            return;
        }
        last.clone_from(&state);
        drop(last);
        let (lock, cvar) = &*self.pending;
        *lock.lock().expect("state-writer mutex poisoned") = Some(state);
        cvar.notify_one();
    }
}

impl Drop for StateWriter {
    /// Wakes the worker with `stop` set so it exits after flushing any pending save,
    /// then joins it — otherwise every menu re-entry (a fresh `App`, a fresh
    /// `StateWriter`) leaked one thread parked forever on the `Condvar`.
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.pending.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
