//! Everything this app ships as bytes: its brand font family, its icon font, the sidebar
//! logo, and the loading spinner (rasterized via [`crate::ui::spinner`]). Card marks are the
//! one asset kept on disk (see [`load_card_icon`]) rather than embedded.
//!
//! Deliberately not in `ui`. `ui` is a widget library — it names font *roles*
//! ([`crate::ui::text::FontId`]) and draws the spinner in whatever two colours it is handed;
//! which typeface backs a role, what the brand mark looks like and which colours are the
//! brand's are this app's to decide. `runtime` hands the font bytes to
//! `platform::webos::text_sdl`, which loads them into `SDL2_ttf` without knowing what they are.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::Pixmap;

use crate::services::paths;
use crate::ui::spinner;
use crate::ui::text::FontWeight;
use crate::ui::Painter;

/// Bundled Geist family (punktfunk brand font); embedded so no asset staging is needed.
pub static GEIST_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Geist-Regular.otf");
pub static GEIST_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Geist-Medium.otf");
pub static GEIST_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Geist-SemiBold.otf");

/// The typeface backing a `ui` weight.
pub fn geist(weight: FontWeight) -> &'static [u8] {
    match weight {
        FontWeight::Regular => GEIST_REGULAR,
        FontWeight::Medium => GEIST_MEDIUM,
        FontWeight::SemiBold => GEIST_SEMIBOLD,
    }
}

/// Icon font bytes, embedded at compile time (no asset staging or runtime path needed).
pub static ICON_FONT_BYTES: &[u8] = include_bytes!("../../assets/icons/MaterialIcons-subset.ttf");

/// Punktfunk logo (rasterized at sidebar size, 1:1 no scaling). See assets/logo/NOTICE.md.
pub static LOGO_PNG: &[u8] = include_bytes!("../../assets/logo/logo-sidebar.png");

/// The file a packaged mark lives in, or `None` if `name` may not be joined onto a path.
/// Lowercases first since file names are lowercase but `GameEntry::icon` is host-casing
/// (`Steam`); a mismatch cached as `None` is permanent, so validate before any miss.
fn card_icon_path<'a>(dir: &str, name: &'a str) -> Option<(PathBuf, Cow<'a, str>)> {
    let name = if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    };
    if !paths::is_asset_token(&name) {
        return None;
    }
    let path = paths::assets_dir()
        .join("cards")
        .join(dir)
        .join(name.as_ref())
        .with_extension("png");
    Some((path, name))
}

/// Kept on disk rather than embedded: a host contributes one OS mark and a handful of launcher
/// marks, so paying flash for the whole set buys nothing. Cached per `(name, side)`: read,
/// decode and resample once per size. Marks are white silhouettes — source PNGs are
/// black-on-transparent, so the fill lifts RGB to the alpha each pixel already has.
fn load_card_icon(dir: &'static str, name: &str, side: u32) -> Option<Arc<Pixmap>> {
    // Nested rather than keyed by a `(String, u32)` tuple so the hit path probes with a
    // borrowed `&str` and allocates nothing.
    type Cache = HashMap<&'static str, HashMap<String, HashMap<u32, Option<Arc<Pixmap>>>>>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let (path, name) = card_icon_path(dir, name)?;
    let mut cache = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().ok()?;
    let by_name = cache.entry(dir).or_default();
    if let Some(icon) = by_name.get(name.as_ref()).and_then(|by_size| by_size.get(&side)) {
        return icon.clone();
    }
    let icon = std::fs::read(path)
        .ok()
        .and_then(|bytes| crate::ui::painter::decode_pixmap(&bytes))
        .map(|mut icon| {
            for pixel in icon.data_mut().chunks_exact_mut(4) {
                let alpha = pixel[3];
                pixel[..3].fill(alpha);
            }
            fit_to(icon, side)
        })
        .map(Arc::new);
    by_name.entry(name.into_owned()).or_default().insert(side, icon.clone());
    icon
}

/// Scales `icon` to fit a `side`-by-`side` box, keeping its aspect. Done once at load rather
/// than per card raster: the grid asks for one size at a time.
fn fit_to(icon: Pixmap, side: u32) -> Pixmap {
    let scale = (side as f32 / icon.width() as f32).min(side as f32 / icon.height() as f32);
    let w = ((icon.width() as f32 * scale).round() as u32).max(1);
    let h = ((icon.height() as f32 * scale).round() as u32).max(1);
    crate::services::art::resize_pixmap(icon, w, h)
}

/// The packaged mark a `GameEntry::icon` token names, at `side` pixels. A bare token is a
/// launcher mark from the host's listing (`steam`); an `os/`-qualified one is the client's own
/// pick for the Desktop card (see [`os_icon_token`]). Any other prefix is refused rather than
/// joined onto a path.
pub fn card_icon(token: &str, side: u32) -> Option<Arc<Pixmap>> {
    let (dir, name) = match token.split_once('/') {
        Some(("os", name)) => ("os", name),
        Some(_) => return None,
        None => ("launchers", token),
    };
    load_card_icon(dir, name, side)
}

/// The [`card_icon`] token for the most specific packaged mark in an advertised OS chain —
/// `linux/fedora/bazzite` prefers `bazzite` and falls back toward `linux`. Resolved when the
/// host's OS becomes known (see `Library::set_desktop_icon`), not per card build, so this
/// only stats for the file rather than decoding it.
pub fn os_icon_token(chain: &str) -> Option<String> {
    chain.split('/').rev().find_map(|name| {
        // The two OS names whose mark is filed under a different one.
        let name = match name {
            "macos" => "apple",
            "steamos" => "steam",
            other => other,
        };
        let (path, name) = card_icon_path("os", name)?;
        path.is_file().then(|| format!("os/{name}"))
    })
}

/// Decode embedded logo once, lazily (premultiplied, ready to composite). None if PNG invalid.
pub fn logo_pixmap() -> Option<&'static Pixmap> {
    static LOGO: std::sync::OnceLock<Option<Pixmap>> = std::sync::OnceLock::new();
    LOGO.get_or_init(|| crate::ui::painter::decode_pixmap(LOGO_PNG))
        .as_ref()
}

/// Rasterizes the whole cycle once, in the brand's two accents. All frames up front so the
/// render thread never stalls on a `tiny_skia` fill; each is ready to upload as a tile texture.
pub fn spinner_frames() -> &'static [Painter] {
    static CACHE: std::sync::OnceLock<Vec<Painter>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let theme = crate::ui::theme::palette();
        (0..spinner::FRAMES)
            .map(|i| spinner::frame(i as f32 / spinner::FRAMES as f32, theme.accent_bright, theme.accent))
            .collect()
    })
}

/// Returns the frame index and reference for `phase` seconds after the spinner started.
pub fn spinner_frame_at(phase: f32) -> (usize, &'static Painter) {
    let cycle = spinner::CYCLE.as_secs_f32();
    let t = (phase.max(0.0) % cycle) / cycle;
    let idx = ((t * spinner::FRAMES as f32) as usize).min(spinner::FRAMES - 1);
    (idx, &spinner_frames()[idx])
}
