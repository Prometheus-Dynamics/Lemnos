// SPDX-License-Identifier: GPL-2.0
//! A block's clocks, supplies and resets, Rust (stage 1e of the port; replaces lemnos_power.c under CONFIG_RUST).
//!
//! A handle is taken on first use and kept, so later calls allocate nothing. What the caller turns on stays
//! on across a close: only an unbind, which must leave the frameworks balanced, turns off what was turned on
//! here. The core (rs/dev.rs) calls its entry points: lem_power_create, lem_power_set_block,
//! lem_power_request, lem_power_destroy.
//!
//! The kernel calls are the ones the C made. Two are static inline in the headers, so they are composed from
//! their exported parts exactly as the inline bodies do: clk_prepare_enable (clk_prepare, then clk_enable, and
//! clk_unprepare if that fails) and clk_disable_unprepare (clk_disable, then clk_unprepare). reset_control_get_
//! exclusive is an inline over the exported __reset_control_get with the same flags. A supply fault is
//! posted through the event queue (rs/events.rs).

use crate::abi::{c_result, err_ptr, ret};
use core::{ffi::c_void, mem::offset_of, ptr};
use kernel::{
    alloc::KBox,
    bindings,
    error::{code::*, from_err_ptr},
    ffi::{c_char, c_int, c_long, c_ulong},
    prelude::*,
    sync::Mutex,
};

/// lemnos.h: LEMNOS_POWER_NAME, LEMNOS_MAX_POWER, the kinds and the operations.
const LEMNOS_POWER_NAME: usize = 32;
const LEMNOS_MAX_POWER: usize = 12;
const POWER_CLOCK: u32 = 0;
const POWER_SUPPLY: u32 = 1;
const POWER_RESET: u32 = 2;
const OP_ENABLE: u32 = 0;
const OP_DISABLE: u32 = 1;
const OP_STATUS: u32 = 2;
const OP_GET: u32 = 3;
const OP_SET: u32 = 4;
const OP_PULSE: u32 = 5;
/// LEMNOS_EVENT_SUPPLY_FAULT of lemnos.h.
const EVENT_SUPPLY_FAULT: u32 = 3;
/// The REGULATOR_EVENT_* values of include/uapi/regulator/regulator.h that a supply's faults are made of:
/// UNDER_VOLTAGE, OVER_CURRENT, REGULATION_OUT, FAIL, OVER_TEMP, and the UNDER_VOLTAGE_WARN, OVER_CURRENT_WARN,
/// OVER_VOLTAGE_WARN and OVER_TEMP_WARN warnings. Routine notifications are not worth an event.
const LEM_SUPPLY_FAULTS: c_ulong = 0x0001 | 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x2000 | 0x4000 | 0x8000 | 0x10000;
/// RESET_CONTROL_EXCLUSIVE: BIT(2) of include/linux/reset.h.
const RESET_CONTROL_EXCLUSIVE: c_int = 1 << 2;

/// `struct lemnos_power` of lemnos.h (LEMNOS_IOC_POWER).
#[repr(C)]
pub(crate) struct LemnosPower {
    block: u32,
    kind: u32,
    op: u32,
    handle: u32,
    name: [c_char; LEMNOS_POWER_NAME],
    value: u64,
    value2: u64,
}

const _: () = assert!(core::mem::size_of::<LemnosPower>() == 64);

/// One handle: the clock, supply or reset of a name, kept until the power state is destroyed.
struct LemHandle {
    name: [u8; LEMNOS_POWER_NAME],
    kind: u32,
    index: u32,
    on: bool,
    power: *mut LemPower,
    nb: bindings::notifier_block,
    /// The clk, regulator or reset_control, by `kind`.
    res: *mut c_void,
}

impl LemHandle {
    fn empty() -> Self {
        LemHandle {
            name: [0; LEMNOS_POWER_NAME],
            kind: 0,
            index: 0,
            on: false,
            power: ptr::null_mut(),
            // SAFETY: a notifier_block of zeros is the unregistered state (no call, no next, priority 0).
            nb: unsafe { core::mem::zeroed() },
            res: ptr::null_mut(),
        }
    }
}

/// The power state of one block: its device, its block number and the handles, under `lock`.
#[pin_data]
struct LemPower {
    dev: *mut bindings::device,
    block: u32,
    /// n, and each handle's `on`, are under this lock.
    #[pin]
    lock: Mutex<u32>,
    h: [LemHandle; LEMNOS_MAX_POWER],
}

unsafe extern "C" {
    fn clk_get(dev: *mut c_void, id: *const c_char) -> *mut c_void;
    fn clk_put(clk: *mut c_void);
    fn clk_prepare(clk: *mut c_void) -> c_int;
    fn clk_unprepare(clk: *mut c_void);
    fn clk_enable(clk: *mut c_void) -> c_int;
    fn clk_disable(clk: *mut c_void);
    fn clk_get_rate(clk: *mut c_void) -> c_ulong;
    fn clk_round_rate(clk: *mut c_void, rate: c_ulong) -> c_long;
    fn clk_set_rate(clk: *mut c_void, rate: c_ulong) -> c_int;
    fn regulator_get_optional(dev: *mut c_void, id: *const c_char) -> *mut c_void;
    fn regulator_put(supply: *mut c_void);
    fn regulator_register_notifier(supply: *mut c_void, nb: *mut c_void) -> c_int;
    fn regulator_unregister_notifier(supply: *mut c_void, nb: *mut c_void) -> c_int;
    fn regulator_enable(supply: *mut c_void) -> c_int;
    fn regulator_disable(supply: *mut c_void) -> c_int;
    fn regulator_is_enabled(supply: *mut c_void) -> c_int;
    fn regulator_get_voltage(supply: *mut c_void) -> c_int;
    fn regulator_set_voltage(supply: *mut c_void, min_uv: c_int, max_uv: c_int) -> c_int;
    fn __reset_control_get(dev: *mut c_void, id: *const c_char, index: c_int, flags: c_int) -> *mut c_void;
    fn reset_control_put(rstc: *mut c_void);
    fn reset_control_deassert(rstc: *mut c_void) -> c_int;
    fn reset_control_assert(rstc: *mut c_void) -> c_int;
    fn reset_control_reset(rstc: *mut c_void) -> c_int;
    fn reset_control_status(rstc: *mut c_void) -> c_int;
}

/// IS_ERR of linux/err.h: an error pointer is an errno in the top 4095 addresses. Kernel pointers are high,
/// so they are tested as unsigned, never as signed.

/// The notifier of a supply: the fault bits of a supply's event become a supply-fault event.
///
/// # Safety
/// `nb` is the `nb` field of a LemHandle of a live LemPower (registered with the supply).
unsafe extern "C" fn lem_supply_event(nb: *mut bindings::notifier_block, event: c_ulong, _data: *mut c_void) -> c_int {
    // SAFETY: `nb` is the `nb` of a LemHandle (the contract), so the handle is the containing struct.
    let h = unsafe { (nb as *mut u8).sub(offset_of!(LemHandle, nb)) as *mut LemHandle };
    let faults = event & LEM_SUPPLY_FAULTS;
    if faults != 0 {
        // SAFETY: the handle is live while its notifier is registered.
        let power = unsafe { (*h).power };
        // SAFETY: the handle's power outlives its notifier (unregistered before the power is freed).
        let block = unsafe { (*power).block };
        // SAFETY: as power.
        let index = unsafe { (*h).index };
        crate::events::post(EVENT_SUPPLY_FAULT, block, index, faults as i64);
    }
    1 // NOTIFY_OK
}

/// The handle's resource opened under `kind` and `name`, or the errno it failed with.
///
/// # Safety
/// `p` and `h` are live, and `h` is in `p.h`.
unsafe fn lem_handle_open(p: *mut LemPower, h: *mut LemHandle) -> Result {
    // SAFETY: the caller's contract; the name is inside the handle.
    let name_ptr = if unsafe { (*h).name[0] } == 0 { ptr::null() } else { unsafe { (*h).name.as_ptr() } } as *const c_char;
    // SAFETY: the power's device is the one the block belongs to.
    let dev = unsafe { (*p).dev };
    // SAFETY: as above.
    match unsafe { (*h).kind } {
        POWER_CLOCK => {
            // SAFETY: the device and the optional name are valid for clk_get.
            let clk = from_err_ptr(unsafe { clk_get(dev.cast(), name_ptr) })?;
            // SAFETY: the handle is live.
            unsafe { (*h).res = clk };
            Ok(())
        }
        POWER_SUPPLY => {
            if name_ptr.is_null() {
                return Err(EINVAL);
            }
            // SAFETY: the device and name are valid.
            let supply = from_err_ptr(unsafe { regulator_get_optional(dev.cast(), name_ptr) })?;
            // SAFETY: the handle is live.
            unsafe { (*h).res = supply };
            // SAFETY: as above; the notifier is not registered yet, so nothing else reads it.
            unsafe { (*h).nb.notifier_call = Some(lem_supply_event) };
            // SAFETY: the handle is live, so its notifier block is in bounds.
            let nb = unsafe { ptr::addr_of_mut!((*h).nb) };
            // SAFETY: the supply just got; the notifier block stays at its address (the handle is
            // in p.h, which is not moved) until lem_power_destroy unregisters it.
            unsafe { regulator_register_notifier(supply, nb.cast()) };
            Ok(())
        }
        POWER_RESET => {
            // SAFETY: as clk_get; the inline wrapper's arguments: index 0, exclusive.
            let rst = from_err_ptr(unsafe { __reset_control_get(dev.cast(), name_ptr, 0, RESET_CONTROL_EXCLUSIVE) })?;
            // SAFETY: the handle is live.
            unsafe { (*h).res = rst };
            Ok(())
        }
        _ => Err(EINVAL),
    }
}

/// The handle of that kind and name, opened if it is the first use. The caller holds the power's lock,
/// whose guard counts the handles (`n`).
///
/// # Safety
/// `p` is a live LemPower, and `n` is the count under its lock.
unsafe fn lem_handle_find(p: *mut LemPower, n: &mut u32, kind: u32, name: &[u8]) -> Result<*mut LemHandle> {
    for i in 0..*n as usize {
        // SAFETY: `i < n` handles are open and stay at their addresses.
        let h = unsafe { ptr::addr_of_mut!((*p).h[i]) };
        // SAFETY: the handle is live.
        if unsafe { (*h).kind } == kind && cstr_eq(unsafe { &(*h).name }, name) {
            return Ok(h);
        }
    }
    if *n as usize == LEMNOS_MAX_POWER {
        return Err(ENOSPC);
    }
    let i = *n as usize;
    // SAFETY: slot `i` is free; `p` is live.
    let h = unsafe { ptr::addr_of_mut!((*p).h[i]) };
    // SAFETY: the slot is free, and is written in full here, before use.
    unsafe { h.write(LemHandle::empty()) };
    let len = name.len().min(LEMNOS_POWER_NAME - 1);
    // SAFETY: the slot was written just above, and nothing else refers to it until `n` counts it.
    let stored = unsafe { &mut (*h).name };
    stored[..len].copy_from_slice(&name[..len]);
    // SAFETY: the slot is initialised and still ours alone.
    unsafe { (*h).kind = kind };
    // SAFETY: as above.
    unsafe { (*h).index = i as u32 };
    // SAFETY: as above.
    unsafe { (*h).power = p };
    // SAFETY: the handle is in place, at its final address.
    unsafe { lem_handle_open(p, h)? };
    *n += 1;
    Ok(h)
}

/// Whether the NUL-terminated `stored` is `name`.
fn cstr_eq(stored: &[u8], name: &[u8]) -> bool {
    let len = stored.iter().position(|&b| b == 0).unwrap_or(stored.len());
    &stored[..len] == name
}

/// Creates the power state of `dev`; an ERR_PTR-encoded errno on failure.
///
/// # Safety
/// `dev` is the block's device, live for the state's life.
pub(crate) unsafe extern "C" fn lem_power_create(dev: *mut c_void) -> *mut c_void {
    let init = try_pin_init!(LemPower {
        dev: dev.cast(),
        block: 0,
        lock <- kernel::sync::new_mutex!(0u32),
        h: core::array::from_fn(|_| LemHandle::empty()),
    });
    match KBox::pin_init(init, GFP_KERNEL) {
        // SAFETY: the box is on the heap and never moved; it is freed by lem_power_destroy.
        Ok(p) => KBox::into_raw(unsafe { core::pin::Pin::into_inner_unchecked(p) }).cast(),
        Err(e) => err_ptr(e),
    }
}

/// Sets the block number events and faults are reported under.
///
/// # Safety
/// `p` is a live power state from lem_power_create.
pub(crate) unsafe extern "C" fn lem_power_set_block(p: *mut c_void, block: u32) {
    // SAFETY: the caller's contract.
    unsafe { (*(p as *mut LemPower)).block = block };
}

/// LEMNOS_IOC_POWER: one operation on a handle of the block.
///
/// # Safety
/// `p` is a live power state; `r` is the ioctl's argument.
pub(crate) unsafe extern "C" fn lem_power_request(p: *mut c_void, r: *mut LemnosPower) -> c_long {
    // SAFETY: the caller's contract.
    ret(unsafe { power_request(p as *mut LemPower, r) })
}

/// The request, as a Result (the ops' negative errnos are Errs).
///
/// # Safety
/// As lem_power_request.
unsafe fn power_request(p: *mut LemPower, r: *mut LemnosPower) -> Result<c_long> {
    // SAFETY: as the caller's contract.
    c_result(unsafe { power_request_c(p, r) })
}

/// The request's body: a value, or the negative errno the ops returned.
///
/// # Safety
/// As lem_power_request.
unsafe fn power_request_c(p: *mut LemPower, r: *mut LemnosPower) -> c_long {
    // SAFETY: the caller's argument.
    let r = unsafe { &mut *r };
    if r.name.iter().all(|&b| b != 0) {
        return EINVAL.to_errno() as c_long;
    }
    let name: &[u8] = {
        let len = r.name.iter().position(|&b| b == 0).unwrap_or(r.name.len());
        // SAFETY: the first `len` bytes of the name are inside it.
        unsafe { core::slice::from_raw_parts(r.name.as_ptr().cast(), len) }
    };
    // SAFETY: the lock is the power's own; the guard is held for the whole request.
    let mut guard = unsafe { (*p).lock.lock() };
    // SAFETY: the handles are under the lock held in `guard`.
    let h = match unsafe { lem_handle_find(p, &mut *guard, r.kind, name) } {
        Ok(h) => h,
        Err(e) => return e.to_errno() as c_long,
    };
    // SAFETY: the handle is live under the lock.
    let kind = unsafe { (*h).kind };
    let status = match kind {
        // SAFETY: a live clock handle, under the power's lock.
        POWER_CLOCK => unsafe { lem_clock(h, r) },
        // SAFETY: a live supply handle, under the power's lock.
        POWER_SUPPLY => unsafe { lem_supply(h, r) },
        // SAFETY: lem_handle_open makes no other kind, so a live reset handle, under the lock.
        _ => unsafe { lem_reset(h, r) },
    };
    // SAFETY: the handle is live under the lock.
    r.handle = unsafe { (*h).index };
    // ENOTSUPP is the kernel's own; user space knows EOPNOTSUPP.
    if status == -524 {
        // ENOTSUPP (524) is the kernel's own; not a code in the kernel crate's list.
        return EOPNOTSUPP.to_errno() as c_long;
    }
    status
}

/// The clock's operations.
///
/// # Safety
/// `h` is a live clock handle under the power's lock.
unsafe fn lem_clock(h: *mut LemHandle, r: &mut LemnosPower) -> c_long {
    // SAFETY: the handle is live; its `res` is the clock from lem_handle_open.
    let clk = unsafe { (*h).res };
    match r.op {
        OP_ENABLE => {
            // SAFETY: as above; nothing to do if it is on already.
            if unsafe { (*h).on } {
                return 0;
            }
            // clk_prepare_enable, as the inline composes it.
            // SAFETY: the clock is live.
            let ret = unsafe { clk_prepare(clk) };
            if ret != 0 {
                // SAFETY: the handle is live under the power's lock (the caller's contract).
                unsafe { (*h).on = false };
                return ret as c_long;
            }
            // SAFETY: the clock is prepared.
            let ret = unsafe { clk_enable(clk) };
            if ret != 0 {
                // SAFETY: undoes the prepare just made.
                unsafe { clk_unprepare(clk) };
            }
            // SAFETY: the handle is live under the power's lock (the caller's contract).
            unsafe { (*h).on = ret == 0 };
            ret as c_long
        }
        OP_DISABLE => {
            // SAFETY: the clock is live; clk_disable_unprepare, as the inline composes it.
            if unsafe { (*h).on } {
                // SAFETY: the clock is on, so it is enabled.
                unsafe { clk_disable(clk) };
                // SAFETY: the clock is on, so it is prepared, and it was disabled just above.
                unsafe { clk_unprepare(clk) };
            }
            // SAFETY: the handle is live under the power's lock (the caller's contract).
            unsafe { (*h).on = false };
            0
        }
        OP_STATUS => {
            // SAFETY: the handle is live under the power's lock (the caller's contract).
            r.value = unsafe { (*h).on } as u64;
            0
        }
        OP_GET => {
            // SAFETY: the clock is live.
            r.value = unsafe { clk_get_rate(clk) } as u64;
            0
        }
        OP_SET => {
            // SAFETY: the clock is live.
            let rate = unsafe { clk_round_rate(clk, r.value as c_ulong) };
            if rate < 0 {
                return rate;
            }
            // SAFETY: as above.
            let ret = unsafe { clk_set_rate(clk, rate as c_ulong) };
            if ret != 0 {
                return ret as c_long;
            }
            // SAFETY: the clock is live.
            r.value = unsafe { clk_get_rate(clk) } as u64;
            0
        }
        _ => EINVAL.to_errno() as c_long,
    }
}

/// The supply's operations.
///
/// # Safety
/// `h` is a live supply handle under the power's lock.
unsafe fn lem_supply(h: *mut LemHandle, r: &mut LemnosPower) -> c_long {
    // SAFETY: the handle is live; `res` is the regulator from lem_handle_open.
    let s = unsafe { (*h).res };
    match r.op {
        OP_ENABLE => {
            // SAFETY: the handle is live.
            if unsafe { (*h).on } {
                return 0;
            }
            // SAFETY: the regulator is live.
            let ret = unsafe { regulator_enable(s) };
            // SAFETY: the handle is live under the power's lock (the caller's contract).
            unsafe { (*h).on = ret == 0 };
            ret as c_long
        }
        OP_DISABLE => {
            // SAFETY: the handle is live.
            if !unsafe { (*h).on } {
                return 0;
            }
            // SAFETY: the regulator is live.
            let ret = unsafe { regulator_disable(s) };
            // SAFETY: the handle is live under the power's lock (the caller's contract).
            unsafe { (*h).on = ret != 0 };
            ret as c_long
        }
        OP_STATUS => {
            // SAFETY: the regulator is live.
            let ret = unsafe { regulator_is_enabled(s) };
            if ret < 0 {
                return ret as c_long;
            }
            r.value = ret as u64;
            0
        }
        OP_GET => {
            // SAFETY: the regulator is live.
            let ret = unsafe { regulator_get_voltage(s) };
            if ret < 0 {
                return ret as c_long;
            }
            r.value = ret as u64;
            0
        }
        OP_SET => {
            if r.value > i32::MAX as u64 || r.value2 > i32::MAX as u64 || r.value > r.value2 {
                return EINVAL.to_errno() as c_long;
            }
            // SAFETY: the regulator is live; the bounds were checked above.
            let ret = unsafe { regulator_set_voltage(s, r.value as c_int, r.value2 as c_int) };
            if ret != 0 {
                return ret as c_long;
            }
            // SAFETY: as above.
            let ret = unsafe { regulator_get_voltage(s) };
            if ret < 0 {
                return ret as c_long;
            }
            r.value = ret as u64;
            0
        }
        _ => EINVAL.to_errno() as c_long,
    }
}

/// The reset's operations.
///
/// # Safety
/// `h` is a live reset handle under the power's lock.
unsafe fn lem_reset(h: *mut LemHandle, r: &mut LemnosPower) -> c_long {
    // SAFETY: the handle is live; `res` is the reset control from lem_handle_open.
    let rst = unsafe { (*h).res };
    match r.op {
        // SAFETY: the reset control is live.
        OP_ENABLE => unsafe { reset_control_deassert(rst) as c_long },
        // SAFETY: as above.
        OP_DISABLE => unsafe { reset_control_assert(rst) as c_long },
        // SAFETY: as above.
        OP_PULSE => unsafe { reset_control_reset(rst) as c_long },
        OP_STATUS => {
            // SAFETY: as above.
            let ret = unsafe { reset_control_status(rst) };
            if ret < 0 {
                return ret as c_long;
            }
            r.value = ret as u64;
            0
        }
        _ => EINVAL.to_errno() as c_long,
    }
}

/// Frees the power state: each handle's release, in the C order (the notifier first, then the disable, then
/// the put).
///
/// # Safety
/// `p` is a live power state from lem_power_create, not used after this call.
pub(crate) unsafe extern "C" fn lem_power_destroy(p: *mut c_void) {
    let p = p as *mut LemPower;
    // SAFETY: the caller's contract; the lock is not contended once the state is being destroyed.
    let n = unsafe { *(*p).lock.lock() };
    for i in 0..n as usize {
        // SAFETY: `i < n` handles are open.
        let h = unsafe { ptr::addr_of_mut!((*p).h[i]) };
        // SAFETY: the handle is live until the state is freed below, and nothing else uses it now.
        match unsafe { (*h).kind } {
            POWER_CLOCK => {
                // SAFETY: the handle is live.
                if unsafe { (*h).on } {
                    // SAFETY: the handle is live.
                    let clk = unsafe { (*h).res };
                    // SAFETY: the handle's clock, which is on, so it is enabled.
                    unsafe { clk_disable(clk) };
                    // SAFETY: the handle is live.
                    let clk = unsafe { (*h).res };
                    // SAFETY: the handle's clock, prepared and disabled just above.
                    unsafe { clk_unprepare(clk) };
                }
                // SAFETY: the handle is live.
                let clk = unsafe { (*h).res };
                // SAFETY: the clock lem_handle_open got, put once.
                unsafe { clk_put(clk) };
            }
            POWER_SUPPLY => {
                // SAFETY: the handle is live.
                let supply = unsafe { (*h).res };
                // SAFETY: the handle is live, so its notifier block is in bounds.
                let nb = unsafe { ptr::addr_of_mut!((*h).nb) };
                // SAFETY: the notifier lem_handle_open registered with this supply.
                unsafe { regulator_unregister_notifier(supply, nb.cast()) };
                // SAFETY: the handle is live.
                if unsafe { (*h).on } {
                    // SAFETY: the handle is live.
                    let supply = unsafe { (*h).res };
                    // SAFETY: the handle's supply, which is on, so this drops its one enable.
                    unsafe { regulator_disable(supply) };
                }
                // SAFETY: the handle is live.
                let supply = unsafe { (*h).res };
                // SAFETY: the supply lem_handle_open got, put once, its notifier gone.
                unsafe { regulator_put(supply) };
            }
            _ => {
                // SAFETY: the handle is live.
                let rst = unsafe { (*h).res };
                // SAFETY: the reset control lem_handle_open got, put once.
                unsafe { reset_control_put(rst) };
            }
        }
    }
    // SAFETY: the state came from KBox::into_raw in lem_power_create and is freed here, once.
    drop(unsafe { KBox::from_raw(p) });
}
