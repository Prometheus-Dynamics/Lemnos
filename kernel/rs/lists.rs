// SPDX-License-Identifier: GPL-2.0
//! Which blocks: the module's lists of compatibles (owned=, confined=, unconfined=, pisp_be=, gsdma=,
//! vip9000=) and the test switch test_irq_inject, as parameters, and the match of a device's compatibles
//! against a list. A list is compatibles, comma-separated in pairs (vendor,name).
//!
//! Only the blocks in owned= are claimed. The module's drivers have no match table: they bind only where
//! driver_override names them, and refuse a device that is not listed.

use crate::param::param;
use kernel::{bindings, ffi::c_char};

/// The compatibles one device is asked for: more than any block's description lists.
const MAX_COMPATIBLE: usize = 8;

param!(
    OWNED,
    owned: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks Lemnos owns, comma-separated in pairs (vendor,name); no other device is claimed"
);
param!(
    CONFINED,
    confined: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks whose DMA their IOMMU really confines; no other block's registers can be mapped, except those in unconfined="
);
param!(
    PISP_BE,
    pisp_be: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks driven by the PiSP back end code; their registers and interrupts never reach user space"
);
param!(
    GSDMA,
    gsdma: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks driven as the K230 graphics DMA engine; their registers and interrupts never reach user space"
);
param!(
    UNCONFINED,
    unconfined: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks whose registers may be mapped though no IOMMU confines their DMA: the driver in user space alone keeps that DMA in bounds"
);
param!(
    VIP9000,
    vip9000: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks driven as the VIP9000 neural accelerator; their registers, interrupts and MMU never reach user space"
);
param!(
    TEST_IRQ_INJECT,
    test_irq_inject: bool = false,
    param_ops_bool,
    "bool",
    "Test only: LEMNOS_IOC_TEST_IRQ sets an interrupt pending at its controller"
);

/// One of the module's lists.
#[derive(Clone, Copy)]
pub(crate) enum List {
    Owned,
    Confined,
    Unconfined,
    PispBe,
    Gsdma,
    Vip9000,
}

impl List {
    fn text(self) -> *const c_char {
        match self {
            List::Owned => OWNED.get(),
            List::Confined => CONFINED.get(),
            List::Unconfined => UNCONFINED.get(),
            List::PispBe => PISP_BE.get(),
            List::Gsdma => GSDMA.get(),
            List::Vip9000 => VIP9000.get(),
        }
    }

    /// Whether any of the device's compatibles is in this list.
    ///
    /// # Safety
    /// `dev` is a live device.
    pub(crate) unsafe fn has(self, dev: *mut bindings::device) -> bool {
        // SAFETY: the caller's device; the list is the parameter's string, set before init and never after.
        unsafe { lem_device_listed(dev, self.text()) }
    }
}

/// Whether LEMNOS_IOC_TEST_IRQ may raise an interrupt (test_irq_inject=).
pub(crate) fn test_irq_inject() -> bool {
    TEST_IRQ_INJECT.get()
}

/// The bytes of a C string, without its terminator.
///
/// # Safety
/// `s` is a NUL-terminated string that outlives the result.
unsafe fn bytes<'a>(s: *const c_char) -> &'a [u8] {
    // SAFETY: the caller's string.
    unsafe { core::ffi::CStr::from_ptr(s.cast()) }.to_bytes()
}

/// Whether `compatible` is one of the comma-separated pairs in `list`: a pair runs to the second comma
/// after its start, and must equal the compatible whole.
fn listed(list: &[u8], compatible: &[u8]) -> bool {
    let mut rest = list;
    while !rest.is_empty() {
        let comma = rest.iter().position(|&b| b == b',');
        let end = comma.and_then(|c| {
            rest[c + 1..]
                .iter()
                .position(|&b| b == b',')
                .map(|e| c + 1 + e)
        });
        let pair = &rest[..end.unwrap_or(rest.len())];
        if pair == compatible {
            return true;
        }
        match end {
            Some(e) => rest = &rest[e + 1..],
            None => break,
        }
    }
    false
}

/// Whether any of the device's compatibles is in `list`, from any firmware description
/// (lemnos_internal.h; the C blocks and rs/regjob.rs call it).
///
/// # Safety
/// `dev` is a live device; `list` is a NUL-terminated string.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_device_listed(
    dev: *mut bindings::device,
    list: *const c_char,
) -> bool {
    let mut c = [core::ptr::null::<c_char>(); MAX_COMPATIBLE];
    // SAFETY: the caller's device; the array takes up to MAX_COMPATIBLE strings, which the device's
    // firmware node owns for as long as the device lives.
    let n = unsafe {
        bindings::device_property_read_string_array(
            dev,
            c"compatible".as_ptr().cast(),
            c.as_mut_ptr(),
            c.len(),
        )
    };
    // SAFETY: the caller's list.
    let list = unsafe { bytes(list) };
    // A negative count (no such property) lists nothing.
    // SAFETY: the first `n` entries were filled with the node's strings.
    c.iter()
        .take(usize::try_from(n).unwrap_or(0))
        .any(|&s| listed(list, unsafe { bytes(s) }))
}

/// Whether `dev` is one of the blocks the owned= parameter lists (lemnos_internal.h).
///
/// # Safety
/// `dev` is a live device.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_block_owned(dev: *mut bindings::device) -> bool {
    // SAFETY: the caller's device.
    unsafe { List::Owned.has(dev) }
}
