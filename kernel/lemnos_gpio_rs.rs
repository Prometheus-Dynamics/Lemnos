// SPDX-License-Identifier: GPL-2.0
//! GPIO lines, Rust (stage CZ-MOD stage 1d of the port; replaces lemnos_gpio.c under CONFIG_RUST).
//!
//! A line is taken the way any module outside a device tree takes one: gpio_request() on the chip's
//! base plus the offset, since gpiod_request() is not exported. gpiolib refuses a line someone holds.
//! The line belongs to a claim (lemnos_claims_rs.rs): the claim's file reads the line's edges, polls,
//! configures, gets and sets it. The four C entry points keep their signatures (lemnos_internal.h):
//! lem_gpio_request, lem_gpio_config, lem_gpio_get and lem_gpio_set.
//!
//! No C is needed for this file. The inline kernel APIs the C used are computed here where their
//! header makes them a formula: ktime_get_ns() over ktime_get(), pinconf_to_config_packed() and
//! dev_name() (see the comments on each).

use core::{
    fmt::Write,
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};
use kernel::{
    alloc::KBox,
    bindings,
    error::{code::*, from_err_ptr},
    ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void},
    fs::file::File,
    prelude::*,
    str::CStr,
    sync::{
        poll::{PollCondVar, PollTable},
        Mutex, SpinLockIrq,
    },
    uaccess::{UserPtr, UserSlice},
};

/// `enum` values of lemnos.h (LEMNOS_GPIO_*), the flags of a line.
const OUTPUT: u32 = 1 << 0;
const ACTIVE_LOW: u32 = 1 << 1;
const PULL_UP: u32 = 1 << 2;
const PULL_DOWN: u32 = 1 << 3;
const BIAS_OFF: u32 = 1 << 4;
const OPEN_DRAIN: u32 = 1 << 5;
const OPEN_SOURCE: u32 = 1 << 6;
const RISING: u32 = 1 << 7;
const FALLING: u32 = 1 << 8;

const LEM_GPIO_RESOURCE_MAX: usize = 40;
const LEM_GPIO_LABEL_MAX: usize = 16;
const LEM_GPIO_EVENTS: usize = 16;
const LEM_GPIO_BIAS: u32 = PULL_UP | PULL_DOWN | BIAS_OFF;
const LEM_GPIO_DRIVE: u32 = OPEN_DRAIN | OPEN_SOURCE;
const LEM_GPIO_EDGES: u32 = RISING | FALLING;
const LEM_GPIO_FLAGS: u32 = OUTPUT | ACTIVE_LOW | LEM_GPIO_BIAS | LEM_GPIO_DRIVE | LEM_GPIO_EDGES;

/// `struct lemnos_gpio_request` of lemnos.h (LEMNOS_IOC_GPIO_REQUEST).
#[repr(C)]
pub struct LemnosGpioRequest {
    claim: c_int,
    chip: u32,
    offset: u32,
    reserved: u32,
}

/// `struct lemnos_gpio_config` of lemnos.h (LEMNOS_IOC_GPIO_CONFIG).
#[repr(C)]
pub struct LemnosGpioConfig {
    claim: c_int,
    flags: u32,
    value: u32,
    debounce_us: u32,
}

/// `struct lemnos_gpio_value` of lemnos.h (LEMNOS_IOC_GPIO_GET and _SET).
#[repr(C)]
pub struct LemnosGpioValue {
    claim: c_int,
    value: u32,
}

/// `struct lemnos_gpio_event` of lemnos.h: what read() on a line's claim returns.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LemnosGpioEvent {
    ns: u64,
    seq: u32,
    rising: u32,
}

const _: () = assert!(size_of::<LemnosGpioEvent>() == 16);

/// `struct lem_claim_ops` of lemnos_internal.h, as lemnos_claims_rs.rs declares it: three pointers.
#[repr(C)]
struct LemClaimOps {
    release: Option<unsafe extern "C" fn(*mut c_void)>,
    read: Option<unsafe extern "C" fn(*mut c_void, *mut bindings::file, *mut c_char, usize) -> isize>,
    poll: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut bindings::file,
            *mut bindings::poll_table_struct,
        ) -> bindings::__poll_t,
    >,
}

const _: () = assert!(size_of::<LemClaimOps>() == 3 * size_of::<usize>());

unsafe extern "C" {
    /// lemnos_claims_rs.rs: the claim API (lemnos_internal.h).
    fn lem_claim_held(fd: c_int, resource: *const c_char) -> bool;
    fn lem_claim_attach(
        fd: c_int,
        resource: *const c_char,
        ops: *const LemClaimOps,
        data: *mut c_void,
    ) -> c_int;
    fn lem_claim_attached(file: *mut c_void, ops: *const LemClaimOps) -> *mut c_void;

    /// gpiolib's legacy numbering (gpio/legacy.h, exported under CONFIG_GPIOLIB), and the consumer
    /// calls that are exported symbols (gpio/consumer.h).
    fn gpio_request(gpio: c_uint, label: *const c_char) -> c_int;
    fn gpio_free(gpio: c_uint);
    fn gpiod_direction_input(desc: *mut c_void) -> c_int;
    fn gpiod_direction_output_raw(desc: *mut c_void, value: c_int) -> c_int;
    fn gpiod_get_raw_value_cansleep(desc: *const c_void) -> c_int;
    fn gpiod_set_raw_value_cansleep(desc: *mut c_void, value: c_int) -> c_int;
    fn gpiod_set_config(desc: *mut c_void, config: c_ulong) -> c_int;
    fn gpiod_set_debounce(desc: *mut c_void, debounce: c_uint) -> c_int;
    fn gpiod_to_irq(desc: *const c_void) -> c_int;
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

/// A NUL-terminated string formatted into N bytes, truncated like snprintf(). Allocates nothing.
struct CBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Write for CBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            if self.len + 1 >= N {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        self.buf[self.len] = 0;
        Ok(())
    }
}

fn format_c<const N: usize>(args: core::fmt::Arguments<'_>) -> CBuf<N> {
    let mut b = CBuf { buf: [0; N], len: 0 };
    let _ = b.write_fmt(args);
    b
}

impl<const N: usize> CBuf<N> {
    fn as_ptr(&self) -> *const c_char {
        self.buf.as_ptr().cast()
    }
}

/// The edge queue, under `events`: the thread handler appends, read takes, poll looks.
struct Queue {
    seq: u32,
    head: usize,
    count: usize,
    events: [LemnosGpioEvent; LEM_GPIO_EVENTS],
}

/// The configuration, under `config`: the edge interrupt is requested and freed only under it.
struct LineConfig {
    irq: c_int,
}

/// One GPIO line: the claim's data. `desc` and `gpio` are set once, by lem_gpio_request.
#[pin_data]
struct LemLine {
    desc: *mut bindings::gpio_desc,
    gpio: c_int,
    #[pin]
    config: Mutex<LineConfig>,
    active_low: AtomicBool,
    /// LEMNOS_GPIO_* as last configured; read without the lock, as the C did (READ_ONCE).
    flags: AtomicU32,
    /// The hard handler's time of the last edge, 0 when none is waiting.
    stamp: AtomicU64,
    #[pin]
    events: SpinLockIrq<Queue>,
    #[pin]
    wait: PollCondVar,
    label: [u8; LEM_GPIO_LABEL_MAX],
}

/// The label gpiolib shows for a line we hold: the C's strscpy of "lemnos".
fn lemnos_label() -> [u8; LEM_GPIO_LABEL_MAX] {
    let mut label = [0u8; LEM_GPIO_LABEL_MAX];
    label[..6].copy_from_slice(b"lemnos");
    label
}

fn new_line(desc: *mut bindings::gpio_desc, gpio: c_int) -> impl PinInit<LemLine, Error> {
    try_pin_init!(LemLine {
        desc,
        gpio,
        config <- kernel::sync::new_mutex!(LineConfig { irq: 0 }),
        active_low: AtomicBool::new(false),
        flags: AtomicU32::new(0),
        stamp: AtomicU64::new(0),
        events <- kernel::sync::new_spinlock_irq!(Queue {
            seq: 0,
            head: 0,
            count: 0,
            events: [LemnosGpioEvent::default(); LEM_GPIO_EVENTS],
        }),
        wait <- PollCondVar::new(c"lemnos-gpio", kernel::static_lock_class!()),
        label: lemnos_label(),
    })
}

/// ktime_get_ns() of linux/timekeeping.h, which is an inline over ktime_get(): the same value.
fn ktime_get_ns() -> u64 {
    // SAFETY: ktime_get is exported and safe outside NMI context, where this runs.
    unsafe { bindings::ktime_get() as u64 }
}

/// The line's edge handler, hard context: only stamps the time. The thread does the rest.
///
/// # Safety
/// `data` is the `LemLine` this handler was requested with, live until the IRQ is freed.
unsafe extern "C" fn lem_line_edge(_irq: c_int, data: *mut c_void) -> bindings::irqreturn {
    // SAFETY: the caller's contract.
    let l = unsafe { &*(data as *const LemLine) };
    l.stamp.store(ktime_get_ns(), Ordering::Relaxed);
    bindings::irqreturn::IRQ_WAKE_THREAD
}

/// The line's edge handler, thread context: queues the edge with its stamp and wakes the readers.
///
/// # Safety
/// As lem_line_edge.
unsafe extern "C" fn lem_line_thread(_irq: c_int, data: *mut c_void) -> bindings::irqreturn {
    // SAFETY: the caller's contract.
    let l = unsafe { &*(data as *const LemLine) };
    // SAFETY: the line's descriptor is held for the line's life.
    let level = unsafe { gpiod_get_raw_value_cansleep((l.desc).cast()) };
    // A nested controller has no hard handler to have stamped the edge.
    let ns = match l.stamp.swap(0, Ordering::SeqCst) {
        0 => ktime_get_ns(),
        ns => ns,
    };
    if level < 0 {
        return bindings::irqreturn::IRQ_HANDLED;
    }
    {
        let mut q = l.events.lock();
        // Full: the newest one is lost, and the seq gap says so.
        q.seq = q.seq.wrapping_add(1);
        if q.count < LEM_GPIO_EVENTS {
            let at = (q.head + q.count) % LEM_GPIO_EVENTS;
            q.events[at] = LemnosGpioEvent {
                ns,
                seq: q.seq,
                rising: ((level ^ l.active_low.load(Ordering::Relaxed) as c_int) != 0) as u32,
            };
            q.count += 1;
        }
    }
    l.wait.notify_all();
    bindings::irqreturn::IRQ_HANDLED
}

/// Stops the line's edge interrupt, if one is requested. Called with the configuration held.
fn stop_edges(l: &LemLine, cfg: &mut LineConfig) {
    if cfg.irq != 0 {
        // SAFETY: cfg.irq was requested with `l` as its dev_id, and is freed here once.
        unsafe { bindings::free_irq(cfg.irq as c_uint, (l as *const LemLine).cast_mut().cast()) };
    }
    cfg.irq = 0;
}

/// The claim's release: the line is let go and freed. Also the error path of lem_gpio_request.
///
/// # Safety
/// `data` is a line made by lem_gpio_request with KBox::into_raw; this is its one free.
unsafe extern "C" fn lem_line_release(data: *mut c_void) {
    let l = data as *mut LemLine;
    // SAFETY: `l` is live until KBox::from_raw below.
    let line = unsafe { &*l };
    {
        let mut cfg = line.config.lock();
        stop_edges(line, &mut cfg);
    }
    // SAFETY: the line is the one lem_gpio_request requested, and is not yet freed.
    unsafe { gpio_free(line.gpio as c_uint) };
    // SAFETY: `l` came from KBox::into_raw, and this is its only free.
    drop(unsafe { KBox::from_raw(l) });
}

fn events_ready(l: &LemLine) -> bool {
    l.events.lock().count > 0
}

/// read() on a line's claim: whole events, blocking unless O_NONBLOCK.
///
/// # Safety
/// `data` is the line the claim is attached to; `file` is the claim's live file; `buf` is the
/// caller's buffer of `len` bytes, as the VFS passes it.
unsafe extern "C" fn lem_line_read(
    data: *mut c_void,
    file: *mut bindings::file,
    buf: *mut c_char,
    len: usize,
) -> isize {
    // SAFETY: the caller's contract.
    let l = unsafe { &*(data as *const LemLine) };
    let want = (len / size_of::<LemnosGpioEvent>()).min(LEM_GPIO_EVENTS);
    if want == 0 {
        return EINVAL.to_errno() as isize;
    }
    let mut ev = [LemnosGpioEvent::default(); LEM_GPIO_EVENTS];
    let mut n = 0;
    loop {
        let mut q = l.events.lock();
        while n < want && q.count > 0 {
            ev[n] = q.events[q.head];
            q.head = (q.head + 1) % LEM_GPIO_EVENTS;
            q.count -= 1;
            n += 1;
        }
        if n > 0 {
            break;
        }
        // SAFETY: `file` is the claim's live file.
        if unsafe { (*file).f_flags } & (bindings::O_NONBLOCK as c_uint) != 0 {
            return EAGAIN.to_errno() as isize;
        }
        // Waits with the queue's lock released and re-takes it on wake, so no edge is missed.
        if l.wait.wait_interruptible(&mut q) {
            return ERESTARTSYS.to_errno() as isize;
        }
    }
    let bytes = n * size_of::<LemnosGpioEvent>();
    // SAFETY: `ev` holds `n` initialised events, and the events are plain integers in repr(C).
    let src = unsafe { core::slice::from_raw_parts(ev.as_ptr().cast::<u8>(), bytes) };
    match UserSlice::new(UserPtr::from_addr(buf as usize), bytes).writer().write_slice(src) {
        Ok(()) => bytes as isize,
        Err(_) => EFAULT.to_errno() as isize,
    }
}

/// poll() on a line's claim: readable when an event is queued.
///
/// # Safety
/// As lem_line_read; `wait` is the poll table the VFS passed.
unsafe extern "C" fn lem_line_poll(
    data: *mut c_void,
    file: *mut bindings::file,
    wait: *mut bindings::poll_table_struct,
) -> bindings::__poll_t {
    // SAFETY: the caller's contract.
    let l = unsafe { &*(data as *const LemLine) };
    // SAFETY: `file` is the claim's live file, and `wait` the table poll gave us.
    let file = unsafe { File::from_raw_file(file) };
    unsafe { PollTable::from_raw(wait) }.register_wait(file, &l.wait);
    if events_ready(l) {
        bindings::POLLIN | bindings::POLLRDNORM
    } else {
        0
    }
}

static LEM_LINE_OPS: LemClaimOps = LemClaimOps {
    release: Some(lem_line_release),
    read: Some(lem_line_read),
    poll: Some(lem_line_poll),
};

/// The chip whose device has this name: dev_name() of linux/device.h, an inline. It uses the init
/// name while there is one, and the kobject's name after.
///
/// # Safety
/// `gc` is a registered chip, and `data` is a NUL-terminated name.
unsafe extern "C" fn lem_chip_match(gc: *mut bindings::gpio_chip, data: *const c_void) -> c_int {
    // SAFETY: gpiolib passes a registered chip, whose device is live.
    let dev = unsafe { bindings::gpio_device_to_device((*gc).gpiodev) };
    // SAFETY: the device is live while the chip is registered.
    let name = unsafe {
        if (*dev).init_name.is_null() {
            (*dev).kobj.name
        } else {
            (*dev).init_name
        }
    };
    // SAFETY: both are NUL-terminated.
    let same = unsafe { CStr::from_char_ptr(name) == CStr::from_char_ptr(data.cast()) };
    same as c_int
}

/// Maps ENOTSUPP (the kernel's own) to EOPNOTSUPP (user space's), as the C did.
fn user_errno(ret: c_int) -> c_long {
    if ret == ENOTSUPP.to_errno() {
        EOPNOTSUPP.to_errno() as c_long
    } else {
        ret as c_long
    }
}

/// gpiod_set_config() of one pin parameter. pinconf_to_config_packed() of pinctrl/pinconf-generic.h,
/// an inline, is PIN_CONF_PACKED(p, a) = (a << 8) | (p & 0xff), with the shift on the u32 argument:
/// computed here as the header has it. The harness's bias checks read the result back.
fn set_one(desc: *mut bindings::gpio_desc, param: u32, arg: u32) -> c_long {
    let packed = ((arg << 8) as c_ulong) | ((param as c_ulong) & 0xff);
    // SAFETY: the line's descriptor is held for the line's life.
    user_errno(unsafe { gpiod_set_config((desc).cast(), packed) })
}

fn set_bias(l: &LemLine, flags: u32) -> c_long {
    if flags & PULL_UP != 0 {
        return set_one(l.desc, bindings::pin_config_param_PIN_CONFIG_BIAS_PULL_UP as u32, 1);
    }
    if flags & PULL_DOWN != 0 {
        return set_one(l.desc, bindings::pin_config_param_PIN_CONFIG_BIAS_PULL_DOWN as u32, 1);
    }
    if flags & BIAS_OFF != 0 {
        return set_one(l.desc, bindings::pin_config_param_PIN_CONFIG_BIAS_DISABLE as u32, 0);
    }
    0
}

fn set_drive(l: &LemLine, flags: u32) -> c_long {
    if flags & OPEN_DRAIN != 0 {
        return set_one(l.desc, bindings::pin_config_param_PIN_CONFIG_DRIVE_OPEN_DRAIN as u32, 0);
    }
    if flags & OPEN_SOURCE != 0 {
        return set_one(l.desc, bindings::pin_config_param_PIN_CONFIG_DRIVE_OPEN_SOURCE as u32, 0);
    }
    0
}

/// Requests the line's edge interrupt for the edges asked for. Called with the configuration held.
fn start_edges(l: &LemLine, cfg: &mut LineConfig, flags: u32) -> c_long {
    let active_low = l.active_low.load(Ordering::Relaxed);
    let mut trigger = bindings::IRQF_ONESHOT as c_ulong;
    if flags & RISING != 0 {
        let t = if active_low { bindings::IRQF_TRIGGER_FALLING } else { bindings::IRQF_TRIGGER_RISING };
        trigger |= t as c_ulong;
    }
    if flags & FALLING != 0 {
        let t = if active_low { bindings::IRQF_TRIGGER_RISING } else { bindings::IRQF_TRIGGER_FALLING };
        trigger |= t as c_ulong;
    }
    // SAFETY: the line's descriptor is held for the line's life.
    let irq = unsafe { gpiod_to_irq((l.desc).cast()) };
    if irq < 0 {
        return irq as c_long;
    }
    // SAFETY: the handlers are this file's; `l` is the dev_id, and stop_edges frees the IRQ before
    // the line is freed.
    let ret = unsafe {
        bindings::request_threaded_irq(
            irq as c_uint,
            Some(lem_line_edge),
            Some(lem_line_thread),
            trigger,
            l.label.as_ptr().cast(),
            (l as *const LemLine).cast_mut().cast(),
        )
    };
    if ret != 0 {
        return ret as c_long;
    }
    cfg.irq = irq;
    0
}

fn lem_line_configure(l: &LemLine, cfg: &mut LineConfig, c: &LemnosGpioConfig) -> c_long {
    let output = c.flags & OUTPUT != 0;
    stop_edges(l, cfg);
    let active_low = c.flags & ACTIVE_LOW != 0;
    l.active_low.store(active_low, Ordering::Relaxed);
    let ret = set_bias(l, c.flags);
    if ret != 0 {
        return ret;
    }
    // SAFETY: the line's descriptor is held for the line's life.
    let ret = unsafe {
        if output {
            gpiod_direction_output_raw((l.desc).cast(), ((c.value != 0) as c_int) ^ active_low as c_int)
        } else {
            gpiod_direction_input((l.desc).cast())
        }
    };
    if ret != 0 {
        return ret as c_long;
    }
    let ret = set_drive(l, c.flags);
    if ret != 0 {
        return ret;
    }
    if c.debounce_us != 0 {
        // SAFETY: the line's descriptor is held for the line's life.
        let ret = unsafe { gpiod_set_debounce((l.desc).cast(), c.debounce_us) };
        if ret != 0 {
            return user_errno(ret);
        }
    }
    if c.flags & LEM_GPIO_EDGES != 0 {
        start_edges(l, cfg, c.flags)
    } else {
        0
    }
}

/// The line attached to the claim `fd`, or NULL. The file must stay held while the line is used.
fn attached(file: &FileRef) -> *const LemLine {
    // SAFETY: the file is the claim's, or NULL, which the callee refuses.
    unsafe { lem_claim_attached(file.0.cast(), &raw const LEM_LINE_OPS) as *const LemLine }
}

/// LEMNOS_IOC_GPIO_REQUEST: takes the line named "gpio-<chip>:<offset>" under `r->claim`.
///
/// # Safety
/// `r` is the caller's own copy of the ioctl argument.
#[no_mangle]
pub unsafe extern "C" fn lem_gpio_request(r: *const LemnosGpioRequest) -> c_long {
    // SAFETY: the caller's contract.
    let r = unsafe { &*r };
    if r.reserved != 0 {
        return EINVAL.to_errno() as c_long;
    }
    let resource = format_c::<LEM_GPIO_RESOURCE_MAX>(format_args!("gpio-{}:{}", r.chip, r.offset));
    // SAFETY: `resource` is NUL-terminated and lives for the call.
    if !unsafe { lem_claim_held(r.claim, resource.as_ptr()) } {
        return EPERM.to_errno() as c_long;
    }
    let chip = format_c::<LEM_GPIO_LABEL_MAX>(format_args!("gpiochip{}", r.chip));
    // SAFETY: `chip` is NUL-terminated and lives for the call; the match is lem_chip_match.
    let gdev = unsafe { bindings::gpio_device_find(chip.as_ptr().cast(), Some(lem_chip_match)) };
    if gdev.is_null() {
        return ENODEV.to_errno() as c_long;
    }
    // SAFETY: `gdev` is held until the put below.
    let desc = unsafe { bindings::gpio_device_get_desc(gdev, r.offset) };
    if let Err(e) = from_err_ptr(desc) {
        // SAFETY: drops the reference gpio_device_find took.
        unsafe { bindings::gpio_device_put(gdev) };
        return e.to_errno() as c_long;
    }
    // SAFETY: `gdev` is held, and the base is read before its put.
    let base = unsafe { bindings::gpio_device_get_base(gdev) };
    // SAFETY: drops the reference gpio_device_find took.
    unsafe { bindings::gpio_device_put(gdev) };
    let gpio = base.wrapping_add(r.offset as c_int);
    let line = match KBox::pin_init(new_line(desc, gpio), GFP_KERNEL) {
        Ok(line) => line,
        Err(_) => return ENOMEM.to_errno() as c_long,
    };
    // SAFETY: the number is the line's; gpiolib refuses it when someone holds it.
    let ret = unsafe { gpio_request(gpio as c_uint, line.label.as_ptr().cast()) };
    if ret != 0 {
        return ret as c_long;
    }
    // SAFETY: the line is not moved after this: it lives at the box until lem_line_release frees it.
    let raw = KBox::into_raw(unsafe { Pin::into_inner_unchecked(line) });
    // SAFETY: `r->claim` is on `resource` (checked above); the ops are this file's static table.
    let ret = unsafe {
        lem_claim_attach(r.claim, resource.as_ptr(), &raw const LEM_LINE_OPS, raw.cast())
    };
    if ret != 0 {
        // SAFETY: nothing else holds the line, since the attach failed.
        unsafe { lem_line_release(raw.cast()) };
    }
    ret as c_long
}

/// LEMNOS_IOC_GPIO_CONFIG: direction, bias, drive, edges and debounce of the line on the claim.
///
/// # Safety
/// `c` is the caller's own copy of the ioctl argument.
#[no_mangle]
pub unsafe extern "C" fn lem_gpio_config(c: *const LemnosGpioConfig) -> c_long {
    // SAFETY: the caller's contract.
    let c = unsafe { &*c };
    let file = FileRef::get(c.claim);
    let l = attached(&file);
    if l.is_null() {
        return EPERM.to_errno() as c_long;
    }
    // SAFETY: the line is attached to this claim, and lives while the claim is open.
    let line = unsafe { &*l };
    if (c.flags & !LEM_GPIO_FLAGS) != 0
        || (c.flags & LEM_GPIO_BIAS).count_ones() > 1
        || (c.flags & LEM_GPIO_DRIVE).count_ones() > 1
        || c.value > 1
        || ((c.flags & OUTPUT) != 0 && (c.flags & LEM_GPIO_EDGES) != 0)
    {
        return EINVAL.to_errno() as c_long;
    }
    let mut cfg = line.config.lock();
    let ret = lem_line_configure(line, &mut cfg, c);
    if ret == 0 {
        line.flags.store(c.flags, Ordering::Relaxed);
    }
    ret
}

/// LEMNOS_IOC_GPIO_GET: the line's level, after the active-low mapping.
///
/// # Safety
/// `v` is the caller's own copy of the ioctl argument.
#[no_mangle]
pub unsafe extern "C" fn lem_gpio_get(v: *mut LemnosGpioValue) -> c_long {
    // SAFETY: the caller's contract.
    let v = unsafe { &mut *v };
    let file = FileRef::get(v.claim);
    let l = attached(&file);
    if l.is_null() {
        return EPERM.to_errno() as c_long;
    }
    // SAFETY: the line is attached to this claim, and lives while the claim is open.
    let line = unsafe { &*l };
    // SAFETY: the line's descriptor is held for the line's life.
    let level = unsafe { gpiod_get_raw_value_cansleep((line.desc).cast()) };
    if level < 0 {
        return level as c_long;
    }
    let active_low = line.active_low.load(Ordering::Relaxed) as c_int;
    v.value = ((level ^ active_low) != 0) as u32;
    0
}

/// LEMNOS_IOC_GPIO_SET: drives an output line, after the active-low mapping.
///
/// # Safety
/// `v` is the caller's own copy of the ioctl argument.
#[no_mangle]
pub unsafe extern "C" fn lem_gpio_set(v: *const LemnosGpioValue) -> c_long {
    // SAFETY: the caller's contract.
    let v = unsafe { &*v };
    let file = FileRef::get(v.claim);
    let l = attached(&file);
    if l.is_null() {
        return EPERM.to_errno() as c_long;
    }
    if v.value > 1 {
        return EINVAL.to_errno() as c_long;
    }
    // SAFETY: the line is attached to this claim, and lives while the claim is open.
    let line = unsafe { &*l };
    if line.flags.load(Ordering::Relaxed) & OUTPUT == 0 {
        return EPERM.to_errno() as c_long;
    }
    let active_low = line.active_low.load(Ordering::Relaxed) as c_int;
    // SAFETY: the line's descriptor is held for the line's life.
    unsafe { gpiod_set_raw_value_cansleep((line.desc).cast(), ((v.value != 0) as c_int) ^ active_low) as c_long }
}
