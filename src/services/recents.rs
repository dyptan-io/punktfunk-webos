//! When each host last launched each game — what orders the Library section, most recently
//! played first.
//!
//! Its own file beside `settings.json` rather than a field on `GamePrefs`: the settings
//! document is the user's configuration and should not accumulate a timestamp per game they
//! ever pressed OK on. It is a cache, so every failure here is silent — an absent, unreadable
//! or truncated file simply reads as "nothing played", which is the ordering the app had
//! before this existed.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::services::paths::app_dir;

/// One host's play times by game id. What [`Library::regroup`](crate::app::library::Library)
/// sorts the dynamic section on, handed over as a borrow so the sort needs no lookups.
pub type HostRecents = HashMap<String, u64>;

/// Play times for every known host, keyed `"<host>:<port>"`.
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Recents {
    hosts: HashMap<String, HostRecents>,
}

/// The empty set every lookup for an unknown host answers with, so callers get a borrow
/// rather than an `Option` to thread through the sort.
static NONE: std::sync::LazyLock<HostRecents> = std::sync::LazyLock::new(HostRecents::new);

fn path() -> PathBuf {
    app_dir().join("recents.json")
}

fn key(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Recents {
    /// Reads the file, or answers empty — see the module note on why nothing here is an error.
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn for_host(&self, host: &str, port: u16) -> &HostRecents {
        self.hosts.get(&key(host, port)).unwrap_or(&NONE)
    }

    /// Stamps `id` as played now and writes immediately, rather than through
    /// [`StateWriter`](crate::services::store::StateWriter): a stream follows this call, and a
    /// coalesced write is the one a crash during it eats. One small file, once per launch.
    pub fn record(&mut self, host: &str, port: u16, id: &str) {
        self.hosts
            .entry(key(host, port))
            .or_default()
            .insert(id.to_string(), now());
        self.save();
    }

    /// Drops play times for ids `live` no longer names — run beside
    /// [`KnownHost::prune_games`](crate::core::model::KnownHost::prune_games), or the file
    /// grows forever with ids nothing can reach. `true` when something went, which is when
    /// the caller saves: unlike a launch, this rides a path that is already saving.
    pub fn prune(&mut self, host: &str, port: u16, live: impl Fn(&str) -> bool) -> bool {
        let Some(entry) = self.hosts.get_mut(&key(host, port)) else {
            return false;
        };
        let before = entry.len();
        entry.retain(|id, _| live(id));
        entry.len() != before
    }

    /// Drops a forgotten host's whole entry.
    pub fn forget_host(&mut self, host: &str, port: u16) -> bool {
        self.hosts.remove(&key(host, port)).is_some()
    }

    pub fn save(&self) {
        let Ok(json) = serde_json::to_string(self) else {
            return;
        };
        if let Err(e) = crate::services::atomic::write(&path(), &json, "recents") {
            tracing::warn!("recents write failed: {e}");
        }
    }
}
