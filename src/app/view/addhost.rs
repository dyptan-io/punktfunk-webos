//! The host address form — presentation, shared by Add host and Edit address. Logic lives
//! in `app::state::addhost` / `app::state::edithost`.
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, Fonts};
use anyhow::Result;

pub(crate) const ADD_TITLE: &str = "Add host";
pub(crate) const EDIT_TITLE: &str = "Edit address";
pub(crate) const ADD_SUBTITLE: &str = "Enter the host's IP address. Right adds an optional port.";

/// Edit gets its own subtitle rather than reusing the Add one, which would overflow the
/// card once the host's name is in it.
pub(crate) fn edit_subtitle(host_name: &str) -> String {
    format!("New IP address for {host_name}. Its pairing is kept.")
}

/// Lifted clear of the on-screen keyboard when it's up.
pub(crate) fn card_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str, keyboard_shown: bool) -> Rect {
    ui::simple_modal_card_above_keyboard(screen_w, screen_h, keyboard_shown, |probe| {
        let header_end = ui::modal_header_end_y(fonts.raster, fonts.label, fonts.value, probe, subtitle);
        (header_end + 20 + 80 + 32) as u32 // field + bottom margin
    })
}

/// The text field, also handed to `SDL_SetTextInputRect` (which the webOS OSK ignores).
pub(crate) fn field_rect(screen_w: u32, screen_h: u32, fonts: &Fonts, subtitle: &str, keyboard_shown: bool) -> Rect {
    let card = card_rect(screen_w, screen_h, fonts, subtitle, keyboard_shown);
    let after_subtitle_y = ui::modal_header_end_y(fonts.raster, fonts.label, fonts.value, card, subtitle);
    Rect::new(
        card.x() + 32,
        after_subtitle_y + 20,
        card.width().saturating_sub(64),
        80,
    )
}

pub(crate) fn render(
    c: &mut Canvas,
    title: &str,
    subtitle: &str,
    typed: &str,
    keyboard_shown: bool,
    hover_close: bool,
) -> Result<()> {
    let card = card_rect(c.screen_w, c.screen_h, c.fonts, subtitle, keyboard_shown);
    ui::draw_modal_shell(c.painter, c.text_cache, c.fonts, card, hover_close)?;
    let after_subtitle_y = ui::draw_modal_header(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.label,
        c.fonts.value,
        card,
        title,
        ui::WHITE,
        subtitle,
        ui::MUTED,
    )?;
    let field = Rect::new(
        card.x() + 32,
        after_subtitle_y + 20,
        card.width().saturating_sub(64),
        80,
    );
    let drawn = ui::draw_card(c.painter, field, true);
    let text_x = drawn.x() + 24;
    let text_w = c.fonts.raster.measure(c.fonts.title, typed).0;
    ui::draw_text(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.title,
        typed,
        text_x,
        drawn.y() + (drawn.height() as i32 - c.fonts.raster.height(c.fonts.title)) / 2,
        ui::WHITE,
    )?;
    // A blinkless text-cursor bar right after what's typed so far — there's no fixed-width
    // mask anymore to show *where* editing happens, so this stands in for it.
    let caret = Rect::new(
        text_x + text_w as i32 + 6,
        drawn.y() + 16,
        3,
        drawn.height().saturating_sub(32),
    );
    c.painter.fill_rect(caret, ui::ACCENT_BRIGHT);
    Ok(())
}
