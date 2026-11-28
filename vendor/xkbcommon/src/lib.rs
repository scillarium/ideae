// ideae patch: a crate of before, its lints as they were on crates.io.
#![allow(warnings)]
extern crate libc;
#[cfg(feature = "wayland")]
extern crate memmap2;

pub mod xkb;
