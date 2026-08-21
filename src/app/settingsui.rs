//! The settings document and the UI editing it.
//!
//! `settings` is the live global document — the value the stream is launched with, and what the
//! Settings screen writes to. The rest is the editing surface around it: which per-game override
//! is open, the dropdown overlay and its fade, and whether the bitrate slider is being dragged.

use crate::services::store::Settings;

use crate::app::state::gamesettings::GameSettingsState;
use crate::app::DropdownState;
use crate::ui;

pub(crate) struct SettingsUi {
    pub(crate) settings: Settings,
    /// What `Screen::GameSettings` is editing, `None` when it isn't up — see
    /// `app::state::gamesettings`.
    pub(crate) game_settings: Option<GameSettingsState>,
    pub(crate) dropdown: Option<DropdownState>,
    /// Dropdown overlay's own open/close fade, payload `(row, focused)` so the close-fade can
    /// still draw it after `dropdown` goes `None`.
    pub(crate) dropdown_fade: ui::fade::ModalFade<(usize, usize)>,
    /// Whether the mouse button is down on the Settings screen's slider row (Bitrate) with the
    /// press having landed on the track itself — while `true`, `MouseMotion` drags the thumb to
    /// the pointer's x instead of just moving hover focus. Cleared on `MouseButtonUp`; never
    /// survives a screen change since the button can't be released on another screen from
    /// webOS's own D-pad OK -> click translation.
    pub(crate) slider_drag: bool,
}

impl SettingsUi {
    pub(crate) fn new(settings: Settings) -> Self {
        Self {
            settings,
            game_settings: None,
            dropdown: None,
            dropdown_fade: ui::fade::ModalFade::modal(),
            slider_drag: false,
        }
    }
}
