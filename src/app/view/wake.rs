//! The "host unreachable — wake it?" modal — presentation. Logic lives in `app::state::wake`.
use crate::app::WakeState;
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, ConfirmButton, Fonts, ModalScreen};
use anyhow::Result;

/// Card for the no-MAC modal: an informational "Host unreachable" message with no button
/// row (nothing to send), so it's a plain message card, not a confirm dialog.
pub(crate) fn message_card(screen_w: u32, screen_h: u32, fonts: &Fonts, status: &str) -> Rect {
    ui::simple_modal_card(screen_w, screen_h, |probe| {
        (ui::modal_header_end_y(fonts.raster, fonts.label, fonts.value, probe, status) + 32) as u32
    })
}

/// With a MAC it's the shared confirmation dialog; without one, the button-less card.
pub(crate) fn card_rect(screen_w: u32, screen_h: u32, wake: &WakeState, fonts: &Fonts) -> Rect {
    let status = status_text(wake);
    if wake.mac.is_empty() {
        message_card(screen_w, screen_h, fonts, &status)
    } else {
        ui::confirm_dialog_card(screen_w, screen_h, fonts, &status)
    }
}

/// Wake/Cancel pair — shared by the shell and the focused-button tile.
pub(crate) fn buttons() -> [ConfirmButton<'static>; 2] {
    [
        ConfirmButton {
            icon: Some(ui::ICON_POWER),
            label: "Wake host",
            color: ui::ACCENT_BRIGHT,
        },
        ConfirmButton {
            icon: None,
            label: "Cancel",
            color: ui::WHITE,
        },
    ]
}

/// Title varies: with a MAC it's an action ("Wake this host?"), without it it's state.
pub(crate) fn title(wake: &WakeState) -> &'static str {
    if wake.mac.is_empty() {
        "Host unreachable"
    } else if wake.sent {
        "Waking host…"
    } else {
        "Wake this host?"
    }
}

/// Status line; reconstructible from `wake` alone, so render and layout can't disagree.
pub(crate) fn status_text(wake: &WakeState) -> String {
    if wake.mac.is_empty() {
        format!(
            "{} isn't responding, and no Wake-on-LAN address is on record for it yet, so it \
             can't be woken from here. It will reconnect automatically once it's back online.",
            wake.name
        )
    } else if wake.sent {
        format!("Wake signal sent to {}. Waiting for it to come back online…", wake.name)
    } else {
        format!("{} isn't responding. It may be powered off or asleep.", wake.name)
    }
}

/// The wake-on-LAN modal as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub wake: &'a WakeState,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, self.wake, fonts)
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let wake = self.wake;
        let status = status_text(wake);
        // With a MAC it's the shared confirmation dialog (card + Wake/Cancel row); without one
        // it's a button-less informational card — `drain_discovery` reconnects automatically
        // once the host reappears on mDNS, so there is nothing for the user to do.
        let (card, button_row) = if wake.mac.is_empty() {
            (message_card(c.screen_w, c.screen_h, c.fonts, &status), None)
        } else {
            let (card, content) = ui::confirm_dialog_layout(c.screen_w, c.screen_h, c.fonts, &status);
            (card, Some(content))
        };
        c.modal_shell(card, hover_close)?;
        c.modal_header(card, title(wake), ui::WHITE, &status, ui::MUTED)?;
        if let Some(content) = button_row {
            // `usize::MAX` = nothing focused; the focused button is its own tile.
            c.confirm_buttons(content, &buttons(), usize::MAX)?;
        }
        Ok(())
    }
}
