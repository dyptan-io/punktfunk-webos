//! Experimental screen logic. Rendering lives in `app::view::experimental`.
use crate::app::menu;
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::{Screen, SettingsScope};

impl App {
    /// Installs `Settings::theme` as the look everything draws in. Bumps the theme epoch,
    /// which stales every tile baked in the old one — and, on the next frame, releases the
    /// compositor's blur chain when the new look has no glass. Called on the pick and once at
    /// startup, so the two paths cannot disagree.
    pub(crate) fn restyle(&self) {
        crate::ui::theme::select(self.settings_ui.settings.theme);
    }

    /// Probes root access for the Game mode row, once per launch — rooting can come and go
    /// between boots, so it is never persisted, and no screen but this one needs the answer.
    ///
    /// Off-thread, and deliberately not on the frame the modal opens: the probe forks
    /// `luna-send-pub`, which in turn launches the Homebrew Channel's service on demand, and
    /// that costs enough CPU on this hardware to show as a stutter in the open animation
    /// running beside it. [`App::tick_root_probe`] starts it once that animation is over.
    fn start_root_probe(&mut self) {
        if self.hosts.rooted.is_some() || self.jobs.rooted.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        match std::thread::Builder::new().name("root-probe".into()).spawn(move || {
            let _ = tx.send(crate::platform::webos::game_mode::probe_rooted());
        }) {
            Ok(_) => self.jobs.rooted = Some(rx),
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
        self.hosts.rooted = Some(rooted);
        if !rooted && self.settings_ui.settings.game_mode {
            self.settings_ui.settings.game_mode = false;
            self.persist();
        }
    }

    /// Starts an owed root probe once the modal that wants it has finished opening. Called
    /// each tick alongside the `drain_*`s.
    pub(crate) fn tick_root_probe(&mut self) {
        // Still on Experimental: leaving before the animation settles defers the probe to the
        // next visit rather than paying for it behind a screen that no longer asks.
        if !self.jobs.root_probe_owed
            || !matches!(self.nav.screen, Screen::Experimental)
            || self.render.modal.fade.is_animating()
        {
            return;
        }
        self.jobs.root_probe_owed = false;
        self.start_root_probe();
    }

    /// Picks up the probe's verdict, unlocking the Game mode row (or explaining why not).
    /// Reports whether anything changed, so the open screen redraws.
    pub(crate) fn drain_rooted(&mut self) -> bool {
        let Some(rx) = &self.jobs.rooted else { return false };
        let rooted = match rx.try_recv() {
            Ok(rooted) => rooted,
            // A probe thread that died without sending would otherwise leave the row on its
            // checking caption forever, so a dead channel settles like a failed spawn.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
        };
        self.jobs.rooted = None;
        self.settle_rooted(rooted);
        true
    }

    /// Opens the Experimental screen (Settings → `menu::SettingsRow::Experimental`). Holds unstable,
    /// off-by-default toggles (hardware Opus decode, Game mode on rooted sets).
    pub(crate) fn open_experimental(&mut self) {
        // Owed, not started — see `start_root_probe`.
        self.jobs.root_probe_owed = self.hosts.rooted.is_none() && self.jobs.rooted.is_none();
        self.nav.enter(Screen::Experimental, 0);
    }

    /// All rows are plain Left/Right/Confirm toggles. Back saves and returns to Settings.
    pub(crate) fn handle_experimental_event(&mut self, ev: MenuEvent) {
        if self.list_nav_event(ev) {
            return;
        }
        match (
            menu::EXP_ROWS.get(self.nav.cursor(ScreenKey::Experimental)).copied(),
            ev,
        ) {
            // A locked row (see `menu::exp_row_lock`) rejects the press — the greyed control
            // already says the value is fixed.
            (Some(menu::ExpRow::GameMode), MenuEvent::Left | MenuEvent::Right | MenuEvent::Confirm)
                if menu::exp_row_lock(menu::ExpRow::GameMode, self.hosts.rooted).is_none() =>
            {
                let from = self.settings_ui.settings.game_mode;
                self.settings_ui.settings.game_mode = !from;
                self.arm_switch_anim(from);
            }
            (_, MenuEvent::Back) => {
                self.persist();
                self.nav.resume(Screen::Settings(SettingsScope::Global));
            }
            _ => {}
        }
    }
}
