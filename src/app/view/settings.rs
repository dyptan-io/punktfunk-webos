//! The settings modal — presentation: row list layout, dropdown overlay geometry, shell.
//! Logic lives in `app::state::settings`.
use crate::app::menu;
use crate::core::VERSION;
use crate::services::store::{CodecPref, GamepadType, Settings, VideoBackend};
use crate::ui::render::Rect;
use crate::ui::{self, Canvas, FocusRow};
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
/// Independent of `ui::SCROLL_FADE_H`, which is taller: this is how much of the next row is
/// *exposed*, while that is how far the fade reaches back over what is already visible. Deep
/// enough to expose a row's icon and label, which sit in the middle third of its height — a
/// shallower peek shows only the row's internal padding, i.e. nothing to dissolve.
pub(crate) const PEEK: u32 = 44;

/// One row per `menu::ROW_*`, in order, filtered by `menu::row_shown`.
///
/// `detected_gamepad_type` is the attached pad per `gamepad::detect_type`, `None` with nothing
/// attached or an unrecognized pad — it only changes what "Automatic" reads as.
///
/// `dualsense_limited`: the *effective* controller type (the explicit pick, or on `Auto`
/// whatever's actually plugged in) is a `DualSense`/`Edge` and the TV's kernel isn't running
/// `hid-playstation` — see `platform::webos::dualsense::hid_playstation_bound`. Computed by
/// the caller, like `webos_major` (the OS major named in the Video backend row's caption,
/// `None` where it couldn't be read), so this module stays platform-neutral.
pub(crate) fn rows(
    settings: &Settings,
    detected_gamepad_type: Option<GamepadType>,
    dualsense_limited: bool,
    webos_major: Option<u32>,
) -> Vec<FocusRow> {
    let bitrate_frac = if settings.bitrate_kbps == menu::BITRATE_AUTOMATIC {
        0.0
    } else {
        (settings.bitrate_kbps.saturating_sub(menu::BITRATE_MIN_KBPS)) as f32
            / (menu::BITRATE_MAX_KBPS - menu::BITRATE_MIN_KBPS) as f32
    };
    let rows = vec![
        FocusRow::dropdown(
            ui::ICON_MONITOR,
            "Resolution",
            menu::resolution_label(settings.width, settings.height),
        ),
        FocusRow::dropdown(ui::ICON_SCHEDULE, "Frame rate", format!("{} Hz", settings.refresh_hz)),
        FocusRow::slider(
            ui::ICON_SIGNAL,
            "Bitrate",
            if settings.bitrate_kbps == menu::BITRATE_AUTOMATIC {
                "Automatic".to_string()
            } else {
                format!("{} Mbps", settings.bitrate_kbps / 1000)
            },
            bitrate_frac,
        )
        .with_subtext_opt(
            (settings.bitrate_kbps > menu::BITRATE_WARN_KBPS)
                .then(|| ui::RowSubtext::caution("May be unstable on Wi-Fi — try Ethernet")),
        ),
        FocusRow::dropdown(
            ui::ICON_MEMORY,
            "Video backend",
            match settings.video_backend {
                VideoBackend::Ndl => "NDL",
                VideoBackend::Smp => "SMP",
            },
        )
        .with_subtext_opt((settings.video_backend == VideoBackend::Ndl).then(|| {
            ui::RowSubtext::caution(match webos_major {
                Some(major) => format!("Limitted HDR support on webOS {major}"),
                None => String::new(),
            })
        })),
        FocusRow::dropdown(ui::ICON_MOVIE, "Codec", menu::codec_label(settings.codec)).with_subtext_opt(
            (settings.codec == CodecPref::H264).then(|| ui::RowSubtext::hint("HDR is not supported with this codec")),
        ),
        FocusRow::toggle(ui::ICON_SUN, "HDR", settings.hdr_enabled),
        FocusRow::dropdown(ui::ICON_SIGNAL, "Audio", menu::audio_label(settings.audio_channels)),
        FocusRow::dropdown(
            ui::ICON_GAMEPAD,
            "Controller",
            if settings.gamepad_type == GamepadType::Auto {
                menu::gamepad_auto_label(detected_gamepad_type)
            } else {
                menu::gamepad_label(settings.gamepad_type).to_string()
            },
        )
        .with_subtext_opt(dualsense_limited.then(|| ui::RowSubtext::caution("Limited support by your WebOS version"))),
        FocusRow::action(ui::ICON_MOUSE, "Cursor"),
        FocusRow::action(ui::ICON_BUG, "Experimental"),
        FocusRow::action(ui::ICON_WRENCH, "Diagnostics"),
        // The build version rides along as this row's value, so it's visible without
        // opening the screen — matching where the other clients surface it.
        FocusRow::action_with_value(ui::ICON_INFO, "About & licenses", format!("v{VERSION}")),
    ];
    debug_assert_eq!(
        rows.len(),
        menu::SETTINGS_ROW_COUNT,
        "one row per logical index, in order"
    );
    // Driven by the one visibility predicate rather than repeating its conditions, so a row hidden
    // there can never linger here. Index == logical row, guaranteed by the assert above.
    rows.into_iter()
        .enumerate()
        .filter(|(row, _)| menu::row_shown(*row, settings))
        .map(|(_, row)| row)
        .collect()
}

/// How many rows are *fully* visible. Capped at the live row count so a hidden row (HDR on
/// an explicit H.264 pick) leaves no empty slot.
///
/// When the list overflows, one row's worth of budget is spent on [`PEEK`] instead —
/// the partially-visible sliver the bottom fade dissolves. Computed without the peek first,
/// because a list that fits entirely has nothing below to peek at and should not give up
/// the space.
pub(crate) fn visible_rows(settings: &Settings, screen_h: u32) -> usize {
    let stride = ui::focus_row_stride();
    let total = menu::settings_row_count(settings);
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
pub(crate) fn content_h(settings: &Settings, screen_h: u32) -> u32 {
    let visible = visible_rows(settings, screen_h);
    let peeks = if visible < menu::settings_row_count(settings) {
        2 * PEEK
    } else {
        0
    };
    visible as u32 * ui::focus_row_stride() + peeks
}

/// Card and content rects, shared by render and hit-test.
pub(crate) fn layout(settings: &Settings, screen_w: u32, screen_h: u32) -> (Rect, Rect) {
    let content_h = content_h(settings, screen_h);
    let card_h = content_h + CHROME_TOP + CHROME_BOTTOM;
    // Widened from 0.56 to fit the scroll indicator on the right edge.
    let card = ui::modal_card_rect(screen_w, screen_h, 0.62, card_h);
    let content = Rect::new(
        card.x() + 40,
        card.y() + CHROME_TOP as i32,
        card.width().saturating_sub(80),
        content_h,
    );
    (card, content)
}

/// Where a dropdown opened from row `row` anchors its option overlay — one row below it.
///
/// Positioned from a pixel scroll offset rather than a viewport-local row index, since a
/// gliding list puts its rows at continuous offsets. `scroll_px` of 0 is the unscrolled
/// case (Diagnostics).
pub(crate) fn dropdown_overlay_rect_at_px(content: Rect, row: usize, scroll_px: i32) -> Rect {
    let y = ui::focus_row_rect_at_px(content, row + 1, scroll_px).y();
    Rect::new(content.x(), y, content.width(), 0)
}

/// The shell only: card chrome, title and rule. The row list is its own scroll-content
/// tile and the open dropdown its own overlay tile, so neither scrolling nor navigating
/// options re-rasterizes this.
pub(crate) fn render(c: &mut Canvas, settings: &Settings, hover_close: bool) -> Result<()> {
    let (card, _content) = layout(settings, c.screen_w, c.screen_h);
    ui::draw_modal_shell(c.painter, c.text_cache, c.fonts, card, hover_close)?;
    ui::draw_text(
        c.painter,
        c.text_cache,
        c.fonts.raster,
        c.fonts.label,
        TITLE,
        card.x() + 40,
        card.y() + 36,
        ui::WHITE,
    )?;
    ui::draw_rule(c.painter, card.x() + 40, card.y() + 88, card.width().saturating_sub(80));
    Ok(())
}
