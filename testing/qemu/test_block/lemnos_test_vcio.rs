// SPDX-License-Identifier: GPL-2.0
//! The firmware mailbox's stand-in, `/dev/vcio`: the node a Raspberry Pi kernel makes (root's, mode
//! 0600, as on a board), and its one request, `IOCTL_MBOX_PROPERTY` (`_IOWR(100, 0, char *)`), for the
//! one-tag messages Lemnos sends (seven words: size, code, tag, value size, tag code, value, end). It
//! answers as the firmware does: the reply code `0x80000000` when it took the request, and the tag code
//! with its value size. It keeps what it was asked in its parameters:
//!
//! - `orders`: the reboot orders (tag `0x0003808b`) it took, and `order`, the last one's value;
//! - `asked`: every message it was sent, taken or not;
//! - `clears`: the boot count sets (tag `0x0003808d`) it took;
//! - `refuse` (writable): while it is not 0, every message is answered with the error code
//!   `0x80000001` and nothing is taken, as a firmware that refuses.
//!
//! Nothing here is the firmware: there is no next boot for an order to apply to.
//!
//! Copyright (c) 2026 the Lemnos authors.

#[path = "fixture.rs"]
mod fixture;

use core::sync::atomic::{AtomicU32, Ordering};
use fixture::{param_typed, parm_desc, this_module, Fixed, Shared};
use kernel::{
    bindings,
    error::code::*,
    ffi::{c_int, c_long, c_uint, c_ulong, c_void},
    ioctl::_IOWR,
    prelude::*,
};

/// `IOCTL_MBOX_PROPERTY`: `_IOWR(100, 0, char *)`.
const MBOX_PROPERTY: u32 = _IOWR::<*mut u8>(100, 0);
/// The message's size in bytes (seven words).
const MESSAGE_BYTES: u32 = 28;
const REQUEST: u32 = 0;
const RESPONSE_OK: u32 = 0x8000_0000;
const RESPONSE_ERROR: u32 = 0x8000_0001;
const TAG_SET_REBOOT_ORDER: u32 = 0x0003_808b;
const TAG_SET_BOOT_COUNT: u32 = 0x0003_808d;
const TAG_GET_BOOT_COUNT: u32 = 0x0003_008d;

static ORDERS: AtomicU32 = AtomicU32::new(0);
static ORDER: AtomicU32 = AtomicU32::new(0);
static ASKED: AtomicU32 = AtomicU32::new(0);
static CLEARS: AtomicU32 = AtomicU32::new(0);
static REFUSE: AtomicU32 = AtomicU32::new(0);
param_typed!(orders, 0o444, param_ops_uint, ORDERS.as_ptr().cast(), "uint");
parm_desc!(orders, "Test only: the reboot orders the stand-in took");
param_typed!(order, 0o444, param_ops_uint, ORDER.as_ptr().cast(), "uint");
parm_desc!(order, "Test only: the value of the last reboot order taken");
param_typed!(asked, 0o444, param_ops_uint, ASKED.as_ptr().cast(), "uint");
parm_desc!(asked, "Test only: the messages the stand-in was sent");
param_typed!(clears, 0o444, param_ops_uint, CLEARS.as_ptr().cast(), "uint");
parm_desc!(clears, "Test only: the boot count sets the stand-in took");
param_typed!(refuse, 0o644, param_ops_uint, REFUSE.as_ptr().cast(), "uint");
parm_desc!(refuse, "Test only: not 0 answers every message with the error code");

/// The answer to one message, in place: the reply code, and for a taken tag its code and value.
fn answer(w: &mut [u32; 7]) -> Result {
    if w[0] != MESSAGE_BYTES || w[1] != REQUEST || w[3] != 4 || w[4] != REQUEST || w[6] != 0 {
        return Err(EINVAL);
    }
    ASKED.fetch_add(1, Ordering::Relaxed);
    if REFUSE.load(Ordering::Relaxed) != 0 {
        w[1] = RESPONSE_ERROR;
        return Ok(());
    }
    match w[2] {
        TAG_SET_REBOOT_ORDER => {
            ORDER.store(w[5], Ordering::Relaxed);
            ORDERS.fetch_add(1, Ordering::Relaxed);
        }
        TAG_SET_BOOT_COUNT => {
            CLEARS.fetch_add(1, Ordering::Relaxed);
        }
        TAG_GET_BOOT_COUNT => w[5] = 0,
        _ => {
            w[1] = RESPONSE_ERROR;
            return Ok(());
        }
    }
    w[1] = RESPONSE_OK;
    w[4] = RESPONSE_OK | 4;
    Ok(())
}

/// One request: the message is copied in, answered and copied back.
///
/// # Safety
///
/// `arg` is the user address the ioctl was passed.
unsafe fn property(arg: c_ulong) -> Result {
    let mut w = [0u32; 7];
    let bytes = core::mem::size_of_val(&w) as c_ulong;
    // SAFETY: `w` is room for the seven words; `arg` is the caller's user address, checked by the copy.
    let left = unsafe { bindings::copy_from_user(w.as_mut_ptr().cast(), arg as *const c_void, bytes) };
    if left != 0 {
        return Err(EFAULT);
    }
    answer(&mut w)?;
    // SAFETY: as above, the other way: the seven words back to the caller's buffer.
    let left = unsafe { bindings::copy_to_user(arg as *mut c_void, w.as_ptr().cast(), bytes) };
    if left != 0 {
        return Err(EFAULT);
    }
    Ok(())
}

unsafe extern "C" fn vcio_ioctl(_file: *mut bindings::file, cmd: c_uint, arg: c_ulong) -> c_long {
    if cmd != MBOX_PROPERTY {
        return ENOTTY.to_errno() as c_long;
    }
    // SAFETY: `arg` is this ioctl's user address.
    match unsafe { property(arg) } {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

static VCIO_FOPS: Fixed<bindings::file_operations> = Fixed(bindings::file_operations {
    owner: this_module(),
    unlocked_ioctl: Some(vcio_ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: every other operation absent (null), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
});

/// The node: `/dev/vcio`, root's with mode 0600, as the board's kernel makes it.
static VCIO_MISC: Shared<bindings::miscdevice> = Shared::new(bindings::miscdevice {
    minor: bindings::MISC_DYNAMIC_MINOR as c_int,
    name: c"vcio".as_ptr().cast(),
    fops: VCIO_FOPS.get(),
    mode: 0o600,
    // SAFETY: the rest of a struct miscdevice is pointers and a list head, zero by default.
    ..unsafe { core::mem::zeroed() }
});

struct TestVcio;

impl kernel::Module for TestVcio {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        // SAFETY: the node is a static; misc_register is its one registration.
        let ret = unsafe { bindings::misc_register(VCIO_MISC.get()) };
        if ret != 0 {
            return Err(Error::from_errno(ret));
        }
        Ok(TestVcio)
    }
}

impl Drop for TestVcio {
    fn drop(&mut self) {
        // SAFETY: the node was registered by init, and this is its one deregistration.
        unsafe { bindings::misc_deregister(VCIO_MISC.get()) };
    }
}

module! {
    type: TestVcio,
    name: "lemnos_test_vcio",
    authors: ["Lemnos"],
    description: "The firmware mailbox's stand-in, for testing Lemnos",
    license: "GPL",
}
