//! The wired `DualSense`'s HID output path: `/dev/hidraw*`.
//!
//! **Why this exists next to the Luna route.** Everything in [`super::dualsense`] reaches the pad
//! through `com.webos.service.bluetooth2`, which a USB pad is not on — it has no Bluetooth link to
//! address, even though `hid-playstation` still publishes its MAC as `U: Uniq=` (see
//! `dualsense::address_in`). The kernel's hidraw node is what reaches a wired pad, and it needs no
//! service, no CRC and no sniff: one 48-byte `0x02` report per write.
//!
//! **The node exists, contrary to what this client used to record.** An earlier probe concluded
//! the jail had no hidraw at all; it ran with no pad plugged in, and `hid-playstation` only creates
//! a node for a device that is actually there. With a pad attached the jail exposes
//! `/dev/hidraw0`, `root:jailer` and read-write — the same group the app runs in (verified on a
//! G5, webOS 10.3, non-rooted). There is no `/sys/class/hidraw` in the jail, so the node is found
//! by asking each candidate what it is rather than by walking sysfs.

use std::ffi::CString;

use anyhow::{bail, Result};

/// Sony's vendor id, and the two `DualSense` product ids (original, then Edge).
const VENDOR_SONY: u16 = 0x054c;
const PRODUCTS: [u16; 2] = [0x0ce6, 0x0df2];
/// `BUS_USB` as hidraw reports it. A Bluetooth pad also has a hidraw node, but its reports carry
/// the `0x31` framing and CRC of the Luna route, so this transport deliberately claims USB only.
const BUS_USB: u32 = 0x03;

/// How many `/dev/hidraw*` nodes to ask about. The jail has published exactly one so far; a
/// handful covers a set with other HID devices attached without walking a sysfs that isn't there.
const MAX_NODES: u8 = 10;

/// `HIDIOCGRAWINFO`: `_IOR('H', 0x03, struct hidraw_devinfo)`, which is `{u32 bus, s16 vendor,
/// s16 product}` — 8 bytes.
const HIDIOCGRAWINFO: libc::c_ulong = (2 << 30) | (8 << 16) | ((b'H' as libc::c_ulong) << 8) | 0x03;

/// What `HIDIOCGRAWINFO` fills in.
#[repr(C)]
#[derive(Default)]
struct DevInfo {
    bustype: u32,
    vendor: i16,
    product: i16,
}

/// An open `/dev/hidraw*` for a wired `DualSense`.
pub struct Hidraw {
    fd: libc::c_int,
    /// Kept for the log line that says which node was claimed; nodes are not stable across replug.
    pub path: String,
}

impl Hidraw {
    /// The first wired `DualSense` the jail exposes, or `None`.
    ///
    /// Opened read-write: a node that only opens read-only is no use here, and taking it anyway
    /// would report success for a transport that cannot carry a single effect.
    pub fn find_dualsense() -> Option<Self> {
        (0..MAX_NODES).find_map(|n| {
            let path = format!("/dev/hidraw{n}");
            let c_path = CString::new(path.as_str()).ok()?;
            // SAFETY: `c_path` is NUL-terminated and outlives the call.
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
            if fd < 0 {
                return None;
            }
            let node = Self { fd, path };
            node.is_wired_dualsense().then_some(node)
        })
    }

    /// Whether this node is a `DualSense` on USB, per the driver's own answer.
    fn is_wired_dualsense(&self) -> bool {
        let mut info = DevInfo::default();
        // SAFETY: `info` is a live local of exactly the size the ioctl writes.
        let rc = unsafe { libc::ioctl(self.fd, HIDIOCGRAWINFO, &raw mut info) };
        rc >= 0
            && info.bustype == BUS_USB
            && info.vendor as u16 == VENDOR_SONY
            && PRODUCTS.contains(&(info.product as u16))
    }

    /// Writes one output report. `report[0]` is the report id.
    ///
    /// A short write is a failure, not a partial send: the pad reads a report whole, and half of
    /// one is a malformed effect rather than a slower one.
    pub fn write_report(&self, report: &[u8]) -> Result<()> {
        // SAFETY: `report` is a live slice; the fd is owned by `self` and open until `Drop`.
        let n = unsafe { libc::write(self.fd, report.as_ptr().cast(), report.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            bail!("write to {}: {err}", self.path);
        }
        if n as usize != report.len() {
            bail!("short write to {}: {n} of {} bytes", self.path, report.len());
        }
        Ok(())
    }
}

impl Drop for Hidraw {
    fn drop(&mut self) {
        // SAFETY: `fd` came from `open` in `find_dualsense` and is closed exactly once.
        unsafe { libc::close(self.fd) };
    }
}
