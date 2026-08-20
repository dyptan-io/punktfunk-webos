//! The selected host's library: its games, their art, and the pin bookkeeping the grid reads
//! every frame.
//!
//! Grouped so the grid's `&self` geometry queries can borrow the library disjointly from the
//! `&mut self` paths that mutate it. Everything here is per-host state and is replaced wholesale
//! on a host switch (`App::clear_selected_host`).

use std::collections::HashMap;

use tiny_skia::Pixmap;

use crate::services::library::GameEntry;

#[derive(Default)]
pub(crate) struct Library {
    pub(crate) selected_host: Option<(String, u16)>,
    pub(crate) games: Vec<GameEntry>,
    /// Leading pinned-game entries; kept in pin order.
    pub(crate) pinned_count: usize,
    /// Whether the selected host has its Desktop card pinned. Maintained next to
    /// `pinned_count` by [`crate::app::App::reorder_games_by_pin`] rather than read back out of
    /// the host's pin map on demand: `grid_layout` is asked for a card rect on every frame and
    /// every pointer motion, and deriving this meant scanning `known_hosts` and a map lookup
    /// each time. Every path that changes a pin, the selected host, or the library goes through
    /// that one function (or clears the grid via [`crate::app::App::clear_grid_pins`]).
    pub(crate) desktop_pin: bool,
    /// Host answered library fetch (gates Desktop card).
    pub(crate) games_loaded: bool,
    /// Cover art pixmaps by game id.
    pub(crate) art: HashMap<String, Pixmap>,
}
