//! Scheduling-priority boosts for the threads that carry the stream.
//!
//! Two sets, both best-effort: the threads this crate and punktfunk-core spawn
//! ([`boost_hot_threads`]), and the vendor decode pipeline's own `GStreamer` pad tasks
//! ([`spawn_vendor_decode_thread_renicer`]). Every renice needs `CAP_SYS_NICE` or a nonzero
//! `RLIMIT_NICE` — present on a rooted install, absent under a plain Dev-Mode SAM jail (see
//! `device::renice`) — so each path logs one summary line saying whether it applied at all.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use punktfunk_core::client::NativeClient;

use crate::platform::webos::device::{renice, HOT_THREAD_NICE};

/// Boosts every thread punktfunk-core registered as hot, and logs whether it worked.
///
/// Summarized at info, not left as per-tid debug lines: a session log that doesn't answer "did the
/// priority boost actually apply here" hides the difference between the two contention regimes
/// `docs/NOTES.md`'s renice findings were measured under.
pub(super) fn boost_hot_threads(client: &NativeClient) {
    let (mut reniced, mut failed) = (0u32, 0u32);
    for tid in client.hot_thread_ids() {
        if renice(tid) {
            reniced += 1;
        } else {
            failed += 1;
            tracing::debug!("setpriority(tid={tid}) failed: {}", std::io::Error::last_os_error());
        }
    }
    tracing::info!(
        "hot-thread renice: {reniced} boosted, {failed} failed{}",
        if failed > 0 {
            " (no CAP_SYS_NICE — priorities unchanged)"
        } else {
            ""
        },
    );
}

/// Suffix identifying a `GStreamer` pad-task thread (`"<element-name>:<pad-name>"`,
/// truncated to the kernel's 15-char `comm` limit) — the NDL vendor `.so` builds its
/// internal decode pipeline out of `GStreamer` elements, each with its own pad-task
/// thread spawned *inside our own process*. These are invisible to punktfunk-core's
/// hot-thread registry (that only covers threads this crate and punktfunk-core spawn
/// themselves) and sit at the default nice 0 despite doing real decode work — confirmed
/// via live `/proc/<pid>/task` sampling during an active NDL stream (its
/// `lxvideodec1:src`/`video-src:src` threads), a real contention cost against our own
/// already-boosted video-pump/data-pump threads on this `SoC`'s 3 cores. Matched by
/// suffix, not a fixed name list, so it covers whichever elements the pipeline uses.
const VENDOR_DECODE_THREAD_SUFFIX: &str = ":src";
/// How long a decode-thread scan may run with no new match before concluding the
/// backend's pipeline has finished spawning threads (typically well under this in
/// practice). Bounded separately by `VENDOR_DECODE_THREAD_SCAN_TIMEOUT` in case a
/// backend never produces a matching thread at all.
const VENDOR_DECODE_THREAD_QUIET_PERIOD: Duration = Duration::from_millis(500);
const VENDOR_DECODE_THREAD_SCAN_TIMEOUT: Duration = Duration::from_secs(5);
/// Gap between scans. The threads appear within the first second or so of a load, so this is
/// fine enough to catch them promptly and coarse enough to cost nothing while it waits.
const VENDOR_DECODE_THREAD_SCAN_POLL: Duration = Duration::from_millis(100);

/// One pass over `/proc/self/task`: renices every vendor decode thread not yet in `seen`,
/// which it extends. Returns (`newly found`, `of those, refused by the kernel`).
fn renice_vendor_threads(seen: &mut HashSet<i32>) -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return (0, 0);
    };
    let (mut found, mut failed) = (0, 0);
    for entry in entries.flatten() {
        let Ok(tid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if seen.contains(&tid) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim();
        if !comm.ends_with(VENDOR_DECODE_THREAD_SUFFIX) {
            continue;
        }
        seen.insert(tid);
        found += 1;
        if renice(tid) {
            tracing::debug!("reniced vendor decode thread {comm} (tid={tid}) to {HOT_THREAD_NICE}");
        } else {
            failed += 1;
            tracing::warn!(
                "setpriority(vendor thread {comm}, tid={tid}) failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    (found, failed)
}

/// Renices the active backend's vendor-spawned `GStreamer` pad-task threads, same as this
/// crate's own hot threads (see [`VENDOR_DECODE_THREAD_SUFFIX`]). Runs on its own thread —
/// these threads spawn asynchronously sometime after the decoder loads, not synchronously
/// within the load call, so this polls `/proc/self/task` rather than scanning once, and must
/// not block the video pump from starting to feed frames while it does.
pub(super) fn spawn_vendor_decode_thread_renicer() {
    std::thread::spawn(|| {
        let start = Instant::now();
        let mut last_found = start;
        let mut failed = 0;
        let mut seen = HashSet::new();
        loop {
            let (found, refused) = renice_vendor_threads(&mut seen);
            failed += refused;
            if found > 0 {
                last_found = Instant::now();
            }
            let quiet = !seen.is_empty() && last_found.elapsed() >= VENDOR_DECODE_THREAD_QUIET_PERIOD;
            if quiet || start.elapsed() >= VENDOR_DECODE_THREAD_SCAN_TIMEOUT {
                break;
            }
            std::thread::sleep(VENDOR_DECODE_THREAD_SCAN_POLL);
        }
        // One summarizing line for the same reason as the hot-thread summary above: whether the
        // boost applied at all is the install-mode question a session log has to answer.
        tracing::info!(
            "vendor decode threads: {} found, {} boosted",
            seen.len(),
            seen.len().saturating_sub(failed),
        );
    });
}
