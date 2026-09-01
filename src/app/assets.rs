//! Everything this app ships as bytes: its brand font family, its icon font, the sidebar
//! logo, and the loading spinner (rasterized via [`crate::ui::spinner`]). Card icons are
//! packaged beside the binary and loaded when their tile is built.
//!
//! Deliberately not in `ui`. `ui` is a widget library — it names font *roles*
//! ([`crate::ui::text::FontId`]) and draws the spinner in whatever two colours it is handed;
//! which typeface backs a role, what the brand mark looks like and which colours are the
//! brand's are this app's to decide. `runtime` hands the font bytes to
//! `platform::webos::text_sdl`, which loads them into `SDL2_ttf` without knowing what they are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::Pixmap;

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

fn card_asset_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let installed = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent().map(|p| p.join("assets/cards")));
        installed
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/cards"))
    })
}

fn load_card_icon(dir: &str, name: &str) -> Option<Arc<Pixmap>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Pixmap>>>>> = OnceLock::new();
    if name.len() > 32
        || !name.starts_with(|c: char| c.is_ascii_lowercase())
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return None;
    }
    let key = format!("{dir}/{name}");
    let mut cache = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().ok()?;
    if let Some(icon) = cache.get(&key) {
        return icon.clone();
    }
    let icon = std::fs::read(card_asset_root().join(dir).join(name).with_extension("png"))
        .ok()
        .and_then(|bytes| crate::ui::painter::decode_pixmap(&bytes))
        .map(|mut icon| {
            for pixel in icon.data_mut().chunks_exact_mut(4) {
                let alpha = pixel[3];
                pixel[..3].fill(alpha);
            }
            icon
        })
        .map(Arc::new);
    cache.insert(key, icon.clone());
    icon
}

/// Loads the packaged launcher mark named by an API icon token.
pub fn card_icon(token: &str) -> Option<Arc<Pixmap>> {
    load_card_icon("launchers", token)
}

/// Loads the most specific packaged mark in an advertised OS chain.
pub fn os_icon(chain: &str) -> Option<Arc<Pixmap>> {
    chain.split('/').rev().find_map(|name| {
        let name = match name {
            "macos" => "apple",
            "steamos" => "steam",
            other => other,
        };
        load_card_icon("os", name)
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
