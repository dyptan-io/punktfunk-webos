//! Experimental: unstable features, off by default. Logic lives in `app::state::experimental`.
//!
//! `rooted` reaches every entry point rather than being read here, so this module stays
//! platform-neutral.
use crate::app::menu::{self, ExpRow, ExpRowLock};
use crate::services::store::Settings;
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::text::Fonts;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub const TITLE: &str = "Experimental";
pub const SUBTITLE: &str = "Unstable, off by default.";

/// Game mode, then audio processing. Order must match `menu::EXP_ROWS`. `rooted` is the
/// root-probe verdict, `None` while it is still running.
pub fn rows(settings: &Settings, rooted: Option<bool>) -> Vec<FocusRow> {
    // Driving the TV's Game picture/sound modes needs the Homebrew Channel's root helper — the
    // public bus is denied `settingsservice` outright (see `platform::webos::game_mode`). The row
    // is always listed, but stays locked until the probe finds that helper actually reachable.
    let game_mode = FocusRow::toggle(crate::app::view::icons::ICON_GAMEPAD, "Game mode", settings.game_mode)
        .with_subtext(ui::widgets::RowSubtext::hint("Your TV is rooted, you can use ALLM"));
    let audio = FocusRow::dropdown(
        crate::app::view::icons::ICON_MEMORY,
        "Audio processing",
        menu::audio_route_label(settings.audio_route),
    )
    .with_subtext_opt(audio_route_hint(settings.audio_route));
    // The lock's caption replaces the row's own: a row the user can't change has nothing more
    // useful to say than why.
    let apply = |row: FocusRow, exp: ExpRow| match menu::exp_row_lock(exp, rooted) {
        Some(lock) => row.locked(true).with_subtext(lock_caption(lock)),
        None => row,
    };
    vec![
        apply(game_mode, ExpRow::GameMode),
        apply(audio, ExpRow::AudioProcessing),
    ]
}

/// What each audio route trades, on the row itself — the pick is a hardware path, and the
/// difference between the three is not inferable from their names.
fn audio_route_hint(route: crate::services::store::AudioRoutePref) -> Option<ui::widgets::RowSubtext> {
    use crate::services::store::AudioRoutePref;
    match route {
        AudioRoutePref::Software => None,
        AudioRoutePref::NdlPcm => Some(ui::widgets::RowSubtext::hint("Lower latency, up to 5.1")),
        AudioRoutePref::NdlOpus => Some(ui::widgets::RowSubtext::caution("Lowest latency, stereo only")),
    }
}

fn lock_caption(lock: ExpRowLock) -> ui::widgets::RowSubtext {
    match lock {
        ExpRowLock::RootUnknown => ui::widgets::RowSubtext::hint("Checking whether your TV is rooted..."),
        ExpRowLock::NotRooted => ui::widgets::RowSubtext::caution("Your TV is not rooted, Game mode is unavailable"),
        ExpRowLock::SoftwareOnly => ui::widgets::RowSubtext::hint("This TV has no NDL audio plane"),
    }
}

pub fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
    ui::widgets::list_modal_card_rect(screen_w, screen_h, fonts, SUBTITLE, menu::EXP_ROWS.len())
}

/// The experimental-features list as a [`ModalScreen`].
pub(crate) struct Modal<'a> {
    pub settings: &'a Settings,
    pub rooted: Option<bool>,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, fonts: &Fonts) -> Rect {
        card_rect(screen_w, screen_h, fonts)
    }

    fn content_rect(&self, card: Rect, fonts: &Fonts) -> Option<Rect> {
        Some(ui::widgets::list_modal_content_rect(
            card,
            fonts,
            SUBTITLE,
            menu::EXP_ROWS.len(),
        ))
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        let card = self.card_rect(c.screen_w, c.screen_h, c.fonts);
        c.list_modal_screen(card, TITLE, SUBTITLE, &rows(self.settings, self.rooted), hover_close)
    }
}
