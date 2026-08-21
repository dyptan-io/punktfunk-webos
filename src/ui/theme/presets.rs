//! The looks on offer, one `static` each.
//!
//! The palette here is punktfunk's own, and it lives beside the neutral defaults rather than
//! being injected at startup: a look is a constant of the binary, and an `install` hook only
//! bought the ability to swap the brand out at runtime, which nothing wants and which cached
//! tiles cannot survive.
use super::{Glass, Icons, Palette, Theme};
use crate::core::model::ThemeChoice;
use crate::ui::render::Color;

/// The one palette both looks draw in. They differ in *material*, not in colour — a look that
/// changed both at once would make it impossible to tell which half you were reacting to.
const PALETTE: Palette = Palette {
    bg: Color::RGB(0x14, 0x10, 0x1f),
    panel: Color::RGB(0x1c, 0x15, 0x30),
    surface: Color::RGB(0x2b, 0x21, 0x48),
    accent: Color::RGB(0x6c, 0x5b, 0xf3),
    accent_bright: Color::RGB(0xa7, 0x9f, 0xf8),
    text: Color::RGB(0xf5, 0xf5, 0xf5),
    muted: Color::RGB(0x9b, 0x94, 0xb8),
    disabled: Color::RGB(0x5c, 0x57, 0x72),
    warning: Color::RGB(0xff, 0xc1, 0x07),
    // Desaturated rather than a signal colour — it sits next to brand purple on every row.
    caution: Color::RGB(0xd1, 0x84, 0x4a),
    error: Color::RGB(0xff, 0x6b, 0x6b),
    ok: Color::RGB(0x5c, 0xd6, 0xa0),
    scrim: Color::RGBA(0x00, 0x00, 0x00, 0x80),
    rule: Color::RGBA(0xff, 0xff, 0xff, 0x1e),
    glass_edge: Color::RGBA(0xff, 0xff, 0xff, 0x18),
};

/// Material Icons codepoints, from the bundled subset (`crate::assets`).
const ICONS: Icons = Icons {
    close: "\u{E5CD}",
    chevron_down: "\u{E5C5}",
    overflow: "\u{E5D3}",
    pin: "\u{F10D}",
};

/// Flat opaque panels. Costs no GPU memory and no render-target binds at all.
pub const DEFAULT: Theme = Theme {
    name: "Default",
    choice: ThemeChoice::Default,
    palette: PALETTE,
    icons: ICONS,
    glass: None,
};

/// Frosted glass: translucent panels over a blurred, grained copy of what they cover.
///
/// Wants render targets and a composed blend mode, and which webOS generations give it both
/// is not something a spec answers — the compositor probes, logs `frosted modals: <bool>` and
/// falls back to flat fills on its own, so this is safe to offer everywhere.
pub const GLOSSY: Theme = Theme {
    name: "Default Glossy",
    choice: ThemeChoice::DefaultGlossy,
    palette: PALETTE,
    icons: ICONS,
    glass: Some(Glass {
        // The panel, made translucent. Derived rather than restated: two hand-written
        // near-identical purples is one edit away from a glass card that does not match the
        // opaque one it replaces.
        panel: PALETTE.panel.with_alpha(0xda),
        // As wide as anything here reads: at this spread the backdrop is a wash rather than a
        // recognisable image of what is behind it, which is the point. The compositor rounds
        // it down to whatever its chain can actually give.
        blur: 64,
        grain: 0x2e,
    }),
};
