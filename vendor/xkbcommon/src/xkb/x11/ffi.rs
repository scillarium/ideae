// ideae patch: libxkbcommon-x11 is loaded when an X11 keyboard is first set
// up, not linked, so that a system with no X11 (a Wayland one) starts the
// app without it. Same names and signatures as the extern block before.

use crate::xkb::ffi::{xkb_context, xkb_keymap, xkb_keymap_compile_flags, xkb_state};

use as_raw_xcb_connection::xcb_connection_t;
use std::os::raw::c_int;
use std::sync::OnceLock;

pub const XKB_X11_MIN_MAJOR_XKB_VERSION: u16 = 1;
pub const XKB_X11_MIN_MINOR_XKB_VERSION: u16 = 0;

#[repr(C)]
#[allow(non_camel_case_types)]
pub enum xkb_x11_setup_xkb_extension_flags {
    /** Do not apply any flags. */
    NO_FLAGS = 0,
}

type SetupXkbExtension = unsafe extern "C" fn(
    *mut xcb_connection_t,
    u16,
    u16,
    xkb_x11_setup_xkb_extension_flags,
    *mut u16,
    *mut u16,
    *mut u8,
    *mut u8,
) -> c_int;
type GetCoreKeyboardDeviceId = unsafe extern "C" fn(*mut xcb_connection_t) -> i32;
type KeymapNewFromDevice = unsafe extern "C" fn(*mut xkb_context, *mut xcb_connection_t, i32, xkb_keymap_compile_flags) -> *mut xkb_keymap;
type StateNewFromDevice = unsafe extern "C" fn(*mut xkb_keymap, *mut xcb_connection_t, i32) -> *mut xkb_state;

struct Lib {
    setup_xkb_extension: SetupXkbExtension,
    get_core_keyboard_device_id: GetCoreKeyboardDeviceId,
    keymap_new_from_device: KeymapNewFromDevice,
    state_new_from_device: StateNewFromDevice,
    _lib: libloading::Library,
}

fn lib() -> &'static Lib {
    static LIB: OnceLock<Lib> = OnceLock::new();
    LIB.get_or_init(|| {
        // SAFETY: the library's initializers are libxkbcommon-x11's own, and
        // the symbols are read with the types its header gives them.
        unsafe {
            let lib = ["libxkbcommon-x11.so.0", "libxkbcommon-x11.so"]
                .iter()
                .find_map(|name| libloading::Library::new(name).ok())
                .expect("an X11 session needs libxkbcommon-x11 (libxkbcommon-x11.so.0), which is not installed");
            let sym = |name: &[u8]| -> *const () {
                *lib.get::<*const ()>(name)
                    .unwrap_or_else(|e| panic!("libxkbcommon-x11 has no {}: {e}", String::from_utf8_lossy(name)))
            };
            Lib {
                setup_xkb_extension: std::mem::transmute::<*const (), SetupXkbExtension>(sym(b"xkb_x11_setup_xkb_extension\0")),
                get_core_keyboard_device_id: std::mem::transmute::<*const (), GetCoreKeyboardDeviceId>(sym(b"xkb_x11_get_core_keyboard_device_id\0")),
                keymap_new_from_device: std::mem::transmute::<*const (), KeymapNewFromDevice>(sym(b"xkb_x11_keymap_new_from_device\0")),
                state_new_from_device: std::mem::transmute::<*const (), StateNewFromDevice>(sym(b"xkb_x11_state_new_from_device\0")),
                _lib: lib,
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn xkb_x11_setup_xkb_extension(
    connection: *mut xcb_connection_t,
    major_xkb_version: u16,
    minor_xkb_version: u16,
    flags: xkb_x11_setup_xkb_extension_flags,
    major_xkb_version_out: *mut u16,
    minor_xkb_version_out: *mut u16,
    base_event_out: *mut u8,
    base_error_out: *mut u8,
) -> c_int {
    (lib().setup_xkb_extension)(
        connection,
        major_xkb_version,
        minor_xkb_version,
        flags,
        major_xkb_version_out,
        minor_xkb_version_out,
        base_event_out,
        base_error_out,
    )
}

pub unsafe fn xkb_x11_get_core_keyboard_device_id(connection: *mut xcb_connection_t) -> i32 {
    (lib().get_core_keyboard_device_id)(connection)
}

pub unsafe fn xkb_x11_keymap_new_from_device(
    context: *mut xkb_context,
    connection: *mut xcb_connection_t,
    device_id: i32,
    flags: xkb_keymap_compile_flags,
) -> *mut xkb_keymap {
    (lib().keymap_new_from_device)(context, connection, device_id, flags)
}

pub unsafe fn xkb_x11_state_new_from_device(
    keymap: *mut xkb_keymap,
    connection: *mut xcb_connection_t,
    device_id: i32,
) -> *mut xkb_state {
    (lib().state_new_from_device)(keymap, connection, device_id)
}
