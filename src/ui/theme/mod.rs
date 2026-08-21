//! The look every widget draws in: one palette, one set of chrome glyphs, and one description
//! of what its surfaces are made of.
//!
//! A theme here is a *value*, not a value plus a handful of switches. [`Theme`] holds the
//! whole of it, the looks on offer are `static` constants in [`presets`], and picking one is
//! storing its index ([`select`]). Every accessor hands back a `&'static`, so a widget can
//! pull a colour out and keep drawing through `&mut self`, and no reader ever takes a lock or
//! allocates.
//!
//! Read directly rather than threaded through every call: a look is process-wide, and the
//! alternative is a sixth argument on every tile builder — the exact parameter-passing that
//! `Canvas` exists to have removed. [`epoch`] is how the caches learn a pick has happened.
//!
//! This app's icon *vocabulary* is not here — which pictogram means "settings" or "forget
//! host" is the app's, and it already passes those in as `&'static str`
//! (`FocusRow::action(icon, …)`); see `app::view::icons`. What is here is [`Icons`]: the four
//! glyphs the library's own chrome draws unasked (a modal's close button, a dropdown's
//! chevron, a row's overflow affordance, the pinned badge), which it must get from somewhere
//! and which no caller is in a position to pass down.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::core::model::ThemeChoice;
use crate::ui::render::Color;

pub mod presets;

/// One complete look: what it is called, what it is coloured, and what its raised surfaces
/// are made of.
pub struct Theme {
    /// Display name, and the only label the Theme dropdown has.
    pub name: &'static str,
    /// The persisted value that names this look. [`PRESETS`] is the only place the two are
    /// tied together, so adding a look is adding one entry there.
    pub choice: ThemeChoice,
    pub palette: Palette,
    pub icons: Icons,
    /// `Some` where this look's raised surfaces are frosted glass over a blurred copy of what
    /// they cover (`DrawCmd::Frost`), `None` where they are flat opaque fills.
    ///
    /// An `Option` rather than a `bool`: "is this look glossy" and "how wide is its blur" were
    /// two separate answers living two layers apart, so a look could be turned on without the
    /// numbers that make it cost anything. Asking for [`Glass`] returns both or neither.
    pub glass: Option<Glass>,
}

/// Every colour the widgets draw with. Copy, so a widget can pull one out and keep drawing
/// through `&mut self`.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
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
    /// The lit edge every raised surface is outlined with — a modal card, a dropdown's popup,
    /// a toast. One value so they read as cut from the same sheet; they had three different
    /// whites before anyone put them side by side.
    pub glass_edge: Color,
}

/// The frosted-glass surface: its tint, and the two numbers the compositor blurs and finishes
/// it with.
///
/// Kept together because they only mean anything together — a tint with no blur behind it is
/// a translucent wash over cover art, which is why the card title strip goes opaque in the
/// same moment the blur goes away.
#[derive(Clone, Copy, Debug)]
pub struct Glass {
    /// [`Palette::panel`] as glass: the same surface, translucent. Its alpha is the whole
    /// effect — too opaque and the blur behind it stops reading.
    pub panel: Color,
    /// How wide the blur spreads, in screen pixels. Declared, never inferred from the pane's
    /// size: sizing it to the pane made a card's one-line title strip and the tall submenu it
    /// grows into land on different blur levels, so the frost visibly changed the instant the
    /// panel opened. The compositor rounds it down to a level of its minification chain.
    pub blur: u32,
    /// How strongly the etch grain sits over the blur. Low: the point is a surface that
    /// catches light, not visible noise. Raise it and the glass starts to look like film
    /// grain.
    pub grain: u8,
}

/// The glyphs the library's own chrome needs, as codepoints in whatever icon font the app
/// loaded for [`FontId::Icon`](crate::ui::text_raster::FontId).
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

/// Every look on offer, in the order the Theme dropdown lists them — the single table, so
/// adding a look is adding one entry and nothing else has an order to keep in step with. A
/// `static` rather than a `const` so the accessors can hand out real `&'static` references
/// into it.
/// Glossy first: the list is the Theme dropdown's order *and* the fallback for a document
/// that names no look at all, so the look a fresh install draws in is simply the first entry
/// (see [`index_of`] and `ACTIVE`'s initial value).
pub static PRESETS: [Theme; 2] = [presets::GLOSSY, presets::DEFAULT];

/// Which preset is drawing, as its index in [`PRESETS`]. An index rather than a pointer or a
/// lock: the looks are constants of the binary, so picking one is one relaxed store and
/// reading one is one relaxed load.
///
/// Starts on the first preset, which is also what an unset `ThemeChoice` resolves to — so the
/// handful of frames drawn before `App::restyle` reads the document are already in the look
/// the document is about to ask for.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Bumped whenever [`select`] changes the pick, and mixed into every tile's cache version
/// ([`crate::ui::cache::version`]) so one pick invalidates every baked tile at once.
///
/// The alternative was a hand-maintained list of which tiles carry a [`glass_fill`]. It went
/// stale the first time one was added — and it could not cover the grid's card tiles at all,
/// which are keyed on content that does not change when the look does.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Where `choice` sits in [`PRESETS`], or the first look for a name this build has no preset
/// for — the same answer `ThemeChoice`'s lenient `Deserialize` gives.
fn index_of(choice: ThemeChoice) -> usize {
    PRESETS.iter().position(|t| t.choice == choice).unwrap_or(0)
}

/// The look `choice` names.
#[inline]
pub fn for_choice(choice: ThemeChoice) -> &'static Theme {
    &PRESETS[index_of(choice)]
}

/// Draws everything in `choice` from here on. Returns whether that was a change — the caller
/// still has to drop the tiles baked in the old look (`App::restyle`).
pub fn select(choice: ThemeChoice) -> bool {
    let i = index_of(choice);
    if ACTIVE.swap(i, Ordering::Relaxed) == i {
        return false;
    }
    EPOCH.fetch_add(1, Ordering::Relaxed);
    true
}

/// The look currently drawing. Indexed raw: [`select`] is the only writer and it only ever
/// stores a position it found in [`PRESETS`].
#[inline]
pub fn active() -> &'static Theme {
    &PRESETS[ACTIVE.load(Ordering::Relaxed)]
}

#[inline]
pub fn palette() -> &'static Palette {
    &active().palette
}

#[inline]
pub fn icons() -> &'static Icons {
    &active().icons
}

/// This look's glass, or `None` where its panels are flat. The one gate: a caller that gets
/// `Some` has the blur width and grain it needs in the same breath.
#[inline]
pub fn glass() -> Option<&'static Glass> {
    active().glass.as_ref()
}

#[inline]
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::Relaxed)
}

/// What a raised surface is filled with: the translucent [`Glass::panel`] on a glossy look,
/// the opaque [`Palette::panel`] on a flat one.
///
/// A modal card, a dropdown's popup, a toast and a scroll-edge fade all take this, so one
/// pick moves the whole set and none of them can drift from the others.
pub fn glass_fill() -> Color {
    glass().map_or(palette().panel, |g| g.panel)
}
