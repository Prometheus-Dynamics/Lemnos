// SPDX-License-Identifier: GPL-2.0
//! RP1 PIO state machines and their DMA, Rust (CZ-MOD stage 1j of the port; replaces lemnos_pio.c under
//! CONFIG_RUST).
//!
//! A claim on "pio-0" holds one client of the RP1 PIO driver. The four C entry points keep their
//! signatures (lemnos_internal.h): lem_pio_open, lem_pio_call, lem_pio_config_xfer and lem_pio_xfer.
//!
//! The driver's calls exist only on a kernel that has the RP1 PIO driver. Kbuild passes `--cfg
//! lemnos_have_pio` then (CONFIG_RP1_PIO); without it the four entry points answer -ENOTTY, as the C
//! stubs did, and no driver symbol is referenced.
//!
//! The arguments are checked here before the driver sees them: an index past the end of a state machine
//! or GPIO table, or a program longer than the instruction memory, never reaches the driver. Data goes
//! through a buffer this module allocated, and the driver is always given address 0, which means its own
//! DMA buffers.

use core::mem::size_of;
use kernel::{
    error::code::*,
    ffi::{c_int, c_long},
};

/// `LEMNOS_PIO_ARGS` of lemnos.h: the argument block of a call.
const LEMNOS_PIO_ARGS: usize = 72;
/// `struct rp1_pio_add_program_args`: two u16 and 32 instructions. The largest argument block.
const PROGRAM_ARGS: usize = 2 + 2 + 2 * 32;
const _: () = assert!(PROGRAM_ARGS <= LEMNOS_PIO_ARGS);

/// `struct lemnos_pio_open` of lemnos.h (LEMNOS_IOC_PIO_OPEN).
#[repr(C)]
pub(crate) struct LemnosPioOpen {
    claim: c_int,
}

/// `struct lemnos_pio_call` of lemnos.h (LEMNOS_IOC_PIO_CALL).
#[repr(C)]
pub(crate) struct LemnosPioCall {
    claim: c_int,
    op: u32,
    result: i64,
    args: [u8; LEMNOS_PIO_ARGS],
}

/// `struct lemnos_pio_config_xfer` of lemnos.h (LEMNOS_IOC_PIO_CONFIG_XFER).
#[repr(C)]
pub(crate) struct LemnosPioConfigXfer {
    claim: c_int,
    sm: u32,
    dir: u32,
    buf_size: u32,
    buf_count: u32,
    reserved: u32,
}

/// `struct lemnos_pio_xfer` of lemnos.h (LEMNOS_IOC_PIO_XFER).
#[repr(C)]
pub(crate) struct LemnosPioXfer {
    claim: c_int,
    sm: u32,
    dir: u32,
    len: u32,
    data: u64,
    timeout_ms: u32,
    reserved: u32,
}

const _: () = assert!(size_of::<LemnosPioOpen>() == 4);
const _: () = assert!(size_of::<LemnosPioCall>() == 88);
const _: () = assert!(size_of::<LemnosPioConfigXfer>() == 24);
const _: () = assert!(size_of::<LemnosPioXfer>() == 32);

#[cfg(lemnos_have_pio)]
use crate::abi::ret;

/// The C entry points (lemnos_internal.h). Built with the driver: the port in `port`.
#[cfg(lemnos_have_pio)]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_open(o: *const LemnosPioOpen) -> c_long {
    // SAFETY: the C caller passes its own copy of the ioctl argument.
    ret(port::open(unsafe { &*o }))
}

/// See the note above: an index the driver is never asked about.
#[cfg(lemnos_have_pio)]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_call(c: *mut LemnosPioCall) -> c_long {
    // SAFETY: the C caller passes its own copy of the ioctl argument.
    ret(port::call(unsafe { &mut *c }))
}

/// See the note above.
#[cfg(lemnos_have_pio)]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_config_xfer(c: *const LemnosPioConfigXfer) -> c_long {
    // SAFETY: the C caller passes its own copy of the ioctl argument.
    ret(port::config_xfer(unsafe { &*c }))
}

/// See the note above.
#[cfg(lemnos_have_pio)]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_xfer(x: *const LemnosPioXfer) -> c_long {
    // SAFETY: the C caller passes its own copy of the ioctl argument.
    ret(port::xfer(unsafe { &*x }))
}

/// Without the RP1 PIO driver the part is absent: every call answers -ENOTTY, as the C stubs did.
#[cfg(not(lemnos_have_pio))]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_open(_o: *const LemnosPioOpen) -> c_long {
    ENOTTY.to_errno() as c_long
}

/// See the note above.
#[cfg(not(lemnos_have_pio))]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_call(_c: *mut LemnosPioCall) -> c_long {
    ENOTTY.to_errno() as c_long
}

/// See the note above.
#[cfg(not(lemnos_have_pio))]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_config_xfer(_c: *const LemnosPioConfigXfer) -> c_long {
    ENOTTY.to_errno() as c_long
}

/// See the note above.
#[cfg(not(lemnos_have_pio))]
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pio_xfer(_x: *const LemnosPioXfer) -> c_long {
    ENOTTY.to_errno() as c_long
}

#[cfg(lemnos_have_pio)]
mod port {
    use super::*;
    use crate::abi::{c_result, LemClaimOps};
    use core::{
        cmp::min,
        pin::Pin,
        sync::atomic::{AtomicPtr, Ordering},
    };
    use kernel::{
        alloc::{KBox, KVVec},
        bindings,
        error::from_err_ptr,
        ffi::{c_char, c_uint, c_ulong, c_void},
        prelude::*,
        sync::Mutex,
        time::msecs_to_jiffies,
        types::Opaque,
        uaccess::{UserPtr, UserSlice},
    };

    // The RP1 PIO driver's interface (rp1_pio_if.h). The client is an opaque pointer.
    unsafe extern "C" {
        fn rp1_pio_open() -> *mut c_void;
        fn rp1_pio_close(client: *mut c_void);
        fn rp1_pio_sm_config_xfer(client: *mut c_void, sm: c_uint, dir: c_uint, buf_size: c_uint, buf_count: c_uint) -> c_int;
        fn rp1_pio_sm_xfer_data(
            client: *mut c_void,
            sm: c_uint,
            dir: c_uint,
            data_bytes: c_uint,
            data: *mut c_void,
            dma_addr: u64,
            callback: unsafe extern "C" fn(*mut c_void),
            param: *mut c_void,
        ) -> c_int;
        fn rp1_pio_add_program(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_remove_program(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_claim(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_unclaim(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_init(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_set_config(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_exec(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_clear_fifos(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_set_clkdiv(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_set_pins(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_set_pindirs(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_set_enabled(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_restart(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_put(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_get(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_fifo_state(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_drain_tx(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_sm_get_flags(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_gpio_init(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_gpio_set_function(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_gpio_set_pulls(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_gpio_set_outover(client: *mut c_void, param: *mut c_void) -> c_int;
        fn rp1_pio_gpio_set_input_enabled(client: *mut c_void, param: *mut c_void) -> c_int;
    }

    // rs/claims.rs: the claim API (lemnos_internal.h).
    unsafe extern "C" {
        fn lem_claim_held(fd: c_int, resource: *const c_char) -> bool;
        fn lem_claim_attach(fd: c_int, resource: *const c_char, ops: *const LemClaimOps, data: *mut c_void) -> c_int;
        fn lem_claim_attached(file: *mut c_void, ops: *const LemClaimOps) -> *mut c_void;
    }

    /// The claim's resource name (NUL-terminated).
    const RESOURCE: &core::ffi::CStr = c"pio-0";
    const SMS: u32 = 4;
    const DIRS: u32 = 2;
    const GPIOS: u16 = 28;
    const INSTRUCTIONS: u16 = 32;
    const BUF_MAX: u32 = 256 * 1024;
    const BUF_COUNT_MAX: u32 = 64;
    const TIMEOUT_MS: u32 = 1000;
    const TO_SM: u32 = 0;

    /// What an operation's argument block must hold before the driver sees it.
    #[derive(Clone, Copy)]
    enum Check {
        None,
        /// The first u16 is a state machine.
        Sm,
        /// The first u16 is a mask of state machines.
        Mask,
        /// The first u16 is a GPIO.
        Gpio,
        /// The first u16 is `num_instrs` of a program: 1 to the instruction memory's size.
        Program,
    }

    /// One PIO operation: the driver's call and the check of its arguments.
    struct PioOp {
        call: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
        check: Check,
    }

    /// The operations, in the order of `enum lemnos_pio_op` (lemnos.h); the index is the op.
    static OPS: [PioOp; 23] = [
        PioOp { call: rp1_pio_add_program, check: Check::Program },
        PioOp { call: rp1_pio_remove_program, check: Check::None },
        PioOp { call: rp1_pio_sm_claim, check: Check::Mask },
        PioOp { call: rp1_pio_sm_unclaim, check: Check::Mask },
        PioOp { call: rp1_pio_sm_init, check: Check::Sm },
        PioOp { call: rp1_pio_sm_set_config, check: Check::Sm },
        PioOp { call: rp1_pio_sm_exec, check: Check::Sm },
        PioOp { call: rp1_pio_sm_clear_fifos, check: Check::Sm },
        PioOp { call: rp1_pio_sm_set_clkdiv, check: Check::Sm },
        PioOp { call: rp1_pio_sm_set_pins, check: Check::Sm },
        PioOp { call: rp1_pio_sm_set_pindirs, check: Check::Sm },
        PioOp { call: rp1_pio_sm_set_enabled, check: Check::Mask },
        PioOp { call: rp1_pio_sm_restart, check: Check::Mask },
        PioOp { call: rp1_pio_sm_put, check: Check::Sm },
        PioOp { call: rp1_pio_sm_get, check: Check::Sm },
        PioOp { call: rp1_pio_sm_fifo_state, check: Check::Sm },
        PioOp { call: rp1_pio_sm_drain_tx, check: Check::Sm },
        PioOp { call: rp1_pio_sm_get_flags, check: Check::Sm },
        PioOp { call: rp1_pio_gpio_init, check: Check::Gpio },
        PioOp { call: rp1_pio_gpio_set_function, check: Check::Gpio },
        PioOp { call: rp1_pio_gpio_set_pulls, check: Check::Gpio },
        PioOp { call: rp1_pio_gpio_set_outover, check: Check::Gpio },
        PioOp { call: rp1_pio_gpio_set_input_enabled, check: Check::Gpio },
    ];

    /// The client of the driver a claim holds. Closed when the claim's data is freed: the client is
    /// the first field of `LemPio`, so it closes before the DMA buffers are freed.
    struct PioClient(AtomicPtr<c_void>);

    impl Drop for PioClient {
        fn drop(&mut self) {
            let client = self.0.load(Ordering::Relaxed);
            if !client.is_null() {
                // SAFETY: the client came from rp1_pio_open, and this is its one close.
                unsafe { rp1_pio_close(client) };
            }
        }
    }

    /// One state machine's DMA in one direction. The completion is outside the lock: the driver's
    /// callback completes it with no lock held.
    #[pin_data]
    struct LemDma {
        #[pin]
        state: Mutex<DmaState>,
        #[pin]
        done: Opaque<bindings::completion>,
    }

    struct DmaState {
        /// `buf_size` bytes: memory for copies only.
        scratch: KVVec<u8>,
        buf_size: u32,
        /// A transfer timed out: nothing more until reconfigured.
        stuck: bool,
    }

    impl LemDma {
        fn new() -> impl PinInit<Self> {
            pin_init!(LemDma {
                state <- kernel::sync::new_mutex!(DmaState { scratch: KVVec::new(), buf_size: 0, stuck: false }),
                // SAFETY: the slot is the completion being initialised, in place, before any use.
                done <- Opaque::ffi_init(|slot| unsafe { bindings::init_completion(slot) }),
            })
        }
    }

    #[pin_data]
    struct LemPio {
        client: PioClient,
        #[pin]
        dma: [[LemDma; DIRS as usize]; SMS as usize],
    }

    fn new_pio() -> impl PinInit<LemPio> {
        pin_init!(LemPio {
            client: PioClient(AtomicPtr::new(core::ptr::null_mut())),
            dma <- pin_init::pin_init_array_from_fn(|_| pin_init::pin_init_array_from_fn(|_| LemDma::new())),
        })
    }

    /// The claim's data is freed here, once: the claim's release. The client closes in the drop.
    unsafe extern "C" fn release(data: *mut c_void) {
        // SAFETY: `data` came from KBox::into_raw in open, and this is its only free.
        drop(unsafe { KBox::from_raw(data.cast::<LemPio>()) });
    }

    static CLAIM_OPS: LemClaimOps = LemClaimOps { release: Some(release), read: None, poll: None };

    /// The completion's callback: the driver finished a transfer into the channel's buffer.
    unsafe extern "C" fn complete_done(param: *mut c_void) {
        // SAFETY: `param` is the channel's completion, given to the driver by xfer, and it is live
        // until the wait in move_data ends.
        unsafe { bindings::complete(param.cast()) };
    }

    /// A file reference taken for one call (CLASS(fd) in the C): fget here, fput when it drops.
    struct FileRef(*mut bindings::file);

    impl FileRef {
        fn get(fd: c_int) -> Self {
            // SAFETY: fget takes a reference, which Drop gives back. A bad fd gives NULL.
            Self(unsafe { bindings::fget(fd as c_uint) })
        }
    }

    impl Drop for FileRef {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: drops the reference fget took.
                unsafe { bindings::fput(self.0) };
            }
        }
    }

    /// The claim's PIO data, or NULL: the file's claim is on "pio-0" and attached with this module's ops.
    fn attached(file: &FileRef) -> *mut LemPio {
        // SAFETY: `file` holds its reference; a NULL file is refused by lem_claim_attached.
        unsafe { lem_claim_attached(file.0.cast(), &raw const CLAIM_OPS) }.cast()
    }

    fn dma_of(p: &LemPio, sm: u32, dir: u32) -> Option<&LemDma> {
        (sm < SMS && dir < DIRS).then(|| &p.dma[sm as usize][dir as usize])
    }

    pub(super) fn open(o: &LemnosPioOpen) -> Result<c_long> {
        // SAFETY: RESOURCE is NUL-terminated.
        if !unsafe { lem_claim_held(o.claim, RESOURCE.as_ptr().cast()) } {
            return Err(EPERM);
        }
        let p = KBox::pin_init(new_pio(), GFP_KERNEL).map_err(|_| ENOMEM)?;
        // SAFETY: rp1_pio_open takes no arguments; a failure is an error pointer.
        let client = from_err_ptr(unsafe { rp1_pio_open() })?;
        p.client.0.store(client, Ordering::Relaxed);
        // SAFETY: the box is not moved after this: it lives until the claim's release frees it.
        let raw = KBox::into_raw(unsafe { Pin::into_inner_unchecked(p) });
        // SAFETY: `o.claim` is on RESOURCE (checked above); the ops are this module's static table.
        let ret = unsafe { lem_claim_attach(o.claim, RESOURCE.as_ptr().cast(), &raw const CLAIM_OPS, raw.cast()) };
        if let Err(e) = c_result(ret as c_long) {
            // SAFETY: nothing else holds the PIO data, since the attach failed; its drop closes the client.
            unsafe { drop(KBox::from_raw(raw)) };
            return Err(e);
        }
        Ok(0)
    }

    fn args_ok(op: &PioOp, args: &[u8; LEMNOS_PIO_ARGS]) -> bool {
        // The C reads the first u16 of the block in native order: the same here.
        let first = u16::from_ne_bytes([args[0], args[1]]);
        match op.check {
            Check::None => true,
            Check::Sm => first < SMS as u16,
            Check::Mask => first < (1u16 << SMS),
            Check::Gpio => first < GPIOS,
            Check::Program => first != 0 && first <= INSTRUCTIONS,
        }
    }

    pub(super) fn call(c: &mut LemnosPioCall) -> Result<c_long> {
        let file = FileRef::get(c.claim);
        let p = attached(&file);
        if p.is_null() {
            return Err(EPERM);
        }
        let op = OPS.get(c.op as usize).ok_or(EINVAL)?;
        if !args_ok(op, &c.args) {
            return Err(EINVAL);
        }
        // SAFETY: `p` is attached to this claim and lives while `file` holds the claim open; the
        // driver's call reads and writes the argument block, the struct of its own operation.
        let r = unsafe { (op.call)((*p).client.0.load(Ordering::Relaxed), c.args.as_mut_ptr().cast()) };
        c.result = r as i64;
        Ok(0)
    }

    pub(super) fn config_xfer(c: &LemnosPioConfigXfer) -> Result<c_long> {
        let file = FileRef::get(c.claim);
        let p = attached(&file);
        if p.is_null() {
            return Err(EPERM);
        }
        // SAFETY: `p` is attached to this claim and lives while `file` holds the claim open.
        let p = unsafe { &*p };
        let d = dma_of(p, c.sm, c.dir).ok_or(EINVAL)?;
        if c.reserved != 0 || c.buf_size == 0 || c.buf_size > BUF_MAX || c.buf_count == 0 || c.buf_count > BUF_COUNT_MAX {
            return Err(EINVAL);
        }
        let fresh = KVVec::<u8>::zeroed(c.buf_size as usize, GFP_KERNEL).map_err(|_| ENOMEM)?;
        let mut st = d.state.lock();
        // The driver stops anything still running on this channel.
        // SAFETY: the client is this claim's, live while `file` holds the claim open.
        let ret = unsafe { rp1_pio_sm_config_xfer(p.client.0.load(Ordering::Relaxed), c.sm, c.dir, c.buf_size, c.buf_count) };
        if ret != 0 {
            return c_result(ret as c_long);
        }
        let old = core::mem::replace(&mut st.scratch, fresh);
        st.buf_size = c.buf_size;
        st.stuck = false;
        // SAFETY: the reinit of a completion nothing waits on (no transfer can be in flight: the lock
        // is held, and the driver stopped this channel above). The C does the same: done = 0.
        unsafe { (*d.done.get()).done = 0 };
        drop(st);
        drop(old);
        Ok(0)
    }

    /// Moves `x.len` bytes through the channel, `buf_size` at a time. The lock is held by the caller.
    fn move_data(client: *mut c_void, x: &LemnosPioXfer, d: &LemDma, st: &mut DmaState) -> Result<c_long> {
        let timeout: c_ulong = msecs_to_jiffies(if x.timeout_ms != 0 { x.timeout_ms } else { TIMEOUT_MS });
        let to_sm = x.dir == TO_SM;
        let user = UserPtr::from_addr(x.data as usize);
        if st.stuck {
            return Err(EIO);
        }
        if st.buf_size == 0 {
            return Err(EINVAL);
        }
        let mut done: u32 = 0;
        while done < x.len {
            let n = min(x.len - done, st.buf_size) as usize;
            let at = user.wrapping_byte_add(done as usize);
            if to_sm {
                UserSlice::new(at, n).reader().read_slice(&mut st.scratch[..n])?;
            }
            // SAFETY: the scratch holds `n` bytes, and the driver's callback is `done`, given `d.done`,
            // which stays live until the wait below ends.
            let ret = unsafe {
                rp1_pio_sm_xfer_data(client, x.sm, x.dir, n as c_uint, st.scratch.as_mut_ptr().cast(), 0, complete_done, d.done.get().cast())
            };
            if ret != 0 {
                return c_result(ret as c_long);
            }
            // Killable, so a holder stuck on a channel nothing drains can still be ended: the caller's
            // timeout can be long. Either way the transfer may still finish, into this scratch: it is
            // kept until reconfigured, or until the claim's release ends the client.
            // SAFETY: `d.done` is this channel's completion, live while the module holds the claim.
            let left = unsafe { bindings::wait_for_completion_killable_timeout(d.done.get(), timeout) };
            if left <= 0 {
                st.stuck = true;
                return Err(if left != 0 { EINTR } else { ETIMEDOUT });
            }
            if !to_sm {
                UserSlice::new(at, n).writer().write_slice(&st.scratch[..n])?;
            }
            done += n as u32;
        }
        Ok(0)
    }

    pub(super) fn xfer(x: &LemnosPioXfer) -> Result<c_long> {
        let file = FileRef::get(x.claim);
        let p = attached(&file);
        if p.is_null() {
            return Err(EPERM);
        }
        // SAFETY: `p` is attached to this claim and lives while `file` holds the claim open.
        let p = unsafe { &*p };
        let d = dma_of(p, x.sm, x.dir).filter(|_| x.reserved == 0).ok_or(EINVAL)?;
        let mut st = d.state.lock();
        let ret = move_data(p.client.0.load(Ordering::Relaxed), x, d, &mut st);
        drop(st);
        ret
    }
}
