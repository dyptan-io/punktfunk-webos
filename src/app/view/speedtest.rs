//! The per-host network speed test — presentation. Logic lives in `app::state::speedtest`.
use crate::app::state::speedtest::{recommended_kbps, SpeedTestState};
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::ConfirmButton;
use crate::ui::Canvas;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) const TITLE: &str = "Network speed test";

/// Primary button label (built from the measurement, not constant). No recommendation →
/// "Retry" rather than "Close", to leave the user an action on a low-throughput result.
pub(crate) fn apply_label(recommended: Option<u32>) -> String {
    recommended.map_or_else(|| "Retry".to_string(), |kbps| format!("Use {} Mbps", kbps / 1000))
}

/// Finished-test buttons (apply the recommendation, or close). Built per render.
pub(crate) fn buttons(apply_label: &str) -> [ConfirmButton<'_>; 2] {
    [
        ConfirmButton {
            icon: Some(crate::app::view::icons::ICON_SIGNAL),
            label: apply_label,
            color: ui::style::theme().accent_bright,
        },
        ConfirmButton {
            icon: None,
            label: "Close",
            color: ui::style::theme().text,
        },
    ]
}

/// Whether the test has stopped — the state in which the card grows a button row.
pub(crate) fn finished(state: Option<&SpeedTestState>) -> bool {
    matches!(
        state,
        Some(SpeedTestState::Done { .. }) | Some(SpeedTestState::Failed(_))
    )
}

/// The status sentence for the current phase — also measured (without drawing) to place
/// the card and its buttons, so it lives in one place.
pub(crate) fn status(state: Option<&SpeedTestState>, host_name: &str) -> String {
    match state {
        None | Some(SpeedTestState::Connecting) => format!("Connecting to {host_name}…"),
        Some(SpeedTestState::Measuring { partial }) => {
            // Deliberately bytes, not Mbps: `throughput_kbps`'s denominator (since core
            // 0.24, the client-measured receive interval, falling back to the host's
            // burst duration) is frozen only when the end-of-burst report lands — so a
            // "Mbps so far" reading here could never show anything. `recv_bytes` is live
            // throughout.
            let so_far = partial
                .filter(|p| p.recv_bytes > 0)
                .map_or_else(String::new, |p| format!(" — {} MB in", p.recv_bytes / (1024 * 1024)));
            format!("Measuring{so_far} over the real data plane…")
        }
        Some(SpeedTestState::Done { outcome, confirmed }) => {
            // The burst deliberately asks for more than the link can carry — that
            // overshoot is *how* the ceiling is found — so a high loss figure here is
            // expected and says nothing bad about the network on its own. Labelled
            // accordingly, since a bare "80% loss" reads as a fault.
            let detail = if *confirmed {
                format!(
                    "({:.0}% of the deliberately over-capacity test burst didn't fit — \
                     that's how the ceiling is found)",
                    outcome.loss_pct
                )
            } else {
                "(the host's own report didn't make it back, so this is measured from what \
                 arrived here — treat it as a floor)"
                    .to_string()
            };
            let base = format!(
                "{} Mbps delivered · {} MB in {} ms\n{detail}",
                outcome.throughput_kbps / 1000,
                outcome.recv_bytes / (1024 * 1024),
                outcome.elapsed_ms,
            );
            match recommended_kbps(outcome) {
                Some(kbps) => format!(
                    "{base}\n\nRecommended bitrate {} Mbps (~70% of measured, leaving headroom \
                     for FEC and loss). This measures what this TV can actually receive and \
                     decrypt, not raw link speed.",
                    kbps / 1000
                ),
                None => format!(
                    "{base}\n\nToo little got through to recommend a bitrate. If the host \
                     reported bytes sent but none arrived, the path is dropping them; if it \
                     sent none at all, it may not support the probe. The app log has both \
                     figures."
                ),
            }
        }
        Some(SpeedTestState::Failed(e)) => format!("Couldn't measure: {e}"),
    }
}

pub(crate) fn card_rect(
    screen_w: u32,
    screen_h: u32,
    fonts: &Fonts,
    state: Option<&SpeedTestState>,
    host_name: &str,
) -> Rect {
    let status = status(state, host_name);
    let done = finished(state);
    ui::widgets::simple_modal_card(screen_w, screen_h, |probe| {
        let header_end = ui::text::modal_header_end_y(fonts, probe, &status);
        if done {
            (header_end + 32 + 72 + 32) as u32
        } else {
            (header_end + 32) as u32
        }
    })
}

/// The button row's rect, below the status text.
pub(crate) fn buttons_rect(card: Rect, fonts: &Fonts, state: Option<&SpeedTestState>, host_name: &str) -> Rect {
    let after = ui::text::modal_header_end_y(fonts, card, &status(state, host_name));
    Rect::new(card.x() + 32, after + 32, card.width().saturating_sub(64), 72)
}

/// The recommendation this result's primary button would apply, if any.
pub(crate) fn recommendation(state: Option<&SpeedTestState>) -> Option<u32> {
    match state {
        Some(SpeedTestState::Done { outcome, .. }) => recommended_kbps(outcome),
        _ => None,
    }
}

/// The speed-test modal as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub state: Option<&'a SpeedTestState>,
    pub host_name: &'a str,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts, self.state, self.host_name)
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let (state, host_name) = (self.state, self.host_name);
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.modal_shell(card, hover_close)?;
        let failed = matches!(state, Some(SpeedTestState::Failed(_)));
        c.modal_header(
            card,
            TITLE,
            ui::style::theme().text,
            &status(state, host_name),
            if failed {
                ui::style::theme().error
            } else {
                ui::style::theme().muted
            },
        )?;
        if finished(state) {
            let apply_label = apply_label(recommendation(state));
            // `usize::MAX` = nothing focused; the focused button is its own tile.
            c.render(
                ui::widgets::ConfirmButtons::new(&buttons(&apply_label)),
                buttons_rect(card, c.fonts, state, host_name),
            )?;
        }
        Ok(())
    }
}
