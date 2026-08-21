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

/// The software-audio override, Game mode, and the frosted theme. `rooted` is the root-probe
/// verdict, `None` while it is still running.
pub fn rows(settings: &Settings, rooted: Option<bool>) -> Vec<FocusRow> {
    // Opt-in, not the default: the audio-enabled load is rejected on at least some webOS 5+ sets
    // and takes the video plane down with it (black picture, sound fine — see
    // `Settings::ndl_audio_offload`). Software Opus is the path that always exists, so it stays
    // the default and this offers the offload to anyone whose TV can take it.
    let mut rows = vec![FocusRow::toggle(
        crate::app::view::icons::ICON_MEMORY,
        "Audio offload",
        settings.ndl_audio_offload,
    )
    .with_subtext(ui::widgets::RowSubtext::hint(if settings.ndl_audio_offload {
        "Turn off for decoding Opus in software"
    } else {
        "Offload Opus decode to NDL"
    }))];
    // Driving the TV's Game picture/sound modes needs the Homebrew Channel's root helper — the
    // public bus is denied `settingsservice` outright (see `platform::webos::game_mode`). The row
    // is always listed, but stays locked until the probe finds that helper actually reachable.
    let game_mode = FocusRow::toggle(crate::app::view::icons::ICON_GAMEPAD, "Game mode", settings.game_mode)
        .with_subtext(ui::widgets::RowSubtext::hint("Your TV is rooted, you can use ALLM"));
    rows.push(match menu::exp_row_lock(ExpRow::GameMode, rooted) {
        // The lock's caption replaces the row's own: a row the user can't change has nothing
        // more useful to say than why.
        Some(lock) => game_mode.locked(true).with_subtext(lock_caption(lock)),
        None => game_mode,
    });
    // Purely cosmetic and purely local: nothing here rides a stream or a handshake, so it
    // applies the moment it is flipped rather than on the next launch.
    rows.push(
        FocusRow::toggle(crate::app::view::icons::ICON_PALETTE, "Frosted theme", settings.frosted).with_subtext(
            ui::widgets::RowSubtext::hint(if settings.frosted {
                "Turn off to remove the transparency effect"
            } else {
                "Turn on to add the transparency effect"
            }),
        ),
    );
    rows
}

fn lock_caption(lock: ExpRowLock) -> ui::widgets::RowSubtext {
    match lock {
        ExpRowLock::RootUnknown => ui::widgets::RowSubtext::hint("Checking whether your TV is rooted..."),
        ExpRowLock::NotRooted => ui::widgets::RowSubtext::caution("Your TV is not rooted, Game mode is unavailable"),
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
