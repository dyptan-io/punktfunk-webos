//! "Send logs to developer?" confirmation. Logic lives in `app::state::sendlogs`.
//!
//! The dialog itself is `app::view::confirm` — this is only its copy.

pub const TITLE: &str = "Send logs to developer?";
pub const SUBTITLE: &str = "This uploads this session's log file to the app developer to help diagnose problems. \
     Logs can include host names, IP addresses, and game titles. Only send them if you're \
     comfortable sharing that.";
