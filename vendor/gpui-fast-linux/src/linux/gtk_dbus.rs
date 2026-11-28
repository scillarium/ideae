// ideae patch: where an app's menus are on D-Bus, told to the shell with each
// window as GTK tells it (GNOME Shell's global menu extensions read it): on
// Wayland through gtk_shell1's set_dbus_properties, on X11 as the window's
// _GTK_* properties.

use std::sync::Mutex;

/// The app's menus on the session bus: `org.gtk.Menus` at `menubar_path`,
/// their actions (`app.…`) as `org.gtk.Actions` at `application_path`.
#[derive(Clone, Debug)]
pub struct GtkDbus {
    pub application_id: String,
    pub bus_name: String,
    pub application_path: String,
    pub menubar_path: String,
}

static GTK_DBUS: Mutex<Option<GtkDbus>> = Mutex::new(None);

/// Told with every window opened after it; set before the first.
pub fn set_gtk_dbus(dbus: GtkDbus) {
    *GTK_DBUS.lock().unwrap_or_else(|e| e.into_inner()) = Some(dbus);
}

pub(crate) fn gtk_dbus() -> Option<GtkDbus> {
    GTK_DBUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
