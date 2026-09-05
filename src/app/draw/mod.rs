//! The pointer UI's screens on the console kit (`webos-pointer-ui-overhaul.md` WP3).
//!
//! Immediate mode: a screen in [`ported`] draws itself on the frame's canvas every tick, from
//! app state, with `pf_console_ui`'s `theme` and `widgets`. Its geometry is one `layout` per
//! screen that the pointer hit tests call too, so what is drawn is what is hit. Everything
//! else still arrives as tiles through `render::skia` until its turn.
//!
//! Sizes are the console's design units, scaled by [`Frame::k`] — the same rule
//! (`height / 800`) the shell applies, so a row here is a row there.

pub(crate) mod dialog;

use pf_console_ui::theme::{self, Fonts, PanelStroke, W};
use skia_safe::canvas::SaveLayerRec;
use skia_safe::{image_filters, Canvas, ClipOp, RRect, Rect, TileMode};

use crate::app::App;
use crate::core::screen::Screen;
use crate::ui;

/// Which screens draw here rather than as tiles. Every prepare, compose and hit-test path
/// asks this, so a screen moves over by being added to this list and nowhere else.
pub(crate) const fn ported(screen: Screen) -> bool {
    matches!(
        screen,
        Screen::ForgetHost
            | Screen::SendLogs
            | Screen::RemoveCollection
            | Screen::ResetHdrCalibration
            | Screen::ResetGameSettings
    )
}

/// What every draw fn takes.
pub(crate) struct Frame<'a> {
    pub canvas: &'a Canvas,
    pub fonts: &'a Fonts,
    /// Layout size, in the units pointer events arrive in.
    pub w: f32,
    pub h: f32,
    /// Pixels per design unit.
    pub k: f32,
}

impl<'a> Frame<'a> {
    pub fn new(canvas: &'a Canvas, fonts: &'a Fonts, w: u32, h: u32) -> Self {
        Self {
            canvas,
            fonts,
            w: w as f32,
            h: h as f32,
            k: scale(h),
        }
    }
}

/// The console's `Viewport` default: 800 design units tall, clamped between a Deck and a 4K
/// panel. 1.35 at 1080p.
pub(crate) fn scale(h: u32) -> f32 {
    (h as f32 / 800.0).clamp(0.75, 3.0)
}

/// Geist's line box at `size`: the ascent-to-descent span comes out near 1.25 em.
pub(crate) fn line_h(size: f64) -> f64 {
    size * 1.25
}

/// Greedy word wrap on the kit's single-line measure, in device pixels. A word wider than
/// `max_w` stands alone and overflows rather than being split mid-word.
pub(crate) fn wrap(fonts: &Fonts, text: &str, w: W, size: f64, max_w: f64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if line.is_empty() || f64::from(fonts.measure(&candidate, w, size)) <= max_w {
            line = candidate;
        } else {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Blur sigma under a raised card, design units. Wide enough that a grid of covers reads as
/// a wash rather than as shapes, which is what the frosted look was.
const GLASS_BLUR: f32 = 14.0;

/// A raised glass card: the frame under `rect` blurred, then the kit's panel over it. The
/// blur is what the SDL compositor's frost chain was; the panel is every console surface.
pub(crate) fn glass_card(canvas: &Canvas, rect: Rect, corner: f32, k: f32) {
    let rr = RRect::new_rect_xy(rect, corner * k, corner * k);
    if let Some(blur) = image_filters::blur((GLASS_BLUR * k, GLASS_BLUR * k), TileMode::Clamp, None, None) {
        canvas.save();
        canvas.clip_rrect(rr, ClipOp::Intersect, true);
        canvas.save_layer(&SaveLayerRec::default().bounds(&rect).backdrop(&blur));
        canvas.restore();
        canvas.restore();
    }
    theme::panel(canvas, rect, corner, None, PanelStroke::Gradient, k);
}

/// The Lucide mark for one of this app's Material glyphs (plan §5), for the screens drawn
/// here while `view::icons` still speaks Material. Dies with that table in WP7.
pub(crate) fn lucide_for(material: &str) -> Option<&'static str> {
    use crate::app::view::icons as m;
    Some(match material {
        x if x == m::ICON_DELETE => "trash-2",
        x if x == m::ICON_SEND => "send",
        x if x == m::ICON_POWER => "power",
        x if x == m::ICON_SIGNAL => "activity",
        x if x == m::ICON_CHECK => "check",
        _ => return None,
    })
}

/// An `ui::render::Rect` as Skia's.
pub(crate) fn sk(r: ui::render::Rect) -> Rect {
    Rect::from_xywh(r.x() as f32, r.y() as f32, r.width() as f32, r.height() as f32)
}

/// A Skia rect as the app's integer one, for the hit tests.
pub(crate) fn ui_rect(r: Rect) -> ui::render::Rect {
    ui::render::Rect::new(
        r.left.round() as i32,
        r.top.round() as i32,
        r.width().round().max(0.0) as u32,
        r.height().round().max(0.0) as u32,
    )
}

impl App {
    /// The ported modal layer, drawn after the tiles: the open card at its open alpha and
    /// rise, and the card being left at its closing alpha — the same cross-fade
    /// `render::compose` plays for tiles, from state instead of from a snapshot.
    pub(crate) fn draw_modals(&self, f: &Frame<'_>) {
        let screen = self.nav.screen;
        let m = if matches!(screen, Screen::Home) {
            0.0
        } else {
            self.render.modal.fade.open_alpha()
        };
        if let Some((alpha, left)) = self.render.modal.fade.closing_frame_against(m) {
            if ported(left) {
                self.draw_modal_screen(f, left, alpha, false);
            }
        }
        if ported(screen) && m > 0.0 {
            self.draw_modal_screen(f, screen, m, true);
        }
    }

    /// One ported card. `live` is whether it is the screen the cursor is on: a card on its
    /// way out keeps its last focus but takes no pop and no press.
    fn draw_modal_screen(&self, f: &Frame<'_>, screen: Screen, alpha: f32, live: bool) {
        let dy = ui::animation::modal_rise(alpha) as f32;
        let (Some(title), Some(confirm)) = (dialog::title_of(screen), self.confirm_for(screen)) else {
            return;
        };
        let focus = self.nav.cursor(crate::app::nav::ScreenKey::of(screen));
        let motion = live.then_some(dialog::Motion {
            focus_anim: self.render.modal.focus_anim,
            press: self.press_dip(screen),
            hover_close: self.render.hover_close,
        });
        dialog::draw(f, title, &confirm, focus, motion.as_ref(), alpha, dy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_follows_the_consoles_rule() {
        assert!((scale(1080) - 1.35).abs() < 1e-6);
        assert!((scale(2160) - 2.7).abs() < 1e-6);
        assert!((scale(400) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn wrap_breaks_on_words_and_never_loses_one() {
        let fonts = theme::build_fonts().unwrap();
        let text = "Alpha will be removed from this TV. You can pair with it again later.";
        let lines = wrap(&fonts, text, W::Regular, 16.0, 220.0);
        assert!(lines.len() > 1, "{lines:?}");
        assert_eq!(lines.join(" "), text);
        for l in &lines {
            assert!(f64::from(fonts.measure(l, W::Regular, 16.0)) <= 220.0 || !l.contains(' '), "{l}");
        }
        assert_eq!(wrap(&fonts, "", W::Regular, 16.0, 100.0), vec![String::new()]);
    }
}
