//! Where a menu render tick's time went.
//!
//! The loop paces off each tick's own start (`TICK_BUDGET`), so a tick that overruns its
//! budget silently halves the frame rate instead of failing — the one class of regression
//! this UI can have without anything logging it. This times each stage of the tick and
//! reports the breakdown — but only when the tick's own work blew the budget. A frame
//! inside budget is the steady state and says nothing at sixty lines a second, so it logs
//! nothing at all.
use std::time::{Duration, Instant};

use crate::core::screen::Screen;
use crate::ui::cache::TileStore;

/// The stages of one render tick, in the order `run_ui_flow` runs them.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    /// `App::prepare_tiles` — the CPU rasterization pass.
    Prepare,
    /// Texture uploads for whatever `Prepare` rebuilt.
    Upload,
    /// `App::draw_list` plus the overlay/toast/dialog commands appended to it.
    Compose,
    /// `Compositor::present` and the vsync swap.
    Present,
}

impl Stage {
    const ALL: [Self; 4] = [Self::Prepare, Self::Upload, Self::Compose, Self::Present];

    fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Upload => "upload",
            Self::Compose => "compose",
            Self::Present => "present",
        }
    }
}

/// What a tick produced, for the overrun report: enough to tell an expensive rasterization
/// pass from an unbounded cache.
pub(super) struct FrameStats<'a> {
    pub screen: Screen,
    /// Tiles rebuilt (and so re-uploaded) this tick.
    pub rebuilt: usize,
    /// The tile store itself, not its size: both counts below are read only on the overrun
    /// path, and totalling the resident bytes is a scan of the store that a frame inside its
    /// budget has no reason to pay.
    pub tiles: &'a TileStore,
    /// Resident entries in the rasterized-text cache — see G5: it is pruned only on its own
    /// terms, so its count is worth reading back.
    pub text: usize,
}

/// Stopwatch for one render tick. Cheap enough to run unconditionally: four `Instant`s and
/// no allocation, against a tick that costs milliseconds.
pub(super) struct FrameTimer {
    start: Instant,
    stage_start: Instant,
    stages: [Duration; Stage::ALL.len()],
}

impl FrameTimer {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            stage_start: now,
            stages: [Duration::ZERO; Stage::ALL.len()],
        }
    }

    /// Runs `work`, charging its wall time to `stage`. Stages need not be contiguous —
    /// whatever the loop does between two of them is charged to neither.
    pub fn stage<T>(&mut self, stage: Stage, work: impl FnOnce() -> T) -> T {
        self.stage_start = Instant::now();
        let out = work();
        self.stages[stage as usize] += self.stage_start.elapsed();
        out
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// The tick's own cost: everything but `Present`. `Present` ends in a blocking vsync
    /// swap, so it measures how long until the panel wanted the frame, not how long the
    /// frame took — on a 60Hz panel against a 16ms budget it sawtooths up to a full
    /// interval and back on its own, and budgeting the total reports that as an overrun
    /// every ~10 frames with nothing rebuilt. Only this is ours to blow.
    fn work(&self) -> Duration {
        self.stages[Stage::Prepare as usize]
            + self.stages[Stage::Upload as usize]
            + self.stages[Stage::Compose as usize]
    }

    /// Reports the tick, but only when its own work blew `budget` — a frame inside budget
    /// says nothing that is worth a line at any level, sixty times a second. Overruns are
    /// routine on this hardware (a menu rebuild alone blows a 16ms budget), so this is a
    /// debug line, not a warning. See [`Self::work`] for why `Present` is excluded.
    pub fn report(&self, budget: Duration, stats: &FrameStats) {
        let work = self.work();
        if work <= budget {
            return;
        }
        // Everything below is overrun-only: the breakdown allocates and `TileStore::bytes`
        // scans the store, and neither is worth paying on a healthy frame.
        let FrameStats {
            screen,
            rebuilt,
            tiles,
            text,
        } = stats;
        let (total, breakdown) = (self.elapsed(), self.breakdown());
        let (resident, mib) = (tiles.len(), tiles.bytes() as f32 / (1024.0 * 1024.0));
        tracing::debug!("frame work overran {budget:?}: {work:?} of {total:?} on {screen:?} ({breakdown}, {rebuilt} tiles rebuilt, {resident} resident / {mib:.1} MiB, {text} text)");
    }

    fn breakdown(&self) -> String {
        Stage::ALL
            .iter()
            .map(|s| format!("{} {:?}", s.name(), self.stages[*s as usize]))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
