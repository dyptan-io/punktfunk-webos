//! Crash-safe file writes.
//!
//! Write-then-rename, never truncate-in-place: `std::fs::write` truncates first, so a
//! kill/power-cut mid-write (this is a TV — losing power IS the off switch) leaves a half-file,
//! and the loaders' `unwrap_or_default()` would then silently discard every paired host / all
//! settings. A rename on the same filesystem is atomic; readers see the old file or the new one,
//! never a torn one.
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn write(path: &Path, contents: &str, what: &str) -> Result<()> {
    write_parts(path, &[contents.as_bytes()], what)
}

/// Same discipline for byte payloads that arrive in pieces (a header plus a pixel buffer, say):
/// the parts are written in order, so nothing has to be concatenated into one allocation first.
///
/// `.tmp` is appended to the whole filename rather than replacing an extension, which would make
/// `id.raw` and `id` — two files the art cache keeps side by side — stage to the same path.
pub fn write_parts(path: &Path, parts: &[&[u8]], what: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = File::create(&tmp).with_context(|| format!("create {what} (tmp)"))?;
    for part in parts {
        file.write_all(part).with_context(|| format!("write {what} (tmp)"))?;
    }
    drop(file);
    std::fs::rename(&tmp, path).with_context(|| format!("rename {what} into place"))
}
