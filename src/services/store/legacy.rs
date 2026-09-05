//! One-shot migration off the pre-consolidation file-per-concern layout.
use super::legacy_settings::LegacySettings;
use super::{app_dir, save, KnownHost, Persisted, Settings, TvSettings};
use crate::core::settings::{gamepad_pref, key, shared_codec, MOVED};

/// Pre-consolidation files, deleted once [`migrate`] has written the document. Only
/// `known-hosts.json` is carried over: a pairing is a fingerprint the host approved once, and
/// losing it means re-pairing. The other two are deleted unread — `selected-host` rewrites itself
/// on the next pick, and `av-trim-ms` fed a calibration this client no longer has.
const FILES: [&str; 3] = ["known-hosts.json", "selected-host.json", "av-trim-ms.conf"];

/// Folds the pre-consolidation host list into the document and writes it, once. Absent legacy
/// files are not an error: a fresh install has none, nor does one already migrated.
pub(super) fn migrate(settings: Settings) -> Persisted {
    let dir = app_dir();
    let present: Vec<&str> = FILES.into_iter().filter(|f| dir.join(f).exists()).collect();
    let known_hosts: Vec<KnownHost> = std::fs::read_to_string(dir.join("known-hosts.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let state = Persisted {
        settings,
        known_hosts,
        selected_host: None,
        version: None,
        // Nothing to carry: profiles arrived with the shared schema, which this shape predates.
        profiles: Vec::new(),
    };
    if present.is_empty() {
        return state;
    }
    // Synchronous rather than through `StateWriter`: the write is what licenses deleting the old
    // files, and doing it here keeps the writer's baseline equal to what is on disk.
    match save(&state) {
        // Only once the document is in place, so a failed write leaves them to migrate from again.
        Ok(()) => {
            for file in &present {
                let _ = std::fs::remove_file(dir.join(file));
            }
            tracing::info!(
                "migrated {} host(s) into settings.json, removed {present:?}",
                state.known_hosts.len()
            );
        }
        Err(e) => tracing::warn!("legacy state migration failed, leaving old files in place: {e:#}"),
    }
    state
}

/// Whether a stored settings object predates the shared schema: any of the rows this client
/// now keeps under its prefix sitting UNPREFIXED at the top.
pub(super) fn legacy_shape(settings: &serde_json::Value) -> bool {
    let Some(map) = settings.as_object() else {
        return false;
    };
    MOVED.iter().any(|k| map.contains_key(*k))
}

/// A pre-shared settings object as the shared document. Runs once, on the load that finds it;
/// the next save writes the shared shape.
pub(super) fn upgrade(s: &LegacySettings) -> Settings {
    let mut t = crate::core::settings::default_document();
    t.width = s.width;
    t.height = s.height;
    t.refresh_hz = s.refresh_hz;
    t.bitrate_kbps = s.bitrate_kbps;
    t.hdr_enabled = s.hdr_enabled;
    t.audio_channels = s.audio_channels;
    t.pad_haptics = s.pad_haptics;
    t.pad_speaker = if s.pad_speaker { "pad" } else { "off" }.to_string();
    t.show_stats = s.stats_overlay;
    t.codec = shared_codec(s.codec).to_string();
    t.set_cursor_capture(s.cursor_capture);
    t.gamepad = gamepad_pref(s.gamepad_type).as_str().to_string();
    t.set_hdr_display(
        crate::core::model::HdrDisplay {
            peak_nits: s.hdr_peak_nits,
            frame_avg_nits: s.hdr_frame_avg_nits,
            black_code: s.hdr_black_code,
        },
        s.hdr_calibrated,
    );
    t.set_log_level_override(s.log_level_override);
    t.set_show_logs(s.show_logs);
    t.set_game_mode(s.game_mode);
    t.set_audio_route(s.audio_route);
    if let Ok(v) = serde_json::to_value(s.cursor_gestures) {
        t.extra.insert(key("cursor_gestures"), v);
    }
    if let Ok(v) = serde_json::to_value(s.gamepad_ui) {
        t.extra.insert("gamepad_ui_enabled".to_string(), v);
    }
    if let Ok(v) = serde_json::to_value(s.gamepad_ui_mode) {
        t.extra.insert("gamepad_ui_mode".to_string(), v);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{AudioRoutePref, CodecPref, GamepadType, LogLevelOverride};

    /// A document this client wrote before the shared schema is recognised, and every field
    /// lands in the shared document where the typed reads find it.
    #[test]
    fn migrates_a_legacy_document_without_loss() {
        let on_disk = serde_json::json!({
            "width": 3840, "height": 2160, "refresh_hz": 60, "bitrate_kbps": 60000,
            "hdr_enabled": true, "hdr_peak_nits": 700, "hdr_calibrated": true,
            "codec": "hevc", "gamepad_type": "switchpro", "audio_route": "ndlopus",
            "game_mode": true, "log_level_override": "debug", "cursor_capture": false
        });
        assert!(legacy_shape(&on_disk));
        let parsed: LegacySettings = serde_json::from_value(on_disk).expect("read legacy");
        let after = upgrade(&parsed);
        assert_eq!((after.width, after.height, after.refresh_hz), (3840, 2160, 60));
        assert_eq!(after.bitrate_kbps, 60_000);
        assert!(after.hdr_enabled && after.hdr_calibrated());
        assert_eq!(after.hdr_peak_nits(), 700);
        assert_eq!(after.codec_pref(), CodecPref::Hevc);
        assert_eq!(after.gamepad_type(), GamepadType::SwitchPro);
        assert_eq!(after.audio_route(), AudioRoutePref::NdlOpus);
        assert_eq!(after.log_level_override(), LogLevelOverride::Debug);
        assert!(after.game_mode() && !after.cursor_capture());
        // The shared shape it now has is not legacy.
        let written = serde_json::to_value(&after).unwrap();
        assert!(!legacy_shape(&written));
    }
}
