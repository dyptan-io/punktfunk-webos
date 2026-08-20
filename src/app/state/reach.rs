//! Ambient reachability polling for sidebar host rows. Pure logic — no view counterpart.
use crate::app::App;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How often the whole host list is re-probed. Deliberately slow: this is ambient status, nobody
/// waits on it, and every round is a connection the host logs. The cost of a longer interval is
/// only that the dot lags a host coming up or going down by up to this long.
const REACH_INTERVAL: Duration = Duration::from_secs(180);

/// One host's probe result.
pub(crate) struct Reachability {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) online: bool,
}

impl App {
    /// Kick off reachability sweep if one is due and none is in flight.
    pub(crate) fn tick_reachability(&mut self) {
        if self.jobs.reach.is_some() {
            return; // a sweep is still running
        }
        if self.hosts.reach_last.is_some_and(|t| t.elapsed() < REACH_INTERVAL) {
            return;
        }
        self.hosts.reach_last = Some(Instant::now());
        let targets: Vec<(String, u16)> = self.hosts.entries.iter().map(|e| (e.host().to_string(), e.port())).collect();
        if targets.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs.reach = Some(rx);
        // One thread for the whole sweep, probing sequentially: the host count here is a
        // handful, and a thread per host would spike this SoC's 3 cores for a cosmetic
        // indicator. Each send failing (the receiver replaced by a newer sweep, or the app
        // gone) just ends the sweep early.
        std::thread::spawn(move || {
            for (host, port) in targets {
                let online = punktfunk_core::client::NativeClient::probe(&host, port, crate::services::budget::PROBE);
                if tx.send(Reachability { host, port, online }).is_err() {
                    return;
                }
            }
        });
    }

    /// Drain finished probes. Returns true if sidebar changed.
    pub(crate) fn drain_reachability(&mut self) -> bool {
        let Some(rx) = &self.jobs.reach else { return false };
        let mut changed = false;
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(r) => {
                    let key = (r.host, r.port);
                    if self.hosts.reachable.get(&key) != Some(&r.online) {
                        self.hosts.reachable.insert(key, r.online);
                        changed = true;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.jobs.reach = None;
        }
        if changed {
            self.render.sidebar_dirty = true;
        }
        changed
    }

    /// Last known reachability (None until first probe).
    pub(crate) fn entry_online(&self, entry: &crate::app::hosts::HostEntry) -> Option<bool> {
        self.hosts.reachable.get(&(entry.host().to_string(), entry.port())).copied()
    }

    /// All reachability states, index-aligned with entries.
    pub(crate) fn reachability_list(&self) -> Vec<Option<bool>> {
        self.hosts.entries.iter().map(|e| self.entry_online(e)).collect()
    }

    /// Initialize empty reachability map.
    pub(crate) fn new_reachability() -> HashMap<(String, u16), bool> {
        HashMap::new()
    }
}
