//! Add host and Edit address — their copy. Both draw `view::textform`.

pub(crate) const ADD_TITLE: &str = "Add host";
pub(crate) const EDIT_TITLE: &str = "Edit address";
pub(crate) const ADD_SUBTITLE: &str = "Enter the host's IP address. Right adds an optional port.";

/// Edit gets its own subtitle rather than reusing the Add one, which would overflow the
/// card once the host's name is in it.
pub(crate) fn edit_subtitle(host_name: &str) -> String {
    format!("New IP address for {host_name}. Its pairing is kept.")
}
