//! Per-screen presentation: each modal's card geometry, its copy, and how it paints.
//! Logic counterparts live in `app::state`.
//!
//! These are this app's screens, so they live here rather than in `ui` — `ui` is the
//! portable, app-agnostic half (cards, rows, list modals, text, fades) that these compose.
//! The migrated modules are plain free functions over the values they need (a `Settings`,
//! a host name, a focus index) rather than `impl App` blocks, so screen presentation
//! carries no dependency on the app state machine.
pub(crate) mod about;
pub(crate) mod addhost;
pub(crate) mod cardmenu;
pub(crate) mod collections;
pub(crate) mod confirm;
pub(crate) mod cursorsettings;
pub(crate) mod diagnostics;
pub(crate) mod experimental;
pub(crate) mod forget;
pub(crate) mod hdrcalibration;
pub(crate) mod home;
pub(crate) mod hostmenu;
pub(crate) mod icons;
pub(crate) mod pairing;
pub(crate) mod resetgame;
pub(crate) mod scrolllist;
pub(crate) mod sendlogs;
pub(crate) mod settings;
pub(crate) mod sidebar;
pub(crate) mod speedtest;
pub(crate) mod textform;
pub(crate) mod wake;
pub(crate) mod wakesettings;
