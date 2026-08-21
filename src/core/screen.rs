/// Which settings document a settings-shaped screen edits. The per-game scope shows the
/// overridable rows only (see `app::menu::settings_visible_logical_rows`) and edits a scratch
/// copy of the global document; both share every row mutator, so a row behaves identically
/// wherever it appears.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SettingsScope {
    Global,
    Game,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Screen {
    Home,
    Pairing,
    /// The settings list, in one of two scopes: the global document, or one game's
    /// overrides of it (`SettingsScope::Game`, reached only by holding that game's card —
    /// there is no path to it from the global screen). One variant, because the two are the
    /// same screen with a different row list: every dispatch site treats them alike.
    Settings(SettingsScope),
    AddHost,
    Wake,
    ForgetHost,
    HostMenu,
    EditHost,
    About,
    SpeedTest,
    WakeSettings,
    /// Log level debug aid (see `app/diagnostics.rs`).
    Diagnostics,
    /// Experimental/unstable toggles (see `app/experimental.rs`).
    Experimental,
    /// Pointer/cursor behaviour, grouped off Settings (see `app/cursorsettings.rs`).
    /// Carries the scope of the settings screen that opened it, so the sub-screen edits the
    /// same document its parent does and Back knows where to return.
    CursorSettings(SettingsScope),
    /// "Send logs to developer" confirmation (see `app/sendlogs.rs`).
    SendLogs,
    /// Which collection a held card belongs to (see `app/collections.rs`). A scrolling row
    /// list, since a host may have every one of `MAX_COLLECTIONS` plus Library.
    Collections,
}

/// Pairing modal's focused input: PIN row or "Request access" button.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PairingFocus {
    #[default]
    Pin,
    RequestAccess,
}

/// Home screen focus location.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HomeFocus {
    Sidebar(usize),
    SidebarMenu(usize),
    Grid(usize),
}
