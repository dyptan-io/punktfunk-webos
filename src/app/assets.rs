//! Everything this app ships as bytes: its brand font family, its icon font, the sidebar
//! logo, and the loading spinner (rasterized via [`crate::ui::spinner`]). Card marks are the
//! one asset kept on disk (see [`load_card_icon`]) rather than embedded.
//!
//! Deliberately not in `ui`. `ui` is a widget library — it names font *roles*
//! ([`crate::ui::text::FontId`]) and draws the spinner in whatever two colours it is handed;
//! which typeface backs a role, what the brand mark looks like and which colours are the
//! brand's are this app's to decide. `runtime` hands the font bytes to
//! `platform::webos::text_sdl`, which loads them into `SDL2_ttf` without knowing what they are.

use crate::ui::text::FontWeight;

/// Bundled Geist family (punktfunk brand font); embedded so no asset staging is needed.
pub static GEIST_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Geist-Regular.otf");
pub static GEIST_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Geist-Medium.otf");

/// The typeface backing a `ui` weight.
pub fn geist(weight: FontWeight) -> &'static [u8] {
    match weight {
        FontWeight::Regular => GEIST_REGULAR,
        FontWeight::Medium => GEIST_MEDIUM,
    }
}

/// Icon font bytes, embedded at compile time (no asset staging or runtime path needed).
pub static ICON_FONT_BYTES: &[u8] = include_bytes!("../../assets/icons/MaterialIcons-subset.ttf");

/// The [`card_icon`] token for the most specific packaged mark in an advertised OS chain —
/// `linux/fedora/bazzite` prefers `bazzite` and falls back toward `linux`. Resolved when the
/// host's OS becomes known (see `Library::set_desktop_icon`), not per card build, so this
/// only stats for the file rather than decoding it.
pub fn os_icon_token(chain: &str) -> Option<String> {
    let probe = skia_safe::Rect::from_wh(16.0, 16.0);
    pf_console_ui::os_marks::os_mark(chain, probe)
        .is_some()
        .then(|| format!("os/{chain}"))
}
