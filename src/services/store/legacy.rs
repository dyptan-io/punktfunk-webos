//! One-shot migration off the pre-consolidation file-per-concern layout.
use super::{app_dir, save, KnownHost, Persisted, Settings};

/// Pre-consolidation files, deleted once [`migrate`] has written the document. Only
/// `known-hosts.json` is carried over: a pairing is a fingerprint the host approved once, and
/// losing it means re-pairing. The other two are deleted unread — `selected-host` rewrites itself
/// on the next pick, and `av-trim-ms` fed a calibration this client no longer has.
const FILES: [&str; 3] = ["known-hosts.json", "selected-host.json", "av-trim-ms.conf"];

/// Folds the pre-consolidation host list into the document and writes it, once. Absent legacy
/// files are not an error: a fresh install has none, nor does one already migrated.
pub(super) fn migrate(settings: Settings) -> Persisted {
    let dir = app_dir();
    let present: Vec<&str> = FILES.into_iter().filter(|f| dir.join(f).exists()).collect();
    let known_hosts: Vec<KnownHost> = std::fs::read_to_string(dir.join("known-hosts.json"))
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
    match save(&state) {
        // Only once the document is in place, so a failed write leaves them to migrate from again.
        Ok(()) => {
            for file in &present {
                let _ = std::fs::remove_file(dir.join(file));
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
