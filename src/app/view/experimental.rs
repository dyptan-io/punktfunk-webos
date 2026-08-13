//! Experimental: unstable features, off by default. Logic lives in `app::state::experimental`.
//!
//! `rooted` reaches every entry point rather than being read here, so this module stays
//! platform-neutral — the Game mode row only exists on a rooted TV, which changes the row
//! count and so the card's height.
use crate::services::store::Settings;
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow, Fonts};
use anyhow::Result;

pub const TITLE: &str = "Experimental";
pub const SUBTITLE: &str = "Unstable, off by default.";

/// The frame pacer toggle (`session::PtsPacer`, live-toggleable mid-stream with the Blue
/// button) and Game mode on rooted sets. Both off by default and untested on hardware.
/// Order must match `menu::EXP_ROW_*`.
pub fn rows(settings: &Settings, rooted: bool) -> Vec<FocusRow> {
    let mut rows = vec![
        FocusRow::toggle(ui::ICON_SCHEDULE, "Frame pacer", settings.video_pacing).with_subtext(ui::RowSubtext::hint(
            if settings.video_pacing {
                "Toggles live with the Blue button"
            } else {
                "May improve framerate smoothness, adds latency"
            },
        )),
    ];
    // Driving the TV's Game picture/sound modes needs the Homebrew Channel's root helper — the
    // public bus is denied `settingsservice` outright (see `platform::webos::game_mode`). So the
    // row only exists on a rooted set, where it's known to work.
    if rooted {
        rows.push(
            FocusRow::toggle(ui::ICON_GAMEPAD, "Game mode", settings.game_mode)
                .with_subtext(ui::RowSubtext::hint("Your TV is rooted, you can use ALLM")),
        );
    }
    rows
}

/// Row count without building the `FocusRow` vec — for card sizing and hit-testing. The
/// Game mode row is only offered on a rooted TV, so the screen is one row shorter otherwise.
pub fn row_count(rooted: bool) -> usize {
    1 + usize::from(rooted)
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, rooted: bool) -> Rect {
    ui::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, row_count(rooted))
}

pub fn render(c: &mut Canvas, settings: &Settings, rooted: bool, hover_close: bool) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts, rooted);
    ui::render_list_modal_screen(c, card, TITLE, SUBTITLE, &rows(settings, rooted), hover_close)
}
