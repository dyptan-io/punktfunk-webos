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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

/// Every colour the widgets draw with. Copy, so a widget can pull one out and keep drawing
/// through `&mut self`.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Behind everything.
    pub bg: Color,
    /// The nav column, and every modal card — a surface that sits *on* [`Self::bg`].
    pub panel: Color,
    /// [`Self::panel`] as frosted glass: the same surface, translucent, for a modal card
    /// drawn over a [`DrawCmd::Frost`](crate::ui::render::DrawCmd::Frost) of what it covers.
    /// Its alpha is the whole effect — too opaque and the blur behind it stops reading.
    pub panel_glass: Color,
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
    /// The lit edge every piece of glass is outlined with — a modal card, a dropdown's popup,
    /// a toast. One value so the surfaces read as cut from the same sheet; they had three
    /// different whites before anyone put them side by side.
    pub glass_edge: Color,
}

impl Theme {
    /// A neutral dark theme, used until an app [`install`]s its own.
    pub const DEFAULT: Self = Self {
        bg: Color::RGB(0x14, 0x14, 0x14),
        panel: Color::RGB(0x1c, 0x1c, 0x1c),
        panel_glass: Color::RGBA(0x1c, 0x1c, 0x1c, 0xda),
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
        glass_edge: Color::RGBA(0xff, 0xff, 0xff, 0x18),
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

/// Whether the menus draw as frosted glass. Unlike the palette this *does* change at runtime
/// (Settings → Theme → "Default Glossy"), so it is an atomic rather than part of the `OnceLock`:
/// the theme is a constant of the process, this is a setting.
///
/// Every glass surface reads it through [`glass_fill`], down inside widget code that has no
/// `RenderCtx` to thread it through — which is why this is a global and not app state. `app`
/// keeps `Settings::frosted` as the source of truth and pushes it here (`App::restyle`), and
/// decides for itself whether to push a `DrawCmd::Frost`. Flipping it makes the tiles that
/// baked the old fill stale, so `restyle` drops them too.
static FROSTED: AtomicBool = AtomicBool::new(true);

/// Bumped whenever a style input changes, and mixed into every tile's cache version
/// ([`crate::ui::cache::version`]) so a flip invalidates every baked tile at once.
///
/// The alternative was a hand-maintained list of which tiles carry a [`glass_fill`]. It went
/// stale the first time one was added — and it could not cover the grid's card tiles at all,
/// which are keyed on content that does not change when the theme does.
static STYLE_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn set_frosted(on: bool) {
    if FROSTED.swap(on, Ordering::Relaxed) != on {
        STYLE_EPOCH.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub fn style_epoch() -> u64 {
    STYLE_EPOCH.load(Ordering::Relaxed)
}

#[inline]
pub fn frosted() -> bool {
    FROSTED.load(Ordering::Relaxed)
}

/// What a raised surface is filled with: the translucent
/// [`Theme::panel_glass`] on the glossy look, the opaque [`Theme::panel`] on the default one.
///
/// A modal card, a dropdown's popup, a toast and a scroll-edge fade all take this, so one
/// switch moves the whole set and none of them can drift from the others.
pub fn glass_fill() -> Color {
    let t = theme();
    if frosted() {
        t.panel_glass
    } else {
        t.panel
    }
}

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
