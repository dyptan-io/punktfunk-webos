//! Everything this app ships as bytes: its brand font family, its icon font, the sidebar
//! logo, and the loading spinner (rasterized via [`crate::ui::spinner`]).
//!
//! Deliberately not in `ui`. `ui` is a widget library — it names font *roles*
//! ([`crate::ui::text::FontId`]) and draws the spinner in whatever two colours it is handed;
//! which typeface backs a role, what the brand mark looks like and which colours are the
//! brand's are this app's to decide. `runtime` hands the font bytes to
//! `platform::webos::text_sdl`, which loads them into `SDL2_ttf` without knowing what they are.

use tiny_skia::{IntSize, Pixmap};

use crate::ui::painter::premultiply_rgba;
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

/// Decode embedded logo once, lazily (premultiplied, ready to composite). None if PNG invalid.
pub fn logo_pixmap() -> Option<&'static Pixmap> {
    static LOGO: std::sync::OnceLock<Option<Pixmap>> = std::sync::OnceLock::new();
    LOGO.get_or_init(|| {
        let decoded = image::load_from_memory(LOGO_PNG).ok()?;
        let rgba = decoded.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        let mut buf = rgba.into_raw();
        premultiply_rgba(&mut buf);
        Pixmap::from_vec(buf, IntSize::from_wh(w, h)?)
    })
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
