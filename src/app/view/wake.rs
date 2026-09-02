//! The "host unreachable — wake it?" modal — presentation. Logic lives in `app::state::wake`.
use crate::app::WakeState;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

/// Status line; reconstructible from `wake` alone, so render and layout can't disagree.
pub(crate) fn status_text(wake: &WakeState) -> String {
    if wake.mac.is_empty() {
        format!(
            "{} isn't responding, and no Wake-on-LAN address is on record for it yet, so it \
             can't be woken from here. It will reconnect automatically once it's back online.",
            wake.name
        )
    } else {
        format!("{} isn't responding. It may be powered off or asleep.", wake.name)
    }
}

/// The wake-on-LAN modal as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub wake: &'a WakeState,
    /// `None` while there is nothing to press — a wake with no MAC on record is an
    /// informational card, since `drain_discovery` reconnects on its own once the host
    /// reappears on mDNS.
    pub confirm: Option<&'a crate::app::screens::confirm::Confirm>,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        match self.confirm {
            Some(confirm) => ui::tiles::confirm_dialog_card(screen_w, screen_h, fonts, &confirm.subtitle),
            None => ui::widgets::message_modal_card(screen_w, screen_h, fonts, &status_text(self.wake)),
        }
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let wake = self.wake;
        if let Some(confirm) = self.confirm {
            return c.confirm_dialog(
                "Wake this host?",
                &confirm.subtitle,
                ui::theme::palette().muted,
                &confirm.widgets(),
                hover_close,
                ui::tiles::ConfirmSurface::Glass,
            );
        }
        let status = status_text(wake);
        // Without a MAC this is a button-less informational card. Discovery reconnects
        // automatically once the host reappears, so there is nothing for the user to do.
        let card = ui::widgets::message_modal_card(c.screen_w, c.screen_h, c.fonts, &status);
        c.modal_shell(card, hover_close)?;
        c.modal_header(
            card,
            "Host unreachable",
            ui::theme::palette().text,
            &status,
            ui::theme::palette().muted,
        )?;
        Ok(())
    }
}
