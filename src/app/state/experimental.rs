//! Experimental screen logic. Rendering lives in `app::view::experimental`.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::{Screen, SettingsScope};
use crate::ui;
use std::time::Instant;

impl App {
    /// Probes root access for the Game mode row, once per launch — rooting can come and go
    /// between boots, so it is never persisted, and no screen but this one needs the answer.
    /// Off-thread: a luna round-trip has no business blocking the modal's open frame.
    fn start_root_probe(&mut self) {
        if self.rooted.is_some() || self.rooted_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        match std::thread::Builder::new().name("root-probe".into()).spawn(move || {
            let _ = tx.send(crate::platform::webos::game_mode::probe_rooted());
        }) {
            Ok(_) => self.rooted_rx = Some(rx),
            // Nothing will ever answer, so settle on "not rooted" rather than leaving the row
            // stuck on its checking caption.
            Err(e) => {
                tracing::warn!("root probe thread: {e}");
                self.settle_rooted(false);
            }
        }
    }

    /// Records the probe's verdict. A `game_mode` left on from when this TV *was* rooted has to
    /// go with it: the row is locked once the verdict is in, so nothing could switch it off
    /// again, and every stream start would keep paying for luna calls that can only fail.
    fn settle_rooted(&mut self, rooted: bool) {
        self.rooted = Some(rooted);
        if !rooted && self.settings.game_mode {
            self.settings.game_mode = false;
            self.persist();
        }
    }

    /// Picks up the probe's verdict, unlocking the Game mode row (or explaining why not).
    /// Reports whether anything changed, so the open screen redraws.
    pub(crate) fn drain_rooted(&mut self) -> bool {
        let Some(rx) = &self.rooted_rx else { return false };
        let rooted = match rx.try_recv() {
            Ok(rooted) => rooted,
            // A probe thread that died without sending would otherwise leave the row on its
            // checking caption forever, so a dead channel settles like a failed spawn.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
        };
        self.rooted_rx = None;
        self.settle_rooted(rooted);
        true
    }

    /// Opens the Experimental screen (Settings → `menu::SettingsRow::Experimental`). Holds unstable,
    /// off-by-default toggles (the software-audio override, Game mode on rooted sets).
    pub(crate) fn open_experimental(&mut self) {
        self.start_root_probe();
        self.nav.enter(Screen::Experimental, 0);
    }

    /// All rows are plain Left/Right/Confirm toggles. Back saves and returns to Settings.
    pub(crate) fn handle_experimental_event(&mut self, ev: MenuEvent) {
        let len = menu::EXP_ROWS.len();
        if ui::widgets::list_nav(self.nav.cursor_mut(ScreenKey::Experimental), len, menu::nav_dir(ev)) {
            self.modal.focus_anim = Some(Instant::now());
            return;
        }
        match (
            menu::EXP_ROWS.get(self.nav.cursor(ScreenKey::Experimental)).copied(),
            ev,
        ) {
            (Some(menu::ExpRow::HwAudio), MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm) => {
                let from = self.settings.ndl_audio_offload;
                self.settings.ndl_audio_offload = !from;
                self.modal.switch_anim = Some((Instant::now(), from, self.nav.cursor(ScreenKey::Experimental)));
            }
            // A locked row (see `menu::exp_row_lock`) rejects the press — the greyed control
            // already says the value is fixed.
            (Some(menu::ExpRow::GameMode), MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm)
                if menu::exp_row_lock(menu::ExpRow::GameMode, self.rooted).is_none() =>
            {
                let from = self.settings.game_mode;
                self.settings.game_mode = !from;
                self.modal.switch_anim = Some((Instant::now(), from, self.nav.cursor(ScreenKey::Experimental)));
            }
            (_, MenuEvent::Back) => {
                self.persist();
                self.nav.resume(Screen::Settings(SettingsScope::Global));
            }
            _ => {}
        }
    }
}
