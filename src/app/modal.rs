//! [`ModalState`] — everything that exists only while a modal is up.
//!
//! Nine fields that were `modal_*`-prefixed on `App` by way of saying they belong together: the
//! shell tile's version, the rendered scroll crop, the region the tile covers, the pixels of a
//! modal that is fading out, and the three clocks (layer fade, focused-widget pop, toggle slide)
//! that no other screen has. Grouping them gives the render path a disjoint borrow and takes
//! nine names out of `App`'s surface.
use std::time::Instant;

use crate::app::render::ModalSnapshot;
use crate::core::screen::Screen;
use crate::ui;
use crate::ui::render::Rect;

pub(crate) struct ModalState {
    /// The version the modal shell tile was last rasterized at — a value change invalidates it,
    /// but moving focus alone must not (that's the focus tile's job). A hash rather than the key
    /// itself (`App::modal_shell_version`), so nothing here owns a copy of every label the shell
    /// draws. `None` while `Screen::Home`/`Screen::AddHost` (no `ModalShellKey` variant;
    /// `AddHost` just redraws on any `content_dirty` tick instead — its typed-digit display has
    /// no separate focus tile to protect).
    pub shell_version: Option<u64>,
    /// Where the scrolling modal's viewport is *rendered*, in pixels, and where it is heading.
    ///
    /// `App::scroll.offset` stays an integral row/line index — focus logic and the scrollbar are
    /// defined in those units, and quantized steps are what make keyboard navigation land
    /// predictably. Only the rendered crop is continuous, which is what makes the motion smooth,
    /// and it is also what lets the last row sit flush against the viewport's bottom (an integral
    /// offset overshoots by whatever the peek strip is worth).
    pub scroll_px: i32,
    pub scroll_target_px: i32,
    /// Which screen `scroll_px` describes, so opening a different modal snaps instead of gliding
    /// from the previous one's offset.
    pub scroll_screen: Option<Screen>,
    /// Screen-space region the `tile::MODAL` painter currently covers (card bbox +
    /// `MODAL_TILE_PAD`) — set by `prepare_modal` when it (re)builds the tile, read by
    /// `compose_modal` to place it. Only the *live* modal's; a fading one carries its own region
    /// in [`ModalSnapshot`].
    pub tile_region: Rect,
    /// The fading-out modal's pixels, taken the frame it was left. `None` when no close fade is
    /// in flight.
    pub prev: Option<ModalSnapshot>,
    /// Open/close fade for whichever modal is up — see `ui::fade::ModalFade`'s docs. Payload is
    /// the `Screen` that was left — `snapshot_closing_modal` needs it to freeze that screen's
    /// scroll crop after `App::screen` has moved on.
    pub fade: ui::fade::ModalFade<Screen>,
    /// When the open modal's focused widget last moved (zooms it in over
    /// `ui::animation::FOCUS_POP`, same GPU-scale technique as `App::focus_anim` — see
    /// `draw_list`'s `tile::MODAL_FOCUS` handling). Shared by every modal (Settings row, Wake
    /// row, Pairing digit/button, `ForgetHost` button) since only one is ever open, and focused,
    /// at a time.
    pub focus_anim: Option<Instant>,
    /// In-flight `Toggle` row flip: `(when it started, the value it flipped from, the focused row
    /// it flipped)` — lets `modal_focus_tile`'s render slide the switch knob from its old state
    /// to its new one over `ui::animation::FOCUS_POP` instead of snapping. The row index scopes
    /// the slide to the row that actually changed: without it, navigating onto a different toggle
    /// whose state happens to differ from `from` mid-animation would make that unrelated switch
    /// spuriously slide (see `App::toggle_frac`). Shared by Settings' HDR/Stats-overlay toggles
    /// and Wake's auto-send one.
    pub switch_anim: Option<(Instant, bool, usize)>,
}

impl Default for ModalState {
    fn default() -> Self {
        Self {
            shell_version: None,
            scroll_px: 0,
            scroll_target_px: 0,
            scroll_screen: None,
            // Never read before `prepare_modal` sets it; 1x1 rather than empty so a stray
            // composite of an unbuilt modal is a pixel, not a division by zero.
            tile_region: Rect::new(0, 0, 1, 1),
            prev: None,
            fade: ui::fade::ModalFade::new(),
            focus_anim: None,
            switch_anim: None,
        }
    }
}
