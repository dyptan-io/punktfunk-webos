//! The settings modal — presentation: row list layout, dropdown overlay geometry, shell.
//! Logic lives in `app::state::settings`.
use crate::app::menu;
use crate::app::menu::SettingsScope;
use crate::app::view::scrolllist;
use crate::core::model;
use crate::core::VERSION;
use crate::services::store::{AudioRoutePref, GamepadType, Settings, VideoBackend};
use crate::ui;
use crate::ui::render::Rect;
use crate::ui::widgets::FocusRow;
use crate::ui::Canvas;
use crate::ui::ModalMetrics;
use crate::ui::ModalScreen;
use anyhow::Result;

pub(crate) const TITLE: &str = "Settings";

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
    // The set itself, for a limit no backend pick can lift. `source` names the *fixable* one.
    let device = || match webos_major {
        Some(major) => format!("webOS {major}"),
        None => "this TV".to_string(),
    };
    let source = || {
        if crate::core::caps::smp_selectable() {
            "the NDL backend — try SMP".to_string()
        } else {
            device()
        }
    };
    match lock {
        menu::RowLock::HdrNeedsHevc => "HDR is not supported by H.264".to_string(),
        menu::RowLock::NoHdr => format!("HDR is not supported by {}", source()),
        menu::RowLock::OneCodec => format!("H.264 is the only codec supported by {}", source()),
        menu::RowLock::StereoOnly => format!("Stereo is the only layout supported by {}", source()),
        // Names the pick AND where it lives, and says whose limit it is: the Opus plane decodes
        // stereo on every set, while the PCM plane's ceiling is this TV's firmware. A caption that
        // only said "stereo only" would leave the user with nowhere to go.
        menu::RowLock::RouteStereoOnly(route) => format!(
            "{} audio processing {} — change it under Experimental",
            menu::audio_route_label(route),
            match route {
                AudioRoutePref::NdlOpus => "decodes stereo only".to_string(),
                // The plane's ceiling is the firmware's, and SMP (which `source` would offer)
                // has no audio plane at all — so this one names the set, not a backend to try.
                _ => format!("carries stereo only on {}", device()),
            },
        ),
        menu::RowLock::NoGamepad => "Connect a controller to your TV".to_string(),
    }
}

/// One row per `menu::SettingsRow`, in order, filtered by `menu::settings_visible_logical_rows`
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
    let row_for = |logical: menu::SettingsRow| match logical {
        menu::SettingsRow::Resolution => FocusRow::dropdown(
            crate::app::view::icons::ICON_MONITOR,
            "Resolution",
            menu::resolution_label(settings.width, settings.height),
        ),
        menu::SettingsRow::Framerate => FocusRow::dropdown(
            crate::app::view::icons::ICON_SCHEDULE,
            "Frame rate",
            format!("{} Hz", settings.refresh_hz),
        ),
        menu::SettingsRow::Bitrate => FocusRow::slider(
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
        menu::SettingsRow::VideoBackend => FocusRow::dropdown(
            crate::app::view::icons::ICON_MEMORY,
            "Video backend",
            match settings.video_backend {
                VideoBackend::Ndl => "NDL",
                VideoBackend::Smp => "SMP",
            },
        ),
        menu::SettingsRow::Codec => FocusRow::dropdown(
            crate::app::view::icons::ICON_MOVIE,
            "Codec",
            menu::codec_label(settings.codec),
        ),
        menu::SettingsRow::Hdr => FocusRow::toggle(crate::app::view::icons::ICON_SUN, "HDR", settings.hdr_enabled),
        menu::SettingsRow::Audio => FocusRow::dropdown(
            crate::app::view::icons::ICON_SIGNAL,
            "Audio",
            menu::audio_label(menu::audio_row_channels(settings)),
        ),
        menu::SettingsRow::Gamepad => FocusRow::dropdown(
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
        menu::SettingsRow::Cursor => FocusRow::action(crate::app::view::icons::ICON_MOUSE, "Cursor"),
        menu::SettingsRow::Theme => FocusRow::dropdown(
            crate::app::view::icons::ICON_PALETTE,
            "Theme",
            crate::ui::theme::for_choice(settings.theme).name,
        ),
        menu::SettingsRow::Experimental => FocusRow::action(crate::app::view::icons::ICON_BUG, "Experimental"),
        menu::SettingsRow::Diagnostics => FocusRow::action(crate::app::view::icons::ICON_WRENCH, "Diagnostics"),
        // The build version rides along as this row's value, so it's visible without
        // opening the screen — matching where the other clients surface it.
        menu::SettingsRow::About => FocusRow::action_with_value(
            crate::app::view::icons::ICON_INFO,
            "About & licenses",
            format!("v{VERSION}"),
        ),
        // Marked destructive: it discards the whole screen's worth of choices in one press,
        // and the red reads as that before it's pressed rather than after. Per-game only —
        // the global list has nothing to fall back to (see `menu::SettingsRow::Reset`).
        menu::SettingsRow::Reset => FocusRow::action(crate::app::view::icons::ICON_DELETE, "Reset")
            .danger()
            .with_subtext(ui::widgets::RowSubtext::hint("Use global settings for this game")),
        // The Cursor sub-screen's own two rows are built by `view::cursorsettings`, which
        // knows their labels and their per-screen wording; they are on neither list here.
        menu::SettingsRow::CursorCapture | menu::SettingsRow::CursorGestures => {
            debug_assert!(false, "the cursor toggles are built by view::cursorsettings");
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

/// [`scrolllist`] geometry bound to a settings scope — which is the only thing that decides
/// how many rows this list has.
pub(crate) fn layout(set: SettingsScope, screen_w: u32, screen_h: u32) -> (Rect, Rect) {
    scrolllist::layout(
        menu::settings_row_count(set),
        screen_w,
        screen_h,
        scrolllist::SETTINGS_WIDTH_FRAC,
    )
}

/// The shell only — see [`scrolllist::render`]. `suffix` is the per-game screen's game name.
pub(crate) fn render(c: &mut Canvas, set: SettingsScope, suffix: Option<&str>, hover_close: bool) -> Result<()> {
    scrolllist::render(
        c,
        menu::settings_row_count(set),
        scrolllist::SETTINGS_WIDTH_FRAC,
        TITLE,
        suffix,
        hover_close,
    )
}

/// The settings modal as a [`ModalScreen`]. `game` names the per-game variant's title suffix;
/// `None` is the global screen.
pub(crate) struct Modal<'a> {
    pub set: SettingsScope,
    pub game: Option<&'a str>,
}

impl ModalMetrics for Modal<'_> {
    fn card_rect(&self, screen_w: u32, screen_h: u32, _fonts: &ui::text::Fonts) -> Rect {
        layout(self.set, screen_w, screen_h).0
    }
}

impl ModalScreen for Modal<'_> {
    fn render(&self, c: &mut Canvas, hover_close: bool) -> Result<()> {
        render(c, self.set, self.game, hover_close)
    }
}
