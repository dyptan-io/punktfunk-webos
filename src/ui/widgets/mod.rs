//! The widgets themselves: cards and posters, the focusable-row list and its controls,
//! modal chrome, list-modal screens, nav rows, toasts.
//!
//! Flat within the group — a caller reaching for one row widget reaches for its neighbours
//! in the same breath, and the file a widget lives in is an implementation detail.

mod cards;
mod confirm;
mod listmodal;
mod modal;
mod notification;
mod rows;
mod scroll;
mod sidebar;

pub use cards::*;
pub use confirm::*;
pub use listmodal::*;
pub use modal::*;
pub use notification::*;
pub use rows::*;
pub use scroll::*;
pub use sidebar::*;
