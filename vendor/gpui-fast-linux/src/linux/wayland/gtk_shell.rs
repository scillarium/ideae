// ideae patch: GTK's gtk_shell1, the part of it a window needs to tell
// GNOME Shell where its menus are (see gtk_dbus.rs).
#![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]

pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("protocols/gtk-shell.xml");
}
use self::__interfaces::*;
use wayland_client;
use wayland_client::protocol::*;

wayland_scanner::generate_client_code!("protocols/gtk-shell.xml");
