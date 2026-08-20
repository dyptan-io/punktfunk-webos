//! The settings modal — presentation: row list layout, dropdown overlay geometry, shell.
//! Logic lives in `app::state::settings`.
use crate::app::menu;
use crate::app::menu::SettingsScope;
use crate::core::model;
use crate::core::VERSION;
use crate::services::store::{GamepadType, Settings, VideoBackend};
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) const TITLE: &str = "Settings";

/// Card space above the row list: title, divider, and their padding.
pub(crate) const CHROME_TOP: u32 = 120;

/// Card space below the row list. The high-bitrate caution rides on the Bitrate row itself
/// (see [`rows`]), so no extra chrome is reserved for it — just enough to clear the card's rounded corner, so the
/// list runs to the card's edge and the bottom fade dissolves into it.
///
/// Anything more shows as a band of flat card background under the fade — the fade already
/// *is* the bottom edge, so padding beneath it reads as dead space rather than breathing room.
pub(crate) const CHROME_BOTTOM: u32 = 16;

/// Minimum gap between the settings card and the screen edges, top and bottom combined.
///
/// Trimmed from 160 when the second peek strip arrived: two 44px peeks cost a whole visible
/// row out of a 1080p budget, and the card had more inset to spare than the list had rows.
pub(crate) const EDGE_MARGIN: u32 = 120;

/// How much of the adjacent row stays visible past each edge of the viewport while the list
/// overflows — the strip an edge fade dissolves. Applied to the top and bottom alike.
///
/// Load-bearing, not decoration: a viewport edge landing exactly on a row boundary has
/// nothing but card background in its outermost pixels (unfocused rows draw no fill of their
/// own), so a fade there blends the card colour into the card colour and is *mathematically
/// invisible*. Both cuts have to land mid-row for either fade to read at all — which is also
/// why the rendered offset is biased by one peek (see `App::sync_modal_scroll`) instead of
/// sitting on the row grid.
///
/// Independent of `ui::widgets::SCROLL_FADE_H`, which is taller: this is how much of the next row is
/// *exposed*, while that is how far the fade reaches back over what is already visible. Deep
/// enough to expose a row's icon and label, which sit in the middle third of its height — a
/// shallower peek shows only the row's internal padding, i.e. nothing to dissolve.
pub(crate) const PEEK: u32 = 44;

/// The caption a locked row carries: what fixed its value, and where to go to change it.
///
/// Lives on the row that is *immutable*, not on the one that caused it — the greyed control is
/// what the user is looking at when they want the reason.
///
/// `webos_major` is the OS major (`None` where it couldn't be read); named only where the OS is
/// the whole story, i.e. where there's no backend to switch to either.
fn lock_caption(lock: menu::RowLock, webos_major: Option<u32>) -> String {
    // Where the Video backend row exists, the limit is the *pick*, not the TV, and SMP lifts it —
    // so the caption points at the fix instead of at a version number the user can't change.
    let source = || {
        if crate::core::caps::smp_selectable() {
            "the NDL backend — try SMP".to_string()
        } else {
            match webos_major {
                Some(major) => format!("webOS {major}"),
                None => "this TV".to_string(),
            }
        }
    };
    match lock {
        menu::RowLock::HdrNeedsHevc => "HDR is not supported by H.264".to_string(),
        menu::RowLock::NoHdr => format!("HDR is not supported by {}", source()),
        menu::RowLock::OneCodec => format!("H.264 is the only codec supported by {}", source()),
        menu::RowLock::StereoOnly => format!("Stereo is the only audio supported by {}", source()),
        menu::RowLock::NoGamepad => "Connect a controller to your TV".to_string(),
    }
}

/// One row per `menu::ROW_*`, in order, filtered by `menu::settings_visible_logical_rows`
/// (so `set` decides which list this is) and greyed by `menu::row_lock` (whose reason becomes the row's caption, see [`lock_caption`]).
///
/// `detected_gamepad_type` is the attached pad per `gamepad::detect_type`, `None` with nothing
/// attached or an unrecognized pad — it only changes what "Automatic" reads as.
///
/// `dualsense_limited`: the *effective* controller type (the explicit pick, or on `Auto`
/// whatever's actually plugged in) is a `DualSense`/`Edge` and the TV's kernel isn't running
/// `hid-playstation` — see `platform::webos::dualsense::hid_playstation_bound`. Computed by
/// the caller, like `webos_major`, so this module stays platform-neutral.
pub(crate) fn rows(
    set: SettingsScope,
    settings: &Settings,
    detected_gamepad_type: Option<GamepadType>,
    dualsense_limited: bool,
    webos_major: Option<u32>,
) -> Vec<FocusRow> {
    let bitrate_frac = if settings.bitrate_kbps == model::BITRATE_AUTOMATIC {
        0.0
    } else {
        (settings.bitrate_kbps.saturating_sub(model::BITRATE_MIN_KBPS)) as f32
            / (model::BITRATE_MAX_KBPS - model::BITRATE_MIN_KBPS) as f32
    };
    let row_for = |logical: usize| match logical {
        menu::ROW_RESOLUTION => FocusRow::dropdown(
            crate::app::view::icons::ICON_MONITOR,
            "Resolution",
            menu::resolution_label(settings.width, settings.height),
        ),
        menu::ROW_FRAMERATE => FocusRow::dropdown(
            crate::app::view::icons::ICON_SCHEDULE,
            "Frame rate",
            format!("{} Hz", settings.refresh_hz),
        ),
        menu::ROW_BITRATE => FocusRow::slider(
            crate::app::view::icons::ICON_SIGNAL,
            "Bitrate",
            if settings.bitrate_kbps == model::BITRATE_AUTOMATIC {
                "Automatic".to_string()
            } else {
                format!("{} Mbps", settings.bitrate_kbps / 1000)
            },
            bitrate_frac,
        )
        .with_subtext_opt(
            (settings.bitrate_kbps > menu::BITRATE_WARN_KBPS)
                .then(|| ui::widgets::RowSubtext::caution("May be unstable on Wi-Fi — try Ethernet")),
        ),
        menu::ROW_VIDEO_BACKEND => FocusRow::dropdown(
            crate::app::view::icons::ICON_MEMORY,
            "Video backend",
            match settings.video_backend {
                VideoBackend::Ndl => "NDL",
                VideoBackend::Smp => "SMP",
            },
        ),
        menu::ROW_CODEC => FocusRow::dropdown(
            crate::app::view::icons::ICON_MOVIE,
            "Codec",
            menu::codec_label(settings.codec),
        ),
        menu::ROW_HDR => FocusRow::toggle(crate::app::view::icons::ICON_SUN, "HDR", settings.hdr_enabled),
        menu::ROW_AUDIO => FocusRow::dropdown(
            crate::app::view::icons::ICON_SIGNAL,
            "Audio",
            menu::audio_label(settings.audio_channels),
        ),
        menu::ROW_GAMEPAD => FocusRow::dropdown(
            crate::app::view::icons::ICON_GAMEPAD,
            "Controller",
            if settings.gamepad_type == GamepadType::Auto {
                menu::gamepad_auto_label(detected_gamepad_type)
            } else {
                menu::gamepad_label(settings.gamepad_type).to_string()
            },
        )
        .with_subtext_opt(
            dualsense_limited.then(|| ui::widgets::RowSubtext::caution("Limited support by your WebOS version")),
        ),
        menu::ROW_CURSOR => FocusRow::action(crate::app::view::icons::ICON_MOUSE, "Cursor"),
        menu::ROW_EXPERIMENTAL => FocusRow::action(crate::app::view::icons::ICON_BUG, "Experimental"),
        menu::ROW_DIAGNOSTICS => FocusRow::action(crate::app::view::icons::ICON_WRENCH, "Diagnostics"),
        // The build version rides along as this row's value, so it's visible without
        // opening the screen — matching where the other clients surface it.
        menu::ROW_ABOUT => FocusRow::action_with_value(
            crate::app::view::icons::ICON_INFO,
            "About & licenses",
            format!("v{VERSION}"),
        ),
        // Marked destructive: it discards the whole screen's worth of choices in one press,
        // and the red reads as that before it's pressed rather than after. Per-game only —
        // the global list has nothing to fall back to (see `menu::ROW_RESET`).
        menu::ROW_RESET => FocusRow::action(crate::app::view::icons::ICON_DELETE, "Reset")
            .danger()
            .with_subtext(ui::widgets::RowSubtext::hint("Use global settings for this game")),
        // `ROW_CURSOR_CAPTURE`/`ROW_CURSOR_GESTURES` are logical ids only — both screens reach
        // them through the Cursor link above, so they are on neither list and never asked for.
        // Anything else means a row list gained an id without an arm here: fail loudly in
        // debug rather than render a blank, focusable row.
        _ => {
            debug_assert!(false, "no settings row built for logical {logical}");
            FocusRow::action("", "")
        }
    };
    // Driven by the shared predicates rather than repeating their conditions, so a row hidden or
    // locked there can never disagree here.
    menu::settings_visible_logical_rows(set)
        .map(|logical| {
            let row = row_for(logical);
            match menu::row_lock(logical, settings, detected_gamepad_type) {
                // The lock's caption replaces whatever contextual one the row carried: a row the
                // user can't change has nothing more useful to say than why.
                Some(lock) => row
                    .locked(true)
                    .with_subtext(ui::widgets::RowSubtext::hint(lock_caption(lock, webos_major))),
                None => row,
            }
        })
        .collect()
}

/// How many rows are *fully* visible. Capped at the live row count so a hidden row (the Video
/// backend row off webOS 3.5-4.x) leaves no empty slot.
///
/// When the list overflows, one row's worth of budget is spent on [`PEEK`] instead —
/// the partially-visible sliver the bottom fade dissolves. Computed without the peek first,
/// because a list that fits entirely has nothing below to peek at and should not give up
/// the space.
pub(crate) fn visible_rows(set: SettingsScope, screen_h: u32) -> usize {
    let stride = ui::widgets::focus_row_stride();
    let total = menu::settings_row_count(set);
    let budget = screen_h.saturating_sub(CHROME_TOP + CHROME_BOTTOM + EDGE_MARGIN);
    if (budget / stride) as usize >= total {
        return total.max(1);
    }
    // Both peeks come out of the budget, not just the bottom one — see [`PEEK`].
    ((budget.saturating_sub(2 * PEEK) / stride) as usize).clamp(1, total)
}

/// Height of the scrolling viewport: the fully-visible rows plus a peek strip past each
/// edge while the list overflows. Deliberately *not* a whole multiple of the row stride
/// when scrolling — see [`PEEK`].
pub(crate) fn content_h(set: SettingsScope, screen_h: u32) -> u32 {
    let visible = visible_rows(set, screen_h);
    let peeks = if visible < menu::settings_row_count(set) {
        2 * PEEK
    } else {
        0
    };
    visible as u32 * ui::widgets::focus_row_stride() + peeks
}

/// Width the title line keeps clear on the right for the close button, which overhangs the
/// content column (see `ui::widgets::modal_close_rect`).
const CLOSE_RESERVE: u32 = 48;

/// Left/right inset of the card's own content column — the title, the rule and the row
/// list all start here.
pub(crate) const SIDE_PAD: u32 = 40;

/// Card and content rects, shared by render and hit-test. One split, read twice: the card's
/// height is what its own stack adds up to, and the viewport is the middle slot of it.
pub(crate) fn layout(set: SettingsScope, screen_w: u32, screen_h: u32) -> (Rect, Rect) {
    let stack = ui::layout::Layout::vertical([
        ui::layout::Constraint::Length(CHROME_TOP),
        ui::layout::Constraint::Length(content_h(set, screen_h)),
        ui::layout::Constraint::Length(CHROME_BOTTOM),
    ]);
    // Widened from 0.56 to fit the scroll indicator on the right edge.
    let card = ui::widgets::modal_card_rect(screen_w, screen_h, 0.62, stack.total_length());
    (card, content_column(stack.split(card)[1]))
}

/// The horizontal inset every element of the card shares.
fn content_column(row: Rect) -> Rect {
    row.inset_x(SIDE_PAD)
}

/// Where a dropdown opened from row `row` anchors its option overlay — one row below it.
///
/// Positioned from a pixel scroll offset rather than a viewport-local row index, since a
/// gliding list puts its rows at continuous offsets. `scroll_px` of 0 is the unscrolled
/// case (Diagnostics).
pub(crate) fn dropdown_overlay_rect_at_px(content: Rect, row: usize, scroll_px: i32) -> Rect {
    let y = ui::widgets::focus_row_rect_at_px(content, row + 1, scroll_px).y();
    Rect::new(content.x(), y, content.width(), 0)
}

/// Radius of the dot separating the heading from the per-game title suffix, and the gap it
/// keeps on each side.
const SEP_DOT_R: i32 = 3;
const SEP_DOT_GAP: i32 = 12;

/// The shell only: card chrome, title and rule. The row list is its own scroll-content
/// tile and the open dropdown its own overlay tile, so neither scrolling nor navigating
/// options re-rasterizes this.
///
/// `suffix` is appended after the title in the muted text colour — the per-game screen names
/// its game there, dimmer so "Settings" still reads as the heading.
pub(crate) fn render(c: &mut Canvas, set: SettingsScope, suffix: Option<&str>, hover_close: bool) -> Result<()> {
    let (card, _content) = layout(set, c.screen_w, c.screen_h);
    let column = content_column(card);
    let baseline = card.y() + 36;
    c.modal_shell(card, hover_close)?;
    c.text(c.fonts.label, TITLE, column.x(), baseline, ui::style::theme().text)?;
    if let Some(suffix) = suffix {
        let font = c.fonts.label;
        let title_w = c.fonts.raster.measure(font, TITLE).0;
        // A dot between the heading and the game name: at this size a space reads as a line
        // break waiting to happen, and the two words are not one phrase.
        let dot_x = column.x() + title_w as i32 + SEP_DOT_GAP;
        c.painter.fill_rounded_rect(
            Rect::new(
                dot_x,
                baseline + c.fonts.raster.height(font) / 2 - SEP_DOT_R,
                2 * SEP_DOT_R as u32,
                2 * SEP_DOT_R as u32,
            ),
            SEP_DOT_R,
            ui::style::theme().muted,
        );
        let used = title_w + (2 * SEP_DOT_GAP + 2 * SEP_DOT_R) as u32;
        // Faded rather than clipped: a long game name must not run under the close button.
        let avail = column.width().saturating_sub(used + CLOSE_RESERVE);
        c.text_faded(
            font,
            suffix,
            column.x() + used as i32,
            baseline,
            avail,
            ui::style::theme().muted,
        )?;
    }
    c.painter.rule(column.x(), card.y() + 88, column.width());
    Ok(())
}

/// The settings modal as a [`ModalScreen`]. `game` names the per-game variant's title suffix;
/// `None` is the global screen.
pub(crate) struct Modal<'a> {
    pub set: SettingsScope,
    pub game: Option<&'a str>,
}

impl ModalScreen for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, _fonts: &ui::text::Fonts) -> Rect {
        layout(self.set, screen_w, screen_h).0
    }

    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        render(c, self.set, self.game, hover_close)
    }
}
