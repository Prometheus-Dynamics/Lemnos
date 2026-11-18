// SPDX-License-Identifier: GPL-2.0
//! The module's parameters, as `module_param(name, type, 0444)` and `MODULE_PARM_DESC` make them in C: a
//! `struct kernel_param` in the `__param` section with the kernel's own operations for the type, so each
//! reads back in sysfs, and its `.modinfo` strings for modinfo(8). A built-in module's names carry the
//! module's name (`lemnos.<name>`), as KBUILD_MODNAME gives them in C.
//!
//! The kernel crate's `module!` parameters are not readable in sysfs, which the harness and the daemon
//! read, hence this.

use core::{cell::UnsafeCell, ffi::c_void};
use kernel::bindings;

/// A parameter's value. The kernel writes it while parsing the module's parameters, before init; every
/// parameter is 0444, so nothing writes it after.
#[repr(transparent)]
pub(crate) struct Value<T>(UnsafeCell<T>);

// SAFETY: written only by the kernel's parameter parser, before init, and only read after.
unsafe impl<T> Sync for Value<T> {}

impl<T: Copy> Value<T> {
    pub(crate) const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    /// The value, as the command line or the module's load gave it.
    pub(crate) fn get(&self) -> T {
        // SAFETY: written only by the parameter parser, before init (see Value); this reads it.
        unsafe { *self.0.get() }
    }

    /// The address the kernel's operations write.
    pub(crate) const fn arg(&self) -> *mut c_void {
        self.0.get().cast()
    }
}

/// `struct kernel_param`, in the `__param` section the kernel reads parameters from.
#[repr(transparent)]
pub(crate) struct KernelParam(pub(crate) bindings::kernel_param);

// SAFETY: the kernel alone reads it (it is never accessed from Rust), and serialises that itself.
unsafe impl Sync for KernelParam {}

/// A `.modinfo` string as an array, for a static in that section.
pub(crate) const fn modinfo<const N: usize>(s: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = s[i];
        i += 1;
    }
    out
}

/// `param!(STATIC, name: type = default, ops, "type name", "description")`: the 0444 parameter `name`,
/// read with `STATIC.get()`. `ops` is the kernel's `param_ops_<type>` for it; the type name is what
/// modinfo shows.
macro_rules! param {
    ($rust:ident, $name:ident: $ty:ty = $default:expr, $ops:ident, $tyname:literal, $desc:literal) => {
        static $rust: crate::param::Value<$ty> = crate::param::Value::new($default);

        const _: () = {
            use crate::param::{modinfo, KernelParam};
            use kernel::bindings;

            #[link_section = "__param"]
            #[used(compiler)]
            static PARAM: KernelParam = KernelParam(bindings::kernel_param {
                name: if cfg!(MODULE) {
                    concat!(stringify!($name), "\0").as_ptr().cast()
                } else {
                    concat!("lemnos.", stringify!($name), "\0").as_ptr().cast()
                },
                #[cfg(MODULE)]
                // `__this_module` is made by the kernel at load and lives until the module is unloaded.
                mod_: (&raw const bindings::__this_module).cast_mut(),
                #[cfg(not(MODULE))]
                mod_: core::ptr::null_mut(),
                // The kernel's exported operations for the type, a static that lives for ever.
                ops: &raw const bindings::$ops,
                perm: 0o444,
                level: -1,
                flags: 0,
                __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: $rust.arg() },
            });

            #[cfg(MODULE)]
            const PARMTYPE: &str = concat!("parmtype=", stringify!($name), ":", $tyname, "\0");
            #[cfg(MODULE)]
            const PARM: &str = concat!("parm=", stringify!($name), ":", $desc, "\0");
            #[cfg(not(MODULE))]
            const PARMTYPE: &str = concat!("lemnos.parmtype=", stringify!($name), ":", $tyname, "\0");
            #[cfg(not(MODULE))]
            const PARM: &str = concat!("lemnos.parm=", stringify!($name), ":", $desc, "\0");
            #[link_section = ".modinfo"]
            #[used(compiler)]
            static MI_PARMTYPE: [u8; PARMTYPE.len()] = modinfo(PARMTYPE.as_bytes());
            #[link_section = ".modinfo"]
            #[used(compiler)]
            static MI_PARM: [u8; PARM.len()] = modinfo(PARM.as_bytes());
        };
    };
}
pub(crate) use param;
