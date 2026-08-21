//! This app's icon vocabulary: which Material glyph means "settings", "wake", "forget".
//!
//! Every icon is a glyph from the bundled Material Icons subset (`crate::assets`), not
//! vector shapes — a real icon font draws cleaner than hand-rolled path math. Rendered as
//! text, then scaled to fit the icon rect (see [`ui::Canvas::icon`](crate::ui::Canvas::icon)).
//!
//! Not in `ui`: the library draws whatever glyph it is handed, and has no opinion on which
//! pictogram stands for which of this app's actions. The two glyphs that *are* the library's
//! — a close X and a pin — are not restated here; the few app rows that want one read
//! `ui::theme::icons()`, so there is one codepoint per picture in the binary.

pub const ICON_TV: &str = "\u{E333}";
pub const ICON_LOCK: &str = "\u{E897}";
pub const ICON_ADD: &str = "\u{E145}";
pub const ICON_SETTINGS: &str = "\u{E8B8}";
pub const ICON_MONITOR: &str = "\u{EF5B}";
pub const ICON_SCHEDULE: &str = "\u{E8B5}";
pub const ICON_SIGNAL: &str = "\u{E202}";
pub const ICON_SUN: &str = "\u{E430}";
pub const ICON_POWER: &str = "\u{E8AC}";
pub const ICON_DELETE: &str = "\u{E872}";
pub const ICON_EDIT: &str = "\u{E3C9}";
pub const ICON_INFO: &str = "\u{E88E}";
pub const ICON_WRENCH: &str = "\u{E869}";
pub const ICON_BUG: &str = "\u{E868}";
pub const ICON_CHART: &str = "\u{E6E1}";
pub const ICON_MEMORY: &str = "\u{E322}";
pub const ICON_MOVIE: &str = "\u{E02C}";
pub const ICON_VISIBILITY: &str = "\u{E8F4}";
pub const ICON_SEND: &str = "\u{E163}";
pub const ICON_GAMEPAD: &str = "\u{E338}";
/// Material `palette` — the frosted-theme toggle.
pub const ICON_PALETTE: &str = "\u{E40A}";
pub const ICON_MOUSE: &str = "\u{E323}";
pub const ICON_TOUCH: &str = "\u{E913}";
/// Material `folder` — a collection of cards.
pub const ICON_FOLDER: &str = "\u{E2C7}";
/// Material `drag_handle` — the grip that puts a row into drag mode.
pub const ICON_REORDER: &str = "\u{E945}";
