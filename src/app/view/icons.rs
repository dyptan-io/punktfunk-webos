//! This app's icon vocabulary: which Material glyph means "settings", "wake", "forget".
//!
//! Every icon is a glyph from the bundled Material Icons subset (`crate::assets`), not
//! vector shapes — a real icon font draws cleaner than hand-rolled path math. Rendered as
//! text, then scaled to fit the icon rect (see [`ui::Canvas::icon`](crate::ui::Canvas::icon)).
//!
//! Not in `ui`: the library draws whatever glyph it is handed, and has no opinion on which
//! pictogram stands for which of this app's actions.

use crate::ui::render::Color;

pub const ICON_TV: &str = "\u{E333}";
pub const ICON_LOCK: &str = "\u{E897}";
pub const ICON_ADD: &str = "\u{E145}";
pub const ICON_CLOSE: &str = "\u{E5CD}";
pub const ICON_SETTINGS: &str = "\u{E8B8}";
pub const ICON_MONITOR: &str = "\u{EF5B}";
pub const ICON_SCHEDULE: &str = "\u{E8B5}";
pub const ICON_SIGNAL: &str = "\u{E202}";
pub const ICON_SUN: &str = "\u{E430}";
pub const ICON_CHEVRON_DOWN: &str = "\u{E5C5}";
pub const ICON_POWER: &str = "\u{E8AC}";
pub const ICON_DELETE: &str = "\u{E872}";
pub const ICON_EDIT: &str = "\u{E3C9}";
pub const ICON_INFO: &str = "\u{E88E}";
pub const ICON_MORE: &str = "\u{E5D3}";
pub const ICON_PIN: &str = "\u{F10D}";
pub const ICON_WRENCH: &str = "\u{E869}";
pub const ICON_BUG: &str = "\u{E868}";
pub const ICON_CHART: &str = "\u{E6E1}";
pub const ICON_MEMORY: &str = "\u{E322}";
pub const ICON_MOVIE: &str = "\u{E02C}";
pub const ICON_VISIBILITY: &str = "\u{E8F4}";
pub const ICON_SEND: &str = "\u{E163}";
pub const ICON_GAMEPAD: &str = "\u{E338}";
pub const ICON_MOUSE: &str = "\u{E323}";
pub const ICON_TOUCH: &str = "\u{E913}";

/// The punktfunk palette and the chrome glyphs `ui` draws for itself, installed into
/// `ui::style` once at startup (see `runtime::run`).
pub fn install_style() {
    crate::ui::style::install(
        crate::ui::style::Theme {
            bg: Color::RGB(0x14, 0x10, 0x1f),
            panel: Color::RGB(0x1c, 0x15, 0x30),
            surface: Color::RGB(0x2b, 0x21, 0x48),
            accent: Color::RGB(0x6c, 0x5b, 0xf3),
            accent_bright: Color::RGB(0xa7, 0x9f, 0xf8),
            text: Color::RGB(0xf5, 0xf5, 0xf5),
            muted: Color::RGB(0x9b, 0x94, 0xb8),
            warning: Color::RGB(0xff, 0xc1, 0x07),
            // A desaturated mint rather than a signal green — it sits next to brand purple
            // on every row, and a pure green fights it.
            caution: Color::RGB(0xd1, 0x84, 0x4a),
            error: Color::RGB(0xff, 0x6b, 0x6b),
            ok: Color::RGB(0x5c, 0xd6, 0xa0),
            scrim: Color::RGBA(0x00, 0x00, 0x00, 0x80),
            rule: Color::RGBA(0xff, 0xff, 0xff, 0x1e),
        },
        crate::ui::style::Icons {
            close: ICON_CLOSE,
            chevron_down: ICON_CHEVRON_DOWN,
            overflow: ICON_MORE,
            pin: ICON_PIN,
        },
    );
}
