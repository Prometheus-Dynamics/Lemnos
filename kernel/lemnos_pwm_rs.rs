// SPDX-License-Identifier: GPL-2.0
//! PWM channels, Rust (stage 1c of the port; replaces lemnos_pwm.c under CONFIG_RUST).
//!
//! A channel is a PWM device held by a claim: `lem_pwm_request` finds the chip (the "pwm" class child of
//! the named platform device) through of_pwm_xlate_with_flags, and attaches it to the claim. The three
//! entry points keep their C signatures (lemnos_internal.h). The kernel's own PWM functions are called
//! through the bindings, exactly as the C called them; the two inline helpers (bus_find_device_by_name,
//! pwm_get_state) are one-line shims in lemnos_helpers.c.

use core::{ffi::c_void, mem::offset_of, ptr};
use kernel::{
    alloc::KBox,
    bindings,
    error::{code::*, from_err_ptr},
    ffi::{c_char, c_int, c_long, c_uint},
    prelude::*,
    str::CStr,
};

/// LEMNOS_NAME_MAX of lemnos.h.
const LEMNOS_NAME_MAX: usize = 64;
/// The resource name a channel is claimed under: the C snprintf's buffer.
const LEM_PWM_RESOURCE_MAX: usize = LEMNOS_NAME_MAX;

/// `struct lemnos_pwm_request` of lemnos.h (LEMNOS_IOC_PWM_REQUEST).
#[repr(C)]
pub struct LemnosPwmRequest {
    claim: i32,
    channel: u32,
    provider: [c_char; LEMNOS_NAME_MAX - 16],
}

/// `struct lemnos_pwm_state` of lemnos.h (LEMNOS_IOC_PWM_APPLY and LEMNOS_IOC_PWM_GET).
#[repr(C)]
pub struct LemnosPwmState {
    claim: i32,
    enabled: u32,
    inverted: u32,
    reserved: u32,
    period_ns: u64,
    duty_ns: u64,
}

const _: () = assert!(core::mem::size_of::<LemnosPwmRequest>() == 56);
const _: () = assert!(core::mem::size_of::<LemnosPwmState>() == 32);

/// The claim's operations table, as lemnos_claims_rs.rs reads it (`struct lem_claim_ops`): three pointers.
/// Only `release` is set for a channel; `read` and `poll` are NULL.
#[repr(C)]
struct LemClaimOps {
    release: Option<unsafe extern "C" fn(*mut c_void)>,
    read: Option<unsafe extern "C" fn()>,
    poll: Option<unsafe extern "C" fn()>,
}

const _: () = assert!(core::mem::size_of::<LemClaimOps>() == 3 * core::mem::size_of::<usize>());

/// One channel, the claim's private data: the PWM device, held until the claim ends.
#[repr(C)]
struct LemChannel {
    pwm: *mut bindings::pwm_device,
}

/// A static table, written once at compile time and only read after.
struct OpsCell(LemClaimOps);

// SAFETY: the table holds function pointers and nulls only, and nothing writes it.
unsafe impl Sync for OpsCell {}

static LEM_CHANNEL_OPS: OpsCell = OpsCell(LemClaimOps {
    release: Some(lem_channel_release),
    read: None,
    poll: None,
});

unsafe extern "C" {
    /// lemnos_claims_rs.rs: the claim functions the C subsystems use, exported with the C signatures.
    fn lem_claim_held(fd: c_int, resource: *const c_char) -> bool;
    fn lem_claim_attach(fd: c_int, resource: *const c_char, ops: *const LemClaimOps, data: *mut c_void) -> c_int;
    fn lem_claim_attached(file: *mut c_void, ops: *const LemClaimOps) -> *mut c_void;
    /// lemnos_helpers.c: one-line shims for the two inline kernel helpers (see the file).
    fn lemnos_bus_find_device_by_name(bus: *const c_void, name: *const c_char) -> *mut c_void;
    fn lemnos_pwm_get_state(pwm: *const c_void, state: *mut c_void);
    /// The bus the platform devices are on: an exported data symbol.
    static platform_bus_type: [u8; 0];
}

/// Writes `"pwm-<provider>:<channel>"` into `out`, truncated the way snprintf truncates, NUL-terminated.
fn resource_name(out: &mut [u8; LEM_PWM_RESOURCE_MAX], provider: &[u8], channel: u32) {
    let mut n = 0usize;
    let mut put = |b: u8| {
        if n + 1 < out.len() {
            out[n] = b;
            n += 1;
        }
    };
    for &b in b"pwm-" {
        put(b);
    }
    for &b in provider {
        if b == 0 {
            break;
        }
        put(b);
    }
    put(b':');
    let mut digits = [0u8; 10];
    let mut d = 0usize;
    let mut v = channel;
    loop {
        digits[d] = b'0' + (v % 10) as u8;
        d += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while d > 0 {
        d -= 1;
        put(digits[d]);
    }
    out[n] = 0;
}

/// The "pwm" class child of a platform device: the chip, as device_find_child's match.
///
/// # Safety
/// `dev` is a live device under the parent device_find_child walks.
unsafe extern "C" fn lem_is_pwm_chip(dev: *mut bindings::device, _data: *const c_void) -> c_int {
    // SAFETY: the caller's contract; reads the class and its name.
    unsafe {
        let class = (*dev).class;
        if class.is_null() || (*class).name.is_null() {
            return 0;
        }
        (CStr::from_char_ptr((*class).name) == c"pwm") as c_int
    }
}

/// The PWM device for `provider`'s channel `channel`, found through its chip; an errno if it is not.
fn lem_pwm_find(provider: &[u8], channel: u32) -> Result<*mut bindings::pwm_device> {
    let mut name = [0u8; LEMNOS_NAME_MAX];
    let n = provider.iter().position(|&b| b == 0).unwrap_or(provider.len()).min(LEMNOS_NAME_MAX - 1);
    name[..n].copy_from_slice(&provider[..n]);
    // SAFETY: `name` is NUL-terminated (its last byte is 0 by construction); the bus is the platform bus.
    let parent = unsafe { lemnos_bus_find_device_by_name(ptr::addr_of!(platform_bus_type).cast(), name.as_ptr().cast()) } as *mut bindings::device;
    if parent.is_null() {
        return Err(ENODEV);
    }
    // SAFETY: `parent` is a referenced device; device_find_child returns a referenced child, or NULL.
    let dev = unsafe { bindings::device_find_child(parent, ptr::null(), Some(lem_is_pwm_chip)) };
    // SAFETY: drops the reference bus_find_device_by_name took on `parent`.
    unsafe { bindings::put_device(parent) };
    if dev.is_null() {
        return Err(ENODEV);
    }
    let args = bindings::of_phandle_args {
        np: ptr::null_mut(),
        args_count: 1,
        args: [channel, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    // SAFETY: `dev` is the chip's device, a child of the chip's parent; the chip is its container.
    let chip = unsafe { (dev as *mut u8).sub(offset_of!(bindings::pwm_chip, dev)) as *mut bindings::pwm_chip };
    // SAFETY: `chip` is the chip `dev` belongs to; `args` is the one argument the xlate reads.
    let pwm = unsafe { bindings::of_pwm_xlate_with_flags(chip, &args) };
    // SAFETY: drops the reference device_find_child took on `dev`.
    unsafe { bindings::put_device(dev) };
    // The xlate returns an ERR_PTR on failure: the kernel's from_err_ptr gives its errno, as PTR_ERR did.
    from_err_ptr(pwm)
}

/// The channel's release: the PWM reference, then the channel. The output is left as it is.
///
/// # Safety
/// `data` is a channel from `lem_pwm_request`, released once, by the claim.
unsafe extern "C" fn lem_channel_release(data: *mut c_void) {
    let c = data as *mut LemChannel;
    // SAFETY: `c` is a channel made by lem_pwm_request, freed only here.
    unsafe { bindings::pwm_put((*c).pwm) };
    // SAFETY: `c` came from KBox::into_raw in lem_pwm_request.
    drop(unsafe { KBox::from_raw(c) });
}

/// `LEMNOS_IOC_PWM_REQUEST`: claims channel `r->channel` of `r->provider`.
///
/// # Safety
/// `r` is the ioctl's argument, from the copy.
#[no_mangle]
pub unsafe extern "C" fn lem_pwm_request(r: *mut LemnosPwmRequest) -> c_long {
    // SAFETY: the caller's argument.
    let r = unsafe { &*r };
    let provider = &r.provider;
    let len = provider.iter().position(|&b| b == 0).unwrap_or(provider.len());
    if len == 0 || len == provider.len() {
        return EINVAL.to_errno() as c_long;
    }
    // SAFETY: the first `len` bytes of `provider` are the name, inside the array.
    let provider_bytes: &[u8] = unsafe { core::slice::from_raw_parts(provider.as_ptr().cast(), len) };
    let mut resource = [0u8; LEM_PWM_RESOURCE_MAX];
    resource_name(&mut resource, provider_bytes, r.channel);
    // SAFETY: `resource` is NUL-terminated.
    if !unsafe { lem_claim_held(r.claim, resource.as_ptr().cast()) } {
        return EPERM.to_errno() as c_long;
    }
    let pwm = match lem_pwm_find(provider_bytes, r.channel) {
        Ok(pwm) => pwm,
        Err(e) => return e.to_errno() as c_long,
    };
    let c = match KBox::new(LemChannel { pwm }, GFP_KERNEL) {
        Ok(c) => KBox::into_raw(c),
        Err(_) => {
            // SAFETY: the reference of a channel that was never made is dropped here.
            unsafe { bindings::pwm_put(pwm) };
            return ENOMEM.to_errno() as c_long;
        }
    };
    // SAFETY: the name is NUL-terminated; the ops table is the static above; `c` is freed by its release.
    let ret = unsafe { lem_claim_attach(r.claim, resource.as_ptr().cast(), &LEM_CHANNEL_OPS.0, c.cast()) };
    if ret != 0 {
        // SAFETY: the attach failed, so nothing else holds `c`: release it as the C did.
        unsafe { lem_channel_release(c.cast()) };
    }
    ret as c_long
}

/// The file's channel, or NULL. `fget` takes a reference; the caller drops it with `fput`.
///
/// # Safety
/// `fd` is an fd number from the ioctl argument.
unsafe fn channel_of(fd: i32) -> (*mut bindings::file, *mut LemChannel) {
    // SAFETY: fget takes a reference on the file, or returns NULL.
    let file = unsafe { bindings::fget(fd as c_uint) };
    // SAFETY: `file` is live until fput; the claim lookup reads its operations only.
    let c = unsafe { lem_claim_attached(file.cast(), &LEM_CHANNEL_OPS.0) } as *mut LemChannel;
    (file, c)
}

/// `LEMNOS_IOC_PWM_APPLY`: applies the state to the channel the claim holds.
///
/// # Safety
/// `s` is the ioctl's argument, from the copy.
#[no_mangle]
pub unsafe extern "C" fn lem_pwm_apply(s: *const LemnosPwmState) -> c_long {
    // SAFETY: the caller's argument.
    let s = unsafe { &*s };
    // SAFETY: the claim is an fd from the ioctl; the reference is dropped below.
    let (file, c) = unsafe { channel_of(s.claim) };
    let ret = if c.is_null() {
        EPERM.to_errno() as c_long
    } else if s.enabled > 1 || s.inverted > 1 || s.reserved != 0 {
        EINVAL.to_errno() as c_long
    } else {
        let state = bindings::pwm_state {
            period: s.period_ns,
            duty_cycle: s.duty_ns,
            polarity: if s.inverted != 0 {
                bindings::pwm_polarity_PWM_POLARITY_INVERSED
            } else {
                bindings::pwm_polarity_PWM_POLARITY_NORMAL
            },
            enabled: s.enabled != 0,
            usage_power: false,
        };
        // SAFETY: `c` is the held channel, live until the claim's file is put below; `state` is a full state.
        unsafe { bindings::pwm_apply_might_sleep((*c).pwm, &state) as c_long }
    };
    if !file.is_null() {
        // SAFETY: drops the reference channel_of took.
        unsafe { bindings::fput(file) };
    }
    ret
}

/// `LEMNOS_IOC_PWM_GET`: the state the channel last had.
///
/// # Safety
/// `s` is the ioctl's argument, from the copy.
#[no_mangle]
pub unsafe extern "C" fn lem_pwm_get(s: *mut LemnosPwmState) -> c_long {
    // SAFETY: the caller's argument.
    let s = unsafe { &mut *s };
    // SAFETY: as lem_pwm_apply.
    let (file, c) = unsafe { channel_of(s.claim) };
    let ret = if c.is_null() {
        EPERM.to_errno() as c_long
    } else {
        let mut state = bindings::pwm_state {
            period: 0,
            duty_cycle: 0,
            polarity: 0,
            enabled: false,
            usage_power: false,
        };
        // SAFETY: `c` is the held channel; `state` is written in full by the shim.
        unsafe { lemnos_pwm_get_state((*c).pwm.cast(), (&mut state as *mut bindings::pwm_state).cast()) };
        s.enabled = state.enabled as u32;
        s.inverted = (state.polarity == bindings::pwm_polarity_PWM_POLARITY_INVERSED) as u32;
        s.period_ns = state.period;
        s.duty_ns = state.duty_cycle;
        0
    };
    if !file.is_null() {
        // SAFETY: drops the reference channel_of took.
        unsafe { bindings::fput(file) };
    }
    ret
}
