//! The palette every widget draws in — a value the app installs, not a brand the library
//! hard-codes.
//!
//! Installed once at startup ([`install`]) and read-only afterwards, rather than threaded
//! through every call: a theme is process-wide, and the alternative is a sixth argument on
//! every tile builder — the exact parameter-passing that `Canvas` exists to have removed.
//! Reading before an install yields [`Theme::DEFAULT`], so a `ui`-only harness draws
//! without any setup.
//!
//! This app's icon *vocabulary* is not here — which pictogram means "settings" or "forget
//! host" is the app's, and it already passes those in as `&'static str`
//! (`FocusRow::action(icon, …)`); see `app::view::icons`. What is here is [`Icons`]: the
//! four glyphs the library's own chrome draws unasked (a modal's close button, a dropdown's
//! chevron, a row's overflow affordance, the pinned badge), which it must get from
//! somewhere and which no caller is in a position to pass down.
use crate::ui::render::Color;
use std::sync::OnceLock;

/// Every colour the widgets draw with. Copy, so a widget can pull one out and keep drawing
/// through `&mut self`.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Behind everything.
    pub bg: Color,
    /// The nav column, and every modal card — a surface that sits *on* [`Self::bg`].
    pub panel: Color,
    /// A card or row raised above [`Self::panel`].
    pub surface: Color,
    /// Selection, fills, the primary button.
    pub accent: Color,
    /// The focus glow and outline — a lighter [`Self::accent`], since it is drawn as light
    /// rather than as a fill.
    pub accent_bright: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text, unfocused icons.
    pub muted: Color,
    /// A control that exists but cannot be changed here — dimmer than [`Self::muted`], which
    /// is merely "not focused". Reads as inert next to a focused row's white label.
    pub disabled: Color,
    pub warning: Color,
    /// A muted caution caption — dimmer than [`Self::warning`], so it reads as a hint
    /// rather than an alert.
    pub caution: Color,
    pub error: Color,
    /// A positive state (a host being online).
    pub ok: Color,
    /// Dims the screen behind an open modal.
    pub scrim: Color,
    /// Hairline rules inside a card.
    pub rule: Color,
}

impl Theme {
    /// A neutral dark theme, used until an app [`install`]s its own.
    pub const DEFAULT: Self = Self {
        bg: Color::RGB(0x14, 0x14, 0x14),
        panel: Color::RGB(0x1c, 0x1c, 0x1c),
        surface: Color::RGB(0x2b, 0x2b, 0x2b),
        accent: Color::RGB(0x5b, 0x5b, 0xf3),
        accent_bright: Color::RGB(0x9f, 0x9f, 0xf8),
        text: Color::RGB(0xf5, 0xf5, 0xf5),
        muted: Color::RGB(0x94, 0x94, 0x9b),
        disabled: Color::RGB(0x5a, 0x5a, 0x60),
        warning: Color::RGB(0xff, 0xc1, 0x07),
        caution: Color::RGB(0xd1, 0x84, 0x4a),
        error: Color::RGB(0xff, 0x6b, 0x6b),
        ok: Color::RGB(0x5c, 0xd6, 0xa0),
        scrim: Color::RGBA(0x00, 0x00, 0x00, 0x80),
        rule: Color::RGBA(0xff, 0xff, 0xff, 0x1e),
    };
}

/// The glyphs the library's own chrome needs, as codepoints in whatever icon font the app
/// loaded for [`FontId::Icon`](crate::ui::text::FontId). Defaults are Material Icons
/// codepoints, since that is the only icon font shipped today.
#[derive(Clone, Copy, Debug)]
pub struct Icons {
    /// A modal's close button.
    pub close: &'static str,
    /// A dropdown row's open/closed affordance.
    pub chevron_down: &'static str,
    /// A row's "more actions" affordance.
    pub overflow: &'static str,
    /// The pinned badge composited over a pinned card.
    pub pin: &'static str,
}

impl Icons {
    pub const DEFAULT: Self = Self {
        close: "\u{E5CD}",
        chevron_down: "\u{E5C5}",
        overflow: "\u{E5D3}",
        pin: "\u{F10D}",
    };
}

static ACTIVE: OnceLock<(Theme, Icons)> = OnceLock::new();

/// Installs the palette and chrome glyphs for this process. First call wins; later ones are
/// ignored, so a re-entered render loop cannot swap the theme out from under cached tiles.
pub fn install(theme: Theme, icons: Icons) {
    let _ = ACTIVE.set((theme, icons));
}

/// The installed palette, or [`Theme::DEFAULT`] if none was installed.
#[inline]
pub fn theme() -> &'static Theme {
    ACTIVE.get().map_or(&Theme::DEFAULT, |(t, _)| t)
}

/// The installed chrome glyphs, or [`Icons::DEFAULT`].
#[inline]
pub fn icons() -> &'static Icons {
    ACTIVE.get().map_or(&Icons::DEFAULT, |(_, i)| i)
}
