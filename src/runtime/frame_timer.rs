//! Where a menu render tick's time went.
//!
//! The loop paces off each tick's own start (`TICK_BUDGET`), so a tick that overruns its
//! budget silently halves the frame rate instead of failing — the one class of regression
//! this UI can have without anything logging it. This times each stage of the tick and
//! reports the breakdown: at WARN when the budget was blown, at DEBUG otherwise.
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

    /// Logs the breakdown — WARN past `budget`, DEBUG within it.
    pub fn report(&self, budget: Duration, stats: &FrameStats) {
        let total = self.elapsed();
        let breakdown = self.breakdown();
        let FrameStats {
            screen,
            rebuilt,
            tiles,
            text,
        } = stats;
        if total > budget {
            let (resident, mib) = (tiles.len(), tiles.bytes() as f32 / (1024.0 * 1024.0));
            tracing::warn!("frame overran {budget:?}: {total:?} on {screen:?} ({breakdown}, {rebuilt} tiles rebuilt, {resident} resident / {mib:.1} MiB, {text} text)");
        } else {
            tracing::debug!("frame {total:?} on {screen:?} ({breakdown})");
        }
    }

    fn breakdown(&self) -> String {
        Stage::ALL
            .iter()
            .map(|s| format!("{} {:?}", s.name(), self.stages[*s as usize]))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
