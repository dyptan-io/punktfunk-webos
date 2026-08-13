//! The sidebar's host-list model: one entry per known or discovered host.
use crate::services::discovery::DiscoveredHost;
use crate::services::store::KnownHost;

/// One entry in the sidebar's host list — either a fully known/paired host or a
/// freshly discovered (not yet paired) one.
///
/// Not in `core`: it wraps a `services` type from each half of the list, and `core` is a
/// dependency leaf.
#[derive(Clone)]
pub enum HostEntry {
    Known(KnownHost),
    Discovered(DiscoveredHost),
}

impl HostEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Known(h) => &h.name,
            Self::Discovered(h) => &h.name,
        }
    }
    pub fn host(&self) -> &str {
        match self {
            Self::Known(h) => &h.host,
            Self::Discovered(h) => &h.addr,
        }
    }
    pub fn port(&self) -> u16 {
        match self {
            Self::Known(h) => h.port,
            Self::Discovered(h) => h.port,
        }
    }
    pub fn is_paired(&self) -> bool {
        matches!(self, Self::Known(h) if h.is_paired())
    }
    pub fn mgmt_port(&self) -> Option<u16> {
        match self {
            Self::Known(h) => h.mgmt_port,
            Self::Discovered(h) => h.mgmt_port,
        }
    }
    /// Wake-on-LAN MAC(s) known for this entry so far — empty until it's been seen
    /// advertising its `mac` mDNS TXT at least once (see `discovery::DiscoveredHost::mac`).
    pub fn mac(&self) -> &[String] {
        match self {
            Self::Known(h) => &h.mac,
            Self::Discovered(h) => &h.mac,
        }
    }
}
