//! Reusable transient toast notifications for the streaming overlay: a short message that
//! appears for a fixed hold then fades out. Styled like the stats overlay panel (same glass
//! background/radius, see [`render_notification_tile`]); the fade reuses the app-wide
//! [`anim_frac`] easing so it matches the modals' curve. One slot — a new [`Notification::show`]
//! replaces whatever is on screen.

use crate::ui::prelude::*;
use anyhow::Result;
use std::time::{Duration, Instant};

/// Fully opaque for this long before the fade begins.
const HOLD: Duration = Duration::from_secs(2);

/// A single-slot transient toast. The message is owned so callers don't have to keep it
/// alive across ticks; [`Notification::frame`] drives both the fade alpha and expiry.
#[derive(Default)]
pub struct Notification {
    active: Option<(String, Instant)>,
}

impl Notification {
    pub fn new() -> Self {
        Self { active: None }
    }

    /// Show `text` from now: full opacity for [`HOLD`], then an [`OVERLAY_FADE`] fade.
    pub fn show(&mut self, text: impl Into<String>) {
        self.active = Some((text.into(), Instant::now()));
    }

    /// `(text, alpha)` to draw this tick, or `None` once fully faded. Clears the slot on
    /// expiry, so a return flipping to `None` marks the on→off edge for the caller.
    pub fn frame(&mut self) -> Option<(String, f32)> {
        let shown = self.active.as_ref()?.1;
        let elapsed = shown.elapsed();
        if elapsed >= HOLD + OVERLAY_FADE {
            self.active = None;
            return None;
        }
        let alpha = if elapsed < HOLD {
            1.0
        } else {
            (1.0 - anim_frac(Some(shown + HOLD), OVERLAY_FADE)).clamp(0.0, 1.0)
        };
        Some((self.active.as_ref()?.0.clone(), alpha))
    }
}

/// Padding inside the notification panel.
const NOTIFICATION_PAD: i32 = 18;

/// Corner radius of the notification panel.
const NOTIFICATION_RADIUS: i32 = 14;

/// Single-line notification panel, styled like the stats overlay's glass background.
pub struct NotificationTile<'a> {
    pub font: FontId,
    pub text: &'a str,
}

impl Widget for NotificationTile<'_> {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()> {
        // The same glass every menu surface is cut from. It used to mix its own near-black
        // fill — the menu's own background hue — which made it a same-colour box on that
        // screen and needed a much brighter stroke to stay legible. The shared fill sits
        // clearly above the background on its own, so the shared hairline is enough.
        c.painter
            .glass_face(area, NOTIFICATION_RADIUS, crate::ui::theme::glass_fill());
        c.text(self.font, self.text, NOTIFICATION_PAD, NOTIFICATION_PAD, palette().text)?;
        Ok(())
    }
}

impl TileWidget for NotificationTile<'_> {
    fn size(&self, fonts: &Fonts) -> (u32, u32) {
        let pad = 2 * NOTIFICATION_PAD;
        (
            fonts.raster.measure(self.font, self.text).0 + pad as u32,
            (fonts.raster.height(self.font) + pad) as u32,
        )
    }
}
