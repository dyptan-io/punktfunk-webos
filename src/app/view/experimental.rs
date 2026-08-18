//! Experimental: unstable features, off by default. Logic lives in `app::state::experimental`.
//!
//! `rooted` reaches every entry point rather than being read here, so this module stays
//! platform-neutral — the Game mode row only exists on a rooted TV, which changes the row
//! count and so the card's height.
use crate::services::store::Settings;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalScreen;
use anyhow::Result;

pub const TITLE: &str = "Experimental";
pub const SUBTITLE: &str = "Unstable, off by default.";

/// The frame pacer toggle (`session::PtsPacer`, live-toggleable mid-stream with the Blue
/// button), the software-audio override, and Game mode on rooted sets. All off by default.
/// Order must match `menu::EXP_ROW_*`.
pub fn rows(settings: &Settings, rooted: bool) -> Vec<FocusRow> {
    let mut rows = vec![FocusRow::toggle(
        crate::app::view::icons::ICON_SCHEDULE,
        "Frame pacer",
        settings.video_pacing,
    )
    .with_subtext(ui::widgets::RowSubtext::hint(if settings.video_pacing {
        "Toggles live with the Blue button"
    } else {
        "May improve framerate smoothness, adds latency"
    }))];
    // Opt-in, not the default: the audio-enabled load is rejected on at least some webOS 5+ sets
    // and takes the video plane down with it (black picture, sound fine — see
    // `Settings::ndl_audio_offload`). Software Opus is the path that always exists, so it stays
    // the default and this offers the offload to anyone whose TV can take it.
    rows.push(
        FocusRow::toggle(
            crate::app::view::icons::ICON_MEMORY,
            "Audio offload",
            settings.ndl_audio_offload,
        )
        .with_subtext(ui::widgets::RowSubtext::hint(if settings.ndl_audio_offload {
            "Turn off for decoding Opus in software"
        } else {
            "Offload Opus decode to NDL"
        })),
    );
    // Driving the TV's Game picture/sound modes needs the Homebrew Channel's root helper — the
    // public bus is denied `settingsservice` outright (see `platform::webos::game_mode`). So the
    // row only exists on a rooted set, where it's known to work.
    if rooted {
        rows.push(
            FocusRow::toggle(crate::app::view::icons::ICON_GAMEPAD, "Game mode", settings.game_mode)
                .with_subtext(ui::widgets::RowSubtext::hint("Your TV is rooted, you can use ALLM")),
        );
    }
    rows
}

/// Row count without building the `FocusRow` vec — for card sizing and hit-testing. The
/// Game mode row is only offered on a rooted TV, so the screen is one row shorter otherwise.
pub fn row_count(rooted: bool) -> usize {
    2 + usize::from(rooted)
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, rooted: bool) -> Rect {
    ui::widgets::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, row_count(rooted))
}

/// The experimental-features list as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub settings: &'a Settings,
    pub rooted: bool,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts, self.rooted)
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        Some(ui::widgets::list_modal_content_rect(
            card,
            fonts,
            SUBTITLE,
            rows(self.settings, self.rooted).len(),
        ))
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.list_modal_screen(card, TITLE, SUBTITLE, &rows(self.settings, self.rooted), hover_close)
    }
}
