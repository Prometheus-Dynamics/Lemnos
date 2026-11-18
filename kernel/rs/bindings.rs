// SPDX-License-Identifier: GPL-2.0
//! The module's own bindings: the kernel headers the kernel crate's bindings leave out, and the module's
//! uAPI structs (rs/bindings.h). The Kbuild generates them at build time with the kernel's bindgen and the
//! kernel's C flags, so every layout here is the compiler's, never one written by hand. Only the items
//! the Kbuild allowlists are generated; every other type they name is the kernel crate's, imported here.
#![allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code,
    unreachable_pub,
    unsafe_op_in_unsafe_fn,
    improper_ctypes,
    clippy::all,
    clippy::undocumented_unsafe_blocks
)]

#[allow(unused_imports)]
use kernel::{bindings::*, ffi};

include!(env!("LEMNOS_BINDINGS"));
