//! Where this app keeps its files.
//!
//! Its own module, rather than a private helper in `store`, so a probe binary can pull in one
//! leaf module instead of the whole persistence layer.
use std::path::{Path, PathBuf};

/// The app's data directory: `$HOME` under webOS's per-app jail, `/tmp` if it is unset (a
/// bare SSH shell), which keeps a dev run working without writing somewhere unexpected.
pub fn app_dir() -> PathBuf {
    std::env::var("HOME").map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}

/// Where the packaged read-only assets live: `assets/` beside the installed binary's `bin/`
/// (see `taskfiles/toolchain.yml`'s staging), falling back to the source tree so a dev run
/// off a plain `cargo build` still finds them.
pub fn assets_dir() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent()?.parent().map(|root| root.join("assets")))
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"))
    })
    .as_path()
}

/// The one charset every packaged asset name and every mDNS-advertised OS token is held to.
pub fn is_asset_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Whether `name` may be joined onto [`assets_dir`]. Bounded, in [`is_asset_char`]'s charset,
/// and starting alphanumeric — which is what rules out `..` and dotfiles, so an untrusted
/// token (`GameEntry::icon` comes straight from the host's listing) can never walk the path.
pub fn is_asset_token(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.chars().all(is_asset_char)
}
