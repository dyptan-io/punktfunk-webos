//! The write destination — a rotating log file, or a TCP stream to a dev machine.
use crate::core::VERSION;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::launch;

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// Rotations kept (`base.log.1`..`.3`), bounding disk use at
/// ~`(MAX_LOG_ROTATIONS + 1) * MAX_LOG_BYTES`.
const MAX_LOG_ROTATIONS: usize = 3;

/// Log destination (file or TCP). Non-blocking dispatch prevents blocking video pump.
pub(super) enum Sink {
    File {
        file: std::fs::File,
        written: u64,
        /// Active log path, so a full file can be rotated (renamed) and reopened.
        path: PathBuf,
    },
    Tcp(TcpStream),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File { file, written, .. } => {
                let n = file.write(buf)?;
                *written += n as u64;
                Ok(n)
            }
            Self::Tcp(s) => s.write(buf),
        }
    }

    /// `tracing_appender`'s worker thread flushes after each drained batch, not per
    /// line — so the size/rotation check runs once per batch instead of per write.
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File { file, written, path } => {
                file.flush()?;
                if *written >= MAX_LOG_BYTES {
                    rotate(path);
                    *file = open_fresh(path)?;
                    *written = 0;
                }
                Ok(())
            }
            Self::Tcp(s) => s.flush(),
        }
    }
}

/// Open TCP or file sink; fall back to file if unreachable (dev convenience, not critical).
pub(super) fn open(app_dir: &Path) -> Result<Sink> {
    if let Some(addr) = launch::telemetry_addr() {
        if let Ok(stream) = TcpStream::connect(addr) {
            return Ok(Sink::Tcp(stream));
        }
    }
    open_file(app_dir)
}

/// A fresh active log each launch; the previous session rotates to `.1` first, so
/// relaunching to reproduce a bug keeps the prior run.
fn open_file(app_dir: &Path) -> Result<Sink> {
    let path = log_file_path(app_dir);
    if path.metadata().is_ok_and(|m| m.len() > 0) {
        rotate(&path);
    }
    let file = open_fresh(&path).with_context(|| format!("open log file {}", path.display()))?;
    Ok(Sink::File { file, written: 0, path })
}

/// Create (truncating) a fresh active log at `path`.
fn open_fresh(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// `base.log` → `base.log.<n>`.
fn numbered(base: &Path, n: usize) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Shift the ring down one: drop `.MAX`, rename `.k`→`.k+1`, then `base`→`.1`.
/// Best-effort — a failed rename loses one rotation, never the active log.
fn rotate(base: &Path) {
    let _ = std::fs::remove_file(numbered(base, MAX_LOG_ROTATIONS));
    for n in (1..MAX_LOG_ROTATIONS).rev() {
        let _ = std::fs::rename(numbered(base, n), numbered(base, n + 1));
    }
    let _ = std::fs::rename(base, numbered(base, 1));
}

/// Absolute path of the active log file (`open_file`'s target).
fn log_file_path(app_dir: &Path) -> PathBuf {
    app_dir.join(format!("punktfunk-webos-{VERSION}.log"))
}

/// The log file to hand to a user-facing report — the one currently being written
/// when there is one, else the newest across all rotations *and* app versions (a
/// version bump changes `VERSION` in `log_file_path`, so matching only the current
/// name would orphan older logs). The active file is preferred outright rather than
/// by mtime: `rotate` renames carry the source mtime, so a just-rotated `.1` can look
/// newer than the active file the process is still appending to.
/// `None` if nothing has been logged yet.
pub fn latest_log_file(app_dir: &Path) -> Option<PathBuf> {
    let active = log_file_path(app_dir);
    if active.metadata().is_ok_and(|m| m.len() > 0) {
        return Some(active);
    }
    std::fs::read_dir(app_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("punktfunk-webos-") && name.contains(".log"))
        })
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            (meta.len() > 0).then_some(())?;
            Some((entry.path(), meta.modified().ok()?))
        })
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path)
}
