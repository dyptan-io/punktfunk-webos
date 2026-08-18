//! LAN discovery via mDNS. Direct mdns-sd dep to avoid pf-client-core's FFmpeg/PipeWire.
use mdns_sd::{ServiceDaemon, ServiceEvent};

/// mDNS service type punktfunk hosts advertise.
pub const SERVICE_TYPE: &str = "_punktfunk._udp.local.";

#[derive(Clone, Debug)]
pub struct DiscoveredHost {
    pub name: String,
    pub addr: String,
    pub port: u16,
    /// Management API port from mDNS (None → `library::DEFAULT_MGMT_PORT`).
    pub mgmt_port: Option<u16>,
    /// Wake-on-LAN MACs from mDNS (learned while awake, persisted to `KnownHost`).
    pub mac: Vec<String>,
}

/// IPv4 address and short instance name from a resolved record. IPv4 only (same as other
/// clients).
fn addr_and_name(info: &mdns_sd::ResolvedService) -> Option<(String, String)> {
    let Some(addr) = info
        .get_addresses_v4()
        .iter()
        .next()
        .map(std::string::ToString::to_string)
    else {
        tracing::warn!("mdns: resolved {} with no IPv4 address, skipping", info.get_fullname());
        return None;
    };
    Some((addr, info.get_fullname().split('.').next().unwrap_or("?").to_string()))
}

/// Turns a resolved record into a host, or `None` if it isn't usable (no IPv4).
fn parse_discovery(info: &mdns_sd::ResolvedService) -> Option<DiscoveredHost> {
    let (addr, name) = addr_and_name(info)?;
    let props = info.get_properties();
    Some(DiscoveredHost {
        name,
        addr,
        port: info.get_port(),
        mgmt_port: props.get_property_val_str("mgmt").and_then(|v| v.parse().ok()),
        mac: props
            .get_property_val_str("mac")
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    })
}

/// mdns-sd doubles its PTR re-query interval every round (1s, 2s ... capped at an hour) and
/// discards a whole incoming message on any parse error, so answers lost inside one malformed
/// packet can leave the list empty for minutes. Restarting resets that backoff, but also its
/// traffic-quieting — hence this long, not shorter.
const REBROWSE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120);

/// A running browse for punktfunk hosts, drained from the menu tick. No thread of its own —
/// mdns-sd's daemon already runs one. That daemon re-queries on its own timers, so what keeps
/// discovery off the network during a stream is `App` dropping this (and with it the daemon)
/// when the menu loop exits, not the tick.
pub struct Discovery {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    last_browse: std::time::Instant,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

impl Discovery {
    /// `None` if the daemon or initial browse won't start — discovery is then simply absent.
    pub fn start() -> Option<Self> {
        let daemon = ServiceDaemon::new()
            .inspect_err(|e| tracing::error!("mdns: ServiceDaemon::new failed: {e}"))
            .ok()?;
        let events = match daemon.browse(SERVICE_TYPE) {
            Ok(events) => events,
            Err(e) => {
                // The daemon's thread outlives its handle, so a failed start still has to stop it.
                tracing::error!("mdns: browse({SERVICE_TYPE}) failed: {e}");
                let _ = daemon.shutdown();
                return None;
            }
        };
        tracing::debug!("mdns: browsing {SERVICE_TYPE}");
        Some(Self {
            daemon,
            events,
            last_browse: std::time::Instant::now(),
        })
    }

    /// Hosts resolved since the last call, restarting the browse when due. Empty on almost
    /// every tick, which costs no allocation.
    pub fn poll(&mut self) -> Vec<DiscoveredHost> {
        let mut found = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            let ServiceEvent::ServiceResolved(info) = event else {
                tracing::debug!("mdns: {event:?}");
                continue;
            };
            let Some(host) = parse_discovery(&info) else {
                continue;
            };
            tracing::info!("mdns: resolved {} at {}:{}", host.name, host.addr, host.port);
            found.push(host);
        }
        // After the drain, not before: re-browsing swaps the receiver out and whatever it still
        // held goes with it.
        if self.last_browse.elapsed() >= REBROWSE_INTERVAL {
            self.rebrowse();
        }
        found
    }

    fn rebrowse(&mut self) {
        // Without the stop, the old retransmission chain keeps running and they stack one per
        // interval. It also drops the cache, which is what makes hosts resolve from scratch.
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        match self.daemon.browse(SERVICE_TYPE) {
            Ok(events) => self.events = events,
            // The stop already removed the querier, so nothing more arrives until the
            // next interval retries this.
            Err(e) => tracing::error!("mdns: re-browse({SERVICE_TYPE}) failed: {e}"),
        }
        self.last_browse = std::time::Instant::now();
    }
}
