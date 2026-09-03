//! The gamepad shell's window onto this client's one document.
//!
//! `pf_console_ui::SettingsStore` is how a non-desktop host lends the shared shell its
//! persistence. The desktop implementation reads `trust::Settings::{load, save}` directly, which
//! this client cannot reuse: that path is hardcoded to `client-gtk-settings.json`, and the TV's
//! settings belong in its app directory beside everything else it keeps. Per-client paths are
//! already the norm — Windows has its own — so what is shared here is the SCHEMA, not the file.
//!
//! 🛑 Writes go through the app's [`StateWriter`], never straight to disk. That writer carries
//! the whole document precisely so a host edit and a settings change cannot race into
//! disagreeing files, and it drops snapshots equal to the last. A second writer behind its back
//! would resurrect exactly the race its docs describe: the app's next save would carry its own
//! stale `settings` and silently undo whatever the shell had just written.
//!
//! Only one UI is live at a time (that is what the flip means), so this owns the document while
//! the console is up and [`ConsoleStore::snapshot`] hands it back when the console closes.

// `expect`, not `allow`: nothing constructs this until the console host lands, and this then
// fails the build the moment it does — so the marker cannot outlive its reason.
#![expect(dead_code, reason = "the console host is the consumer; it lands next")]

use std::sync::{Arc, Mutex};

use pf_client_core::trust;
use pf_console_ui::SettingsStore;

use super::shared;
use super::{Persisted, StateWriter};

/// Poisoning means a panic while holding the document; nothing here can recover from it.
const POISONED: &str = "console-store mutex poisoned";

pub struct ConsoleStore {
    state: Mutex<Persisted>,
    writer: Arc<StateWriter>,
}

impl ConsoleStore {
    /// `state` is the document as the app has it right now; `writer` is the app's own.
    pub fn new(state: Persisted, writer: Arc<StateWriter>) -> Self {
        Self {
            state: Mutex::new(state),
            writer,
        }
    }

    /// The document as the console leaves it — what the other UI adopts when the flip returns,
    /// so a setting changed in the shell is not stale in the old screens.
    pub fn snapshot(&self) -> Persisted {
        self.state.lock().expect(POISONED).clone()
    }
}

impl SettingsStore for ConsoleStore {
    fn load(&self) -> trust::Settings {
        shared::to_shared(&self.state.lock().expect(POISONED).settings)
    }

    fn save(&self, settings: &trust::Settings) {
        let snapshot = {
            let mut state = self.state.lock().expect(POISONED);
            state.settings = shared::from_shared(settings);
            state.clone()
        };
        // Whole document, one writer — see the module note.
        self.writer.save(snapshot);
    }

    /// Empty: this client has no named-profile catalog. Its per-game overrides live on the host
    /// record (`KnownHost::games`), which is a different model from punktfunk's profiles — see
    /// `core::model::SettingsOverride`. Reporting none is honest; inventing ids the rest of this
    /// client cannot resolve would not be.
    fn profiles(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn known_hosts(&self) -> trust::KnownHosts {
        let state = self.state.lock().expect(POISONED);
        trust::KnownHosts {
            hosts: state.known_hosts.iter().map(shared::to_shared_host).collect(),
        }
    }
}

// No tests here on purpose. This module is arm-gated with pf-console-ui, and `task test` builds
// the HOST target — an armv7 test binary cannot execute on a runner, so anything asserted here
// would type-check and never run. The conversions it delegates to live in `shared`, which is not
// gated and is tested there; what is left in this file is a mutex, a clone and a writer call.
