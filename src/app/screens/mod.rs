//! The screen *families* — the shapes more than one screen shares.
//!
//! A screen's own state and view stay in `app::state::<screen>` / `app::view::<screen>`; what
//! lives here is the description a family's one implementation reads, so four dialogs that
//! differ only in their labels are four values rather than four copies of the same match arm
//! (see `docs/APP-REWORK-PLAN.md` §1, P4).
pub(crate) mod confirm;
pub(crate) mod list;
pub(crate) mod rowbuttons;
pub(crate) mod scrolllist;
pub(crate) mod slots;

use crate::core::screen::Screen;

/// Whether `screen` is one of the two-button confirm dialogs — the family that shares a card
/// (one subtitle sizes it), a button row and a focus cursor, differing only in its labels.
///
/// Exhaustive on purpose: a new screen has to say which family it joins rather than being
/// absorbed by a `_ =>` arm into the wrong geometry.
pub(crate) const fn is_confirm(screen: Screen) -> bool {
    match screen {
        Screen::Wake | Screen::ForgetHost | Screen::SendLogs | Screen::SpeedTest | Screen::RemoveCollection => true,
        Screen::Home
        | Screen::Pairing
        | Screen::Settings(_)
        | Screen::AddHost
        | Screen::HostMenu
        | Screen::EditHost
        | Screen::About
        | Screen::WakeSettings
        | Screen::Diagnostics
        | Screen::Experimental
        | Screen::CursorSettings(_)
        | Screen::Collections
        | Screen::RenameCollection => false,
    }
}

/// Whether `screen` is a *scrolling* row list: a shell tile plus one tile per row, cropped to
/// a viewport that scrolls under edge fades (see `view::scrolllist`). Same contract as
/// [`is_confirm`].
pub(crate) const fn is_scroll_list(screen: Screen) -> bool {
    match screen {
        Screen::Settings(_) | Screen::Collections => true,
        Screen::Home
        | Screen::Pairing
        | Screen::AddHost
        | Screen::Wake
        | Screen::ForgetHost
        | Screen::HostMenu
        | Screen::EditHost
        // About scrolls, but wrapped text rather than rows.
        | Screen::About
        | Screen::SpeedTest
        | Screen::WakeSettings
        | Screen::Diagnostics
        | Screen::Experimental
        | Screen::CursorSettings(_)
        | Screen::SendLogs
        | Screen::RenameCollection
        | Screen::RemoveCollection => false,
    }
}

/// Whether `screen` is a plain list modal: a card holding one `FocusRow` per line, baked into
/// one tile and hit-tested by row index. Same contract as [`is_confirm`] — and the reason it
/// stays exhaustive is that a screen silently missing from a table like this inherits the
/// wrong geometry in silence.
pub(crate) const fn is_list_modal(screen: Screen) -> bool {
    match screen {
        Screen::HostMenu
        | Screen::WakeSettings
        | Screen::Diagnostics
        | Screen::Experimental
        | Screen::CursorSettings(_) => true,
        Screen::Home
        | Screen::Pairing
        // Settings is a list too, but a scrolling one — see `is_scroll_list`.
        | Screen::Settings(_)
        | Screen::AddHost
        | Screen::Wake
        | Screen::ForgetHost
        | Screen::EditHost
        | Screen::About
        | Screen::SpeedTest
        | Screen::SendLogs
        // Collections is a scrolling list too, and its name dialog is a text form.
        | Screen::Collections
        | Screen::RenameCollection
        | Screen::RemoveCollection => false,
    }
}

