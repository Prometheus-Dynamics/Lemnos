// SPDX-License-Identifier: GPL-2.0
//! The module's ABI helpers, shared by every port. The Rust half is one crate (lemnos_entry.rs declares
//! the ports as its modules), so these are defined once, here.
//!
//! The C returns are the kernel's own negative errnos. `Error::to_errno()` is already negative: a return
//! negates it at no point. Error pointers go through the kernel's from_err_ptr, never a sign test.

use core::ffi::c_void;
use kernel::{
    bindings,
    error::{Error, Result},
    ffi::{c_char, c_int, c_long},
};

/// The C return of a Result: the value, or the kernel's own negative errno.
pub(crate) fn ret(r: Result<c_long>) -> c_long {
    match r {
        Ok(v) => v,
        Err(e) => e.to_errno() as c_long,
    }
}

/// A Result from a C return that is a value or a negative errno.
pub(crate) fn c_result(v: c_long) -> Result<c_long> {
    if v < 0 {
        Err(Error::from_errno(v as c_int))
    } else {
        Ok(v)
    }
}

/// An error pointer of an errno, as ERR_PTR makes it.
pub(crate) fn err_ptr(e: Error) -> *mut c_void {
    (e.to_errno() as isize) as *mut c_void
}

/// `struct lem_claim_ops` of lemnos_internal.h: three function pointers, in this order. An absent `read`
/// or `poll` is NULL, which is `None`.
#[repr(C)]
pub(crate) struct LemClaimOps {
    pub(crate) release: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(crate) read: Option<unsafe extern "C" fn(*mut c_void, *mut bindings::file, *mut c_char, usize) -> isize>,
    pub(crate) poll: Option<
        unsafe extern "C" fn(*mut c_void, *mut bindings::file, *mut bindings::poll_table_struct) -> bindings::__poll_t,
    >,
}

const _: () = assert!(core::mem::size_of::<LemClaimOps>() == 3 * core::mem::size_of::<usize>());
