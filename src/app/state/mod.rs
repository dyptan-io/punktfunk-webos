//! Per-screen app-state logic (event handling, state transitions). Split out of the
//! former fused `app/<screen>.rs` modules — see `docs/APP-REWORK-PLAN.md`.
//! Rendering counterparts live in `app::view`.
mod about;
pub(crate) mod addhost;
pub(crate) mod cardmenu;
pub(crate) mod collections;
mod cursorsettings;
pub(crate) mod diagnostics;
mod edithost;
mod experimental;
mod forget;
pub(crate) mod gamesettings;
pub(crate) mod hdrcalibration;
mod home;
pub(crate) mod hostmenu;
pub(crate) mod hostpower;
mod pairing;
pub(crate) mod reach;
pub(crate) mod sendlogs;
mod settings;
pub(crate) mod speedtest;
pub(crate) mod textfield;
mod wake;

use crate::app::App;

impl App {
    /// Everything an open screen changes *by itself*, with no event behind it — a wake probe
    /// landing, a pattern feed starting to present or giving up. Returns whether any of it moved
    /// pixels, which is the loop's whole interest in it.
    pub(crate) fn tick_screens(&mut self) -> bool {
        let mut changed = self.tick_wake();
        changed |= self.tick_hdr_pattern();
        changed
    }

    /// What the frame is cleared to before the draw list runs.
    ///
    /// Transparent while a screen is drawing over the video plane and that plane is actually
    /// presenting (see `screens::over_video`): NDL is an *underlay*, and the menu's opaque
    /// background is what normally hides it. Clearing transparent instead leaves the graphics
    /// plane carrying only the card, with the picture showing through everywhere else.
    pub(crate) fn frame_clear_color(&self) -> crate::ui::render::Color {
        // The launch's hero dissolves into the picture behind it (`app::hero`), so the plane
        // has to be uncovered for the whole dissolve rather than at the hand-off.
        if self.render.hero.dissolving()
            || (crate::app::screens::over_video(self.nav.screen) && self.hdr_pattern_presenting())
        {
            crate::ui::render::Color::RGBA(0, 0, 0, 0)
        } else {
            crate::ui::theme::palette().bg
        }
    }
}
