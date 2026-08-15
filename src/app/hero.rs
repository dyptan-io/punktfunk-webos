//! The connecting screen's backdrop: which game's wide art it wants, the one decoded
//! image kept for it, and the clocks that fade that image in, pan it, and fade it out
//! again once the stream is ready.
//!
//! One struct rather than seven `App` fields, because none of them mean anything on their
//! own — every launch moves all of them together.
use std::time::{Duration, Instant};

use crate::services::art::HeroImage;
use crate::ui;
use crate::ui::render::RectF;

/// How long a launch's fade to black runs before the loading screen takes over.
pub const LAUNCH_FADE: Duration = Duration::from_millis(600);

/// How long the connecting screen's hero pan takes to cross the whole image. Set
/// well past any plausible handshake so a real load only ever shows a slow drift.
pub const HERO_PAN: Duration = Duration::from_secs(75);

/// The hero's fade, in and back out to black again. Slower than `LAUNCH_FADE`: the card zoom is
/// a reaction to a button press and has to feel immediate, while this is a scene settling in —
/// and leaving it settles out on the same curve it arrived on.
pub const HERO_FADE: Duration = Duration::from_millis(1_300);

/// How long the hero stays up once it appears, [`HERO_FADE`] included. A floor, not a cap: a
/// slower handshake keeps it up longer. Long enough to read as a loading screen with a visible
/// pan rather than a flash. Only ever paid by a game that has wide art — with no hero to hold,
/// the hand-off is unchanged.
pub const HERO_MIN_SHOW: Duration = Duration::from_millis(2_700);

/// Longest the loading screen waits for a hero still being fetched off the host before handing
/// over without one (`app::hero`).
pub const HERO_ART_GRACE: Duration = Duration::from_millis(1_500);

/// Longest a launch waits for the first frame to reach the decoder — the one budget for it,
/// shared by the loading screen (`app::hero`) and the reveal that follows it
/// (`runtime::stream`). A host's first delivery can be seconds late (its startup capacity probe,
/// or a new UDP flow the AP holds — see `session`'s `PROBE_WARMUP_CAP`), and until it lands the
/// video plane is black, so the loading screen is what the user should be looking at.
pub const FIRST_FRAME_WAIT: Duration = Duration::from_secs(6);

/// Longest the loading screen runs before handing over regardless of the connect thread.
/// Only a backstop — `session::connect` has its own timeouts.
pub const HERO_LOADING_MAX: Duration = Duration::from_secs(30);

/// How much the hero is darkened once fully faded in, so it reads as a backdrop
/// rather than as content.
pub const HERO_SCRIM_ALPHA: f32 = 70.0;

/// Destination for the connecting screen's slow left-to-right pan: the hero scaled to
/// full screen height (so it is wider than the screen, by design — see the art loader's
/// `HERO_ASPECT`) and slid leftwards across that slack, off the edges of the target.
///
/// Subpixel on purpose. At this speed the image travels well under a pixel per frame, so
/// a whole-pixel destination would hold still for ten-odd frames and then jump; the
/// fractional offset plus bilinear filtering makes it a continuous drift instead.
///
/// Linear rather than eased — a constant drift reads as deliberate motion, while an
/// ease would visibly stall on a loading screen of unpredictable length.
pub fn hero_pan_dst(img_w: u32, img_h: u32, screen_w: u32, screen_h: u32, elapsed: Duration) -> RectF {
    let full = RectF {
        x: 0.0,
        y: 0.0,
        w: screen_w as f32,
        h: screen_h as f32,
    };
    if img_h == 0 || img_w == 0 {
        return full;
    }
    let scaled_w = img_w as f32 * (screen_h as f32 / img_h as f32);
    let slack = (scaled_w - screen_w as f32).max(0.0);
    let f = (elapsed.as_secs_f32() / HERO_PAN.as_secs_f32()).clamp(0.0, 1.0);
    RectF {
        x: -slack * f,
        w: scaled_w,
        ..full
    }
}

/// How far the launch's connect has got, as the loading screen can see it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Connect {
    /// Still running.
    Pending,
    /// Finished with a session; whether it is decoding yet is `presenting`.
    Done,
    /// Finished with an error, which the menu behind the hero is about to show.
    Failed,
}

#[derive(Default)]
pub(crate) struct Hero {
    /// The decoded image and the game id it belongs to. Several MB, and only one can ever
    /// be on screen, so a newer one simply replaces it.
    image: Option<(String, HeroImage)>,
    /// Last id asked for (the focused card), so an image that arrives after focus has
    /// moved on can be recognised as no longer useful.
    wanted: Option<String>,
    /// Id whose hero belongs on the connecting screen. `None` for Desktop.
    target: Option<String>,
    /// Whether this launch's hero is still in flight: the loading screen waits a moment for art on
    /// its way, but must not delay a launch with nothing to wait for.
    expected: bool,
    /// Id currently uploaded as `TileId::Hero`. Uploaded only once a launch starts —
    /// browsing must not put a multi-MB texture on the GPU.
    uploaded: Option<String>,
    /// When the uploaded image started fading in. Its own clock, not the launch fade's: a
    /// hero can land mid-handshake, and then it fades in from there.
    since: Option<Instant>,
    /// When it started fading back out, i.e. when the stream became ready.
    fade_out: Option<Instant>,
    /// When the wait for a decoded frame runs out, set the moment the connect finishes so the
    /// budget is bounded from there rather than from the launch. Handed to `runtime::stream`,
    /// whose reveal waits on the same signal and must not start the clock over.
    first_frame_deadline: Option<Instant>,
}

impl Hero {
    /// Notes whose hero is worth keeping — the focused card, whose art is prefetched.
    pub(crate) fn want(&mut self, game_id: &str) {
        // Called per tile-prep pass, so the usual no-op case allocates nothing.
        if self.wanted.as_deref() != Some(game_id) {
            self.wanted = Some(game_id.to_string());
        }
    }

    /// Arms the loading screen for a launch of `target` (`None` = Desktop). Nothing is waited
    /// for unless [`Self::await_art`] says so.
    pub(crate) fn arm(&mut self, target: Option<String>) {
        self.target = target;
        self.expected = false;
    }

    /// Says this launch's hero is still being fetched, so the hand-off gives it a moment to
    /// land. A game with none, or one whose hero is already in hand, must not spend that moment.
    pub(crate) fn await_art(&mut self) {
        self.expected = true;
    }

    /// Takes a freshly decoded image, and reports whether it was kept. A `false` means the
    /// caller should let the loader forget it, so focusing that card again re-requests it
    /// (from the disk cache by then) rather than never asking a second time.
    pub(crate) fn accept(&mut self, game_id: String, image: HeroImage) -> bool {
        let useful =
            self.wanted.as_deref() == Some(game_id.as_str()) || self.target.as_deref() == Some(game_id.as_str());
        if useful {
            self.image = Some((game_id, image));
        }
        useful
    }

    /// The id whose texture the launch needs but doesn't have yet, if any.
    pub(crate) fn pending_upload(&self) -> Option<String> {
        let target = self.target.as_deref()?;
        if self.uploaded.as_deref() == Some(target) {
            return None;
        }
        // Still fetching — the fade to black covers this, and the hero joins it whenever
        // it lands (possibly not at all, on a slow host).
        self.image
            .as_ref()
            .filter(|(loaded, _)| loaded == target)
            .map(|(loaded, _)| loaded.clone())
    }

    /// Records that `id`'s texture is now uploaded and starts its fade-in, returning any
    /// texture the caller should drop in exchange.
    pub(crate) fn mark_uploaded(&mut self, id: String) -> Option<String> {
        let stale = self.uploaded.replace(id);
        self.since = Some(Instant::now());
        stale
    }

    /// `id`'s decoded pixels, if they are the ones in hand.
    pub(crate) fn image_for(&self, id: &str) -> Option<&HeroImage> {
        self.image.as_ref().filter(|(loaded, _)| loaded == id).map(|(_, im)| im)
    }

    /// The decoded pixels behind the hero tile, for the upload itself.
    pub(crate) fn uploaded_image(&self) -> Option<&HeroImage> {
        let id = self.uploaded.as_deref()?;
        self.image.as_ref().filter(|(loaded, _)| loaded == id).map(|(_, im)| im)
    }

    /// The uploaded tile's pixels, once there is one to draw.
    pub(crate) fn visible(&self) -> Option<&HeroImage> {
        self.since?;
        self.uploaded_image()
    }

    /// How far into the pan the backdrop is.
    pub(crate) fn panned_for(&self) -> Duration {
        self.since.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// This frame's opacity factor, 0..=1: the fade-in, less the fade-out once that starts.
    pub(crate) fn opacity(&self) -> f32 {
        let out = self
            .fade_out
            .map_or(0.0, |t| ui::animation::anim_frac(Some(t), HERO_FADE));
        ui::animation::anim_frac(self.since, HERO_FADE) * (1.0 - out)
    }

    /// Whether an uploaded hero is on screen as the connecting backdrop. `since` is only ever
    /// written by `mark_uploaded`, so it implies `uploaded`.
    pub(crate) fn showing(&self) -> bool {
        self.since.is_some()
    }

    /// Whether the loading screen is finished, so the streaming loop can take the screen.
    /// Also what starts the fade-out, once everything else it waits on is satisfied.
    ///
    /// `presenting` is a frame having reached the decoder — NOT NDL's `PLAYING`, which lands
    /// during the load with nothing decoded. With a hero to pan, the screen waits for it so the
    /// fade-out is the last thing before the plane is uncovered; with none, `runtime::stream` holds
    /// the finished launch frame instead, on the same [`FIRST_FRAME_WAIT`] budget.
    pub(crate) fn handover_ready(&mut self, launch_elapsed: Duration, connect: Connect, presenting: bool) -> bool {
        if launch_elapsed < LAUNCH_FADE {
            return false;
        }
        // Capped whatever else is going on, so a connect that never returns can't strand
        // the app on a panning image.
        if launch_elapsed >= HERO_LOADING_MAX {
            return true;
        }
        // What the screen was waiting for has resolved, either way. A failure counts — it never
        // presents a frame, so waiting on `presenting` held every launch that *had* hero art to
        // the backstop before the error could be shown, while a game with none reported it at
        // the end of the fade. So does a handshake that lands and then decodes nothing.
        let settled = match connect {
            Connect::Failed => true,
            Connect::Done => presenting || self.first_frame_expired(),
            Connect::Pending => false,
        };
        if !self.showing() {
            // No hero to hold the wait: hand over at the end of the fade, and `runtime::stream`
            // keeps that finished frame up until the first frame arrives. A game that *has* wide
            // art gets a grace period first — on a cold cache the hero can still be a fetch away,
            // and would otherwise land just after the hand-off.
            return settled || !self.expected || launch_elapsed >= LAUNCH_FADE + HERO_ART_GRACE;
        }
        // Held until the launch resolves, then for the hero's own minimum and fade-out, so the
        // backdrop leaves the same way whether it runs into live video or into the error on the
        // menu behind it — never a hero cut mid-fade.
        settled && self.since.is_some_and(|t| t.elapsed() >= HERO_MIN_SHOW) && self.faded_out()
    }

    /// Starts the first-frame budget on the first call and reports whether it has run out.
    fn first_frame_expired(&mut self) -> bool {
        let deadline = *self
            .first_frame_deadline
            .get_or_insert_with(|| Instant::now() + FIRST_FRAME_WAIT);
        Instant::now() >= deadline
    }

    /// The deadline for `runtime::stream`'s reveal to carry on from — the same budget, not a
    /// second one.
    pub(crate) fn first_frame_deadline(&self) -> Option<Instant> {
        self.first_frame_deadline
    }

    /// Starts the fade-out (idempotent) and reports whether it has finished.
    fn faded_out(&mut self) -> bool {
        let since = *self.fade_out.get_or_insert_with(Instant::now);
        since.elapsed() >= HERO_FADE
    }
}
