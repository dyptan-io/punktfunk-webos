//! "Reset this game's settings?" confirmation. Logic lives in `app::state::gamesettings`.
//!
//! The dialog itself is `app::view::confirm` — this is only its copy.

pub const TITLE: &str = "Reset game settings?";

pub fn subtitle(game_name: &str) -> String {
    format!("{game_name} will go back to using the global settings. Its overrides are discarded.")
}
