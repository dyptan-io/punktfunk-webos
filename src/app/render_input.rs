//! `RenderInput` — the read-only slice of `App` state the render path consumes.
//!
//! The renderer (`prepare_tiles`/`draw_list`, today still on `App`) is being lifted onto the
//! tile cache so it takes app state as *input* rather than reading `App` directly.
//! `App::render_input` assembles this once per frame; the render methods read `input.<field>`
//! instead of `self.<field>`. `app`-internal: nothing below `app` sees it.
//!
//! Grown one family at a time: only the fields already migrated off `self` live here.

use crate::app::hosts::HostEntry;
use crate::core::screen::HomeFocus;

pub struct RenderInput<'a> {
    pub home_focus: HomeFocus,
    pub entries: &'a [HostEntry],
    /// A host is selected (grid has content rather than the "no host" hint).
    pub host_selected: bool,
    /// The bottom status block's opacity, or `None` where no line is up (see
    /// `App::home_status_alpha`).
    pub status_alpha: Option<f32>,
    /// The grid's cards are built and revealed (past the load spinner).
    pub grid_reveal_ready: bool,
    /// The focused row's press dip (see `App::press`) — the sidebar's rows are buttons.
    pub press: crate::ui::animation::Press,
    /// Focus-pop clock shared by Home's focused grid card and sidebar row.
    pub focus_anim: Option<std::time::Instant>,
}
