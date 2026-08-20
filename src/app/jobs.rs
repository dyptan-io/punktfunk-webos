//! Every background job the menu owns, in one place.
//!
//! Each field is the receiving end of work running off the UI thread; `None` means nothing is in
//! flight. Cancellation is dropping the receiver, so the cancel helpers here are the whole
//! protocol — the sender notices on its next send and stops.
//!
//! The `drain_*` bodies stay on `App` (they apply results to app state, which is where that
//! belongs); `App::drain_jobs` is the one call the runtime makes per tick.

use std::sync::mpsc::Receiver;

use crate::app::state::{reach::Reachability, sendlogs::SendLogsMsg, speedtest::SpeedTestMsg};
use crate::services::art::ArtLoader;
use crate::services::discovery::Discovery;
use crate::services::library::GamesLoaded;
use crate::app::PairingOutcome;

#[derive(Default)]
pub(crate) struct Jobs {
    /// `None` if the mDNS daemon didn't start. Owned here so it stops with the menu.
    pub(crate) discovery: Option<Discovery>,
    pub(crate) games: Option<Receiver<GamesLoaded>>,
    pub(crate) art: Option<ArtLoader>,
    /// Drained each tick by `drain_pairing`; dropping it (Back while busy) cancels.
    pub(crate) pairing: Option<Receiver<PairingOutcome>>,
    pub(crate) reach: Option<Receiver<Reachability>>,
    /// Delivers the background probe's progress/result — dropping it cancels.
    pub(crate) speed_test: Option<Receiver<SpeedTestMsg>>,
    /// Delivers the background log upload's result; `None` when no upload is in flight.
    pub(crate) send_logs: Option<Receiver<SendLogsMsg>>,
    /// Answers [`App::start_root_probe`].
    pub(crate) rooted: Option<Receiver<bool>>,
    /// Whether the Experimental screen still owes its root probe. The probe forks
    /// `luna-send-pub` and wakes the Homebrew Channel's service, and on this hardware that
    /// costs enough CPU to drop frames out of whatever is animating — so it is held until the
    /// modal it belongs to has finished opening rather than started on the open frame.
    pub(crate) root_probe_owed: bool,
}

impl Jobs {
    pub(crate) fn cancel_pairing(&mut self) {
        self.pairing = None;
    }

    pub(crate) fn cancel_speed_test(&mut self) {
        self.speed_test = None;
    }

    /// Drops the library fetch and the art loader together — they are one pipeline, and a stale
    /// fetch landing after a host switch would start art for the wrong library.
    pub(crate) fn cancel_library(&mut self) {
        self.games = None;
        self.art = None;
    }
}

impl crate::app::App {
    /// One tick's worth of background results, applied in the order the runtime used to call
    /// them in. Returns whether anything changed and so a redraw is owed.
    pub fn drain_jobs(&mut self) -> bool {
        self.tick_root_probe();
        let mut dirty = self.drain_discovery();
        dirty |= self.drain_art();
        dirty |= self.drain_games();
        dirty |= self.drain_pairing();
        dirty |= self.drain_rooted();
        dirty |= self.drain_speed_test();
        dirty |= self.drain_send_logs();
        self.tick_reachability();
        dirty |= self.drain_reachability();
        dirty
    }
}
