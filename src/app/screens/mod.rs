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
