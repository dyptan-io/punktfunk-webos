//! "Forget this host?" confirmation. Logic lives in `app::state::forget`.
//!
//! The dialog itself is `app::view::confirm` — this is only its copy.

pub const TITLE: &str = "Forget this host?";

pub fn subtitle(host_name: &str) -> String {
    format!("{host_name} will be removed from this TV. You can pair with it again later.")
}
