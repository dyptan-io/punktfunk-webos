//! Everything this app ships as bytes: its brand font family, its icon font, the sidebar
//! logo and the loading spinner.
//!
//! Deliberately not in `ui`. `ui` is a widget library — it names font *roles*
//! ([`ui::text::FontId`]) and takes glyphs through [`ui::text_raster::TextRaster`]; which
//! typeface backs a role, and what the brand mark looks like, is this app's to decide.
//! `platform::webos::text_sdl` loads the bytes below into `SDL2_ttf`.
use tiny_skia::{IntSize, Pixmap};

use crate::ui::painter::premultiply_rgba;
use crate::ui::text::FontWeight;

/// Bundled Geist family (punktfunk brand font); embedded so no asset staging is needed.
pub static GEIST_REGULAR: &[u8] = include_bytes!("../assets/fonts/Geist-Regular.otf");
pub static GEIST_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Geist-Medium.otf");
pub static GEIST_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Geist-SemiBold.otf");

/// The typeface backing a `ui` weight.
pub fn geist(weight: FontWeight) -> &'static [u8] {
    match weight {
        FontWeight::Regular => GEIST_REGULAR,
        FontWeight::Medium => GEIST_MEDIUM,
        FontWeight::SemiBold => GEIST_SEMIBOLD,
    }
}

/// Icon font bytes, embedded at compile time (no asset staging or runtime path needed).
pub static ICON_FONT_BYTES: &[u8] = include_bytes!("../assets/icons/MaterialIcons-subset.ttf");

/// Punktfunk logo (rasterized at sidebar size, 1:1 no scaling). See assets/logo/NOTICE.md.
pub static LOGO_PNG: &[u8] = include_bytes!("../assets/logo/logo-sidebar.png");

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

/// The animated loading spinner (purple, from
/// lottiefiles.com/free-animation/purple-spinner-peYjszu1K5, embedded as
/// `assets/logo/punktfunk-spinner.gif`).
static SPINNER_GIF_BYTES: &[u8] = include_bytes!("../assets/logo/punktfunk-spinner.gif");

/// One decoded spinner frame (straight RGBA8) and how long it stays on screen.
pub struct SpinnerFrame {
    pub width: u32,
    pub height: u32,
    pub delay: std::time::Duration,
    pub pixels: Vec<u8>,
}

/// Decodes `SPINNER_GIF_BYTES` once into pre-decoded straight RGBA8 frames.
pub fn spinner_frames() -> &'static [SpinnerFrame] {
    static FRAMES: std::sync::OnceLock<Vec<SpinnerFrame>> = std::sync::OnceLock::new();
    FRAMES.get_or_init(|| {
        use image::{codecs::gif::GifDecoder, AnimationDecoder};
        let Ok(decoder) = GifDecoder::new(std::io::Cursor::new(SPINNER_GIF_BYTES)) else {
            return Vec::new();
        };
        let Ok(raw_frames) = decoder.into_frames().collect::<image::ImageResult<Vec<_>>>() else {
            return Vec::new();
        };
        let mut frames = Vec::with_capacity(raw_frames.len());
        for frame in raw_frames {
            let (w, h) = frame.buffer().dimensions();
            let (numer, denom) = frame.delay().numer_denom_ms();
            let raw_delay = numer.checked_div(denom).unwrap_or(0);
            // WHY: clamp to ~30 FPS min to avoid busy-looping the render thread.
            let delay_ms = if raw_delay < 20 { 33 } else { raw_delay };
            let delay = std::time::Duration::from_millis(u64::from(delay_ms));
            let pixels = frame.into_buffer().into_raw();
            frames.push(SpinnerFrame {
                width: w,
                height: h,
                delay,
                pixels,
            });
        }
        frames
    })
}

/// Returns `SpinnerFrame` at index `idx`, or `None` when the GIF decoded to zero frames.
pub fn spinner_frame(idx: usize) -> Option<&'static SpinnerFrame> {
    spinner_frames().get(idx)
}

/// Returns the frame index and reference for `phase` seconds after the spinner started.
/// Falls back to a 1×1 transparent dummy if the GIF decoded to zero frames.
pub fn spinner_frame_at(phase: f32) -> (usize, &'static SpinnerFrame) {
    let frames = spinner_frames();
    if let Some(first) = frames.first() {
        let total: std::time::Duration = frames.iter().map(|f| f.delay).sum();
        let mut elapsed = std::time::Duration::from_secs_f32(phase.max(0.0)).as_nanos() % total.as_nanos().max(1);
        for (idx, f) in frames.iter().enumerate() {
            if elapsed < f.delay.as_nanos() {
                return (idx, f);
            }
            elapsed -= f.delay.as_nanos();
        }
        (0, first)
    } else {
        static DUMMY: std::sync::OnceLock<SpinnerFrame> = std::sync::OnceLock::new();
        let dummy = DUMMY.get_or_init(|| SpinnerFrame {
            width: 1,
            height: 1,
            delay: std::time::Duration::from_millis(100),
            pixels: vec![0, 0, 0, 0],
        });
        (0, dummy)
    }
}
