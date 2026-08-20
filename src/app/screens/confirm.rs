//! The two-button confirm dialogs: Wake, Forget host, Send logs, and a finished speed test.
//!
//! All four are one card whose height its subtitle decides, one button row under it, and a
//! cursor picking between the two buttons. That used to be six `match self.screen` tables —
//! the subtitle, the focus, the focus setter, the button rect, and two inside `prepare_modal`
//! — each of which had to list the same four screens. Here the screen answers once, with a
//! value, and everything else reads that value.
use crate::app::view;
use crate::app::App;
use crate::core::screen::Screen;
use crate::ui::render::Color;
use crate::ui::style::theme;
use crate::ui::widgets::ConfirmButton;

/// One button of a [`Confirm`]. Owns its label because a speed test's primary button names
/// the bitrate it would apply, which is derived rather than static.
pub(crate) struct Button {
    pub icon: Option<&'static str>,
    pub label: String,
    pub color: Color,
}

/// An open confirm dialog, as data: what its card says, and what its two buttons are.
pub(crate) struct Confirm {
    pub subtitle: String,
    pub buttons: [Button; 2],
}

impl Confirm {
    fn new(icon: Option<&'static str>, label: &str, color: Color, cancel: &str, subtitle: String) -> Self {
        Self {
            subtitle,
            buttons: [
                Button {
                    icon,
                    label: label.to_string(),
                    color,
                },
                Button {
                    icon: None,
                    label: cancel.to_string(),
                    color: theme().text,
                },
            ],
        }
    }

    /// The widget-facing view of [`Self::buttons`] — borrowed, so nothing is copied per frame.
    pub fn widgets(&self) -> [ConfirmButton<'_>; 2] {
        std::array::from_fn(|i| ConfirmButton {
            icon: self.buttons[i].icon,
            label: &self.buttons[i].label,
            color: self.buttons[i].color,
        })
    }
}

impl App {
    /// The open confirm dialog, or `None` — on a screen that isn't one, and on the two whose
    /// buttons aren't up yet: a Wake with no MAC on record is a button-less message, and a
    /// speed test still running has nothing to apply.
    ///
    /// That `None` is load-bearing beyond the geometry: it is what says the dialog is not
    /// showing buttons, so a caller holding a `Some` has already proved the arm it is in is
    /// reachable — which is what four `expect`/`unreachable!` in `prepare_modal` used to
    /// assert by hand.
    pub(crate) fn confirm_of(&self) -> Option<Confirm> {
        Some(match self.nav.screen {
            Screen::ForgetHost => Confirm::new(
                Some(view::icons::ICON_DELETE),
                "Forget",
                theme().error,
                "Cancel",
                view::forget::subtitle(self.host_menu_host_name().unwrap_or_default()),
            ),
            Screen::SendLogs => Confirm::new(
                Some(view::icons::ICON_SEND),
                "Send",
                // The same red as Forget: both are consequential.
                theme().error,
                "Cancel",
                view::sendlogs::SUBTITLE.to_string(),
            ),
            Screen::Wake => Confirm::new(
                Some(view::icons::ICON_POWER),
                "Wake host",
                theme().accent_bright,
                "Cancel",
                view::wake::status_text(self.wake.as_ref().filter(|w| !w.mac.is_empty())?),
            ),
            Screen::SpeedTest => {
                let state = self.speed_test.as_ref();
                if !view::speedtest::finished(state) {
                    return None;
                }
                Confirm::new(
                    Some(view::icons::ICON_SIGNAL),
                    &view::speedtest::apply_label(view::speedtest::recommendation(state)),
                    theme().accent_bright,
                    // "Close" rather than "Cancel": the test has already run, so there is
                    // nothing left to call off.
                    "Close",
                    view::speedtest::status(state, &self.speed_test_name),
                )
            }
            _ => return None,
        })
    }
}
