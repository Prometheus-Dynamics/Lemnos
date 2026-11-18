// SPDX-License-Identifier: GPL-2.0
//! /dev/lemnos and the blocks behind it: the core of the one Lemnos kernel module. It claims platform
//! blocks through driver_override and gives one opener of /dev/lemnos what a driver in user space needs
//! from them (lemnos.h): register regions, DMA buffers as dma-bufs, dma-bufs mapped to device
//! addresses, and interrupts as reads.
//!
//! Only the blocks listed in owned= are claimed (rs/lists.rs). The driver has no match table, so it binds
//! only where driver_override names it, after the stock driver is unbound; it never replaces or patches
//! one.
//!
//! Locking: BLOCKS_SEM is held for reading across each request and for writing while a block goes away
//! or the file closes, so a request never sees half a block. User memory is only touched outside it. The
//! interrupt state is rs/irq.rs's (IRQ_LOCK), the only lock taken in the handler.
//!
//! A block's record is device-managed memory (devm), as its interrupts are: the device core frees the
//! interrupts first and the record after, once the unbind is over.

use crate::{
    bindings as lb,
    dmabuf::{self, StaticWq},
    irq::{self, LemIrq, LemIrqEvent},
    lists::{self, List},
};
use core::{
    cell::UnsafeCell,
    ffi::c_void,
    mem::{size_of, MaybeUninit},
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering},
};
use kernel::{
    bindings,
    device::Device,
    error::from_err_ptr,
    ffi::{c_char, c_int, c_long, c_uint, c_ulong},
    ioctl::{_IOC_DIR, _IOC_SIZE, _IOC_TYPE, _IOW, _IOWR},
    prelude::*,
    types::Opaque,
    uaccess::{UserPtr, UserSlice},
};

const MAX_REGIONS: u32 = lb::LEMNOS_MAX_REGIONS;
const MAX_IRQS: usize = lb::LEMNOS_MAX_IRQS as usize;
/// The events one read returns at most.
const READ_BATCH: usize = 8;
/// mmap offset, in pages: the block number above these bits, the region below.
const REGION_BITS: u32 = 8;
const PAGE_SHIFT: u32 = kernel::page::PAGE_SHIFT as u32;
const PAGE_SIZE: u64 = kernel::page::PAGE_SIZE as u64;
/// The largest block number: one whose mmap offset still fits.
const MAX_BLOCK: u32 = {
    let most = (1u64 << (usize::BITS - PAGE_SHIFT - REGION_BITS)) - 1;
    if most > u32::MAX as u64 {
        u32::MAX
    } else {
        most as u32
    }
};

const L: u32 = b'L' as u32;
const IOC_CLAIM: u32 = _IOWR::<lb::lemnos_claim>(L, 0xce);
const IOC_NEXT: u32 = _IOWR::<lb::lemnos_next>(L, 0xcf);
const IOC_INFO: u32 = _IOWR::<lb::lemnos_info>(L, 0xd0);
const IOC_REGION: u32 = _IOWR::<lb::lemnos_region>(L, 0xd1);
const IOC_ALLOC: u32 = _IOWR::<lb::lemnos_alloc>(L, 0xd2);
const IOC_IMPORT: u32 = _IOWR::<lb::lemnos_import>(L, 0xd3);
const IOC_RELEASE: u32 = _IOW::<lb::lemnos_release>(L, 0xd4);
const IOC_IRQ_ENABLE: u32 = _IOW::<lb::lemnos_irq>(L, 0xd5);
const IOC_POWER: u32 = _IOWR::<lb::lemnos_power>(L, 0xd6);
const IOC_TEST_IRQ: u32 = _IOW::<lb::lemnos_irq>(L, 0xdf);
const IOC_STREAM_CREATE: u32 = _IOWR::<lb::lemnos_stream_create>(L, 0x80);
const IOC_PIO_OPEN: u32 = _IOW::<lb::lemnos_pio_open>(L, 0x90);
const IOC_PIO_CALL: u32 = _IOWR::<lb::lemnos_pio_call>(L, 0x91);
const IOC_PIO_CONFIG_XFER: u32 = _IOW::<lb::lemnos_pio_config_xfer>(L, 0x92);
const IOC_PIO_XFER: u32 = _IOW::<lb::lemnos_pio_xfer>(L, 0x93);
const IOC_GPIO_REQUEST: u32 = _IOW::<lb::lemnos_gpio_request>(L, 0x98);
const IOC_GPIO_CONFIG: u32 = _IOW::<lb::lemnos_gpio_config>(L, 0x99);
const IOC_GPIO_GET: u32 = _IOWR::<lb::lemnos_gpio_value>(L, 0x9a);
const IOC_GPIO_SET: u32 = _IOW::<lb::lemnos_gpio_value>(L, 0x9b);
const IOC_THERMAL_READ: u32 = _IOWR::<lb::lemnos_thermal>(L, 0xa0);
const IOC_THERMAL_WATCH: u32 = _IOWR::<lb::lemnos_thermal_watch>(L, 0xa1);
const IOC_I2C_TRANSFER: u32 = _IOW::<lb::lemnos_i2c_transfer>(L, 0xa8);
const IOC_I2C_SMBUS: u32 = _IOWR::<lb::lemnos_i2c_smbus>(L, 0xa9);
const IOC_SPI_TRANSFER: u32 = _IOW::<lb::lemnos_spi_transfer>(L, 0xaa);
const IOC_SPI_INFO: u32 = _IOWR::<lb::lemnos_spi_info>(L, 0xab);
const IOC_PISP_BE_CREATE: u32 = _IOWR::<lb::lemnos_pisp_be_create>(L, 0xb0);
const IOC_PISP_BE_VERSION: u32 = _IOWR::<lb::lemnos_pisp_be_version>(L, 0xb2);
const IOC_PWM_REQUEST: u32 = _IOW::<lb::lemnos_pwm_request>(L, 0xb8);
const IOC_PWM_APPLY: u32 = _IOW::<lb::lemnos_pwm_state>(L, 0xb9);
const IOC_PWM_GET: u32 = _IOWR::<lb::lemnos_pwm_state>(L, 0xba);
const IOC_VIP_CREATE: u32 = _IOWR::<lb::lemnos_vip_create>(L, 0xe0);
const IOC_GSDMA_CREATE: u32 = _IOWR::<lb::lemnos_gsdma_create>(L, 0xe8);
const IOC_REGJOB_OPEN: u32 = _IOWR::<lb::lemnos_regjob_open>(L, 0xf0);

/// What a request carries, as large as the largest. Each request for a block starts with its number.
#[repr(C)]
union Args {
    block: u32,
    next: lb::lemnos_next,
    info: lb::lemnos_info,
    region: lb::lemnos_region,
    alloc: lb::lemnos_alloc,
    import: lb::lemnos_import,
    release: lb::lemnos_release,
    irq: lb::lemnos_irq,
    claim: lb::lemnos_claim,
    stream: lb::lemnos_stream_create,
    pisp_be: lb::lemnos_pisp_be_create,
    pisp_be_version: lb::lemnos_pisp_be_version,
    gsdma: lb::lemnos_gsdma_create,
    regjob: lb::lemnos_regjob_open,
    vip: lb::lemnos_vip_create,
    i2c: lb::lemnos_i2c_transfer,
    smbus: lb::lemnos_i2c_smbus,
    spi: lb::lemnos_spi_transfer,
    spi_info: lb::lemnos_spi_info,
    gpio_request: lb::lemnos_gpio_request,
    gpio_config: lb::lemnos_gpio_config,
    gpio_value: lb::lemnos_gpio_value,
    pwm_request: lb::lemnos_pwm_request,
    pwm_state: lb::lemnos_pwm_state,
    power: lb::lemnos_power,
    thermal: lb::lemnos_thermal,
    thermal_watch: lb::lemnos_thermal_watch,
    pio_open: lb::lemnos_pio_open,
    pio_call: lb::lemnos_pio_call,
    pio_config: lb::lemnos_pio_config_xfer,
    pio_xfer: lb::lemnos_pio_xfer,
}

// The parts of the module the requests go to (rs/). They take the request where the core copied it.
use crate::bus::lem_i2c_smbus;
use crate::bus::lem_i2c_transfer;
use crate::bus::lem_spi_info;
use crate::bus::lem_spi_transfer;
use crate::claims::lem_claim;
use crate::events::lem_event_pending;
use crate::events::lem_event_post;
use crate::events::lem_event_take;
use crate::events::lem_events_reset;
use crate::events::lem_thermal_read;
use crate::events::lem_thermal_watch;
use crate::gpio::lem_gpio_config;
use crate::gpio::lem_gpio_get;
use crate::gpio::lem_gpio_request;
use crate::gpio::lem_gpio_set;
use crate::gsdma::lem_gd_create;
use crate::gsdma::lem_gd_probe;
use crate::gsdma::lem_gd_remove;
use crate::pio::lem_pio_call;
use crate::pio::lem_pio_config_xfer;
use crate::pio::lem_pio_open;
use crate::pio::lem_pio_xfer;
use crate::pisp_be::lem_pb_create;
use crate::pisp_be::lem_pb_probe;
use crate::pisp_be::lem_pb_remove;
use crate::pisp_be::lem_pb_version;
use crate::power::lem_power_create;
use crate::power::lem_power_destroy;
use crate::power::lem_power_request;
use crate::power::lem_power_set_block;
use crate::pwm::lem_pwm_apply;
use crate::pwm::lem_pwm_get;
use crate::pwm::lem_pwm_request;
use crate::regjob::lem_rj_open;
use crate::regjob::lem_rj_probe;
use crate::regjob::lem_rj_remove;
use crate::stream_file::lem_stream_create;
use crate::stream_file::lem_streams_block_gone;
use crate::vip::lem_vip_create;
use crate::vip::lem_vip_probe;
use crate::vip::lem_vip_remove;
// improper_ctypes: a VMA holds members that are empty in this configuration; it is only passed by address.
#[allow(improper_ctypes)]
unsafe extern "C" {
    /// lemnos_helpers.c: the three static inlines and macros of a register mapping.
    fn lemnos_vm_flags_set(vma: *mut bindings::vm_area_struct, flags: bindings::vm_flags_t);
    fn lemnos_vma_noncached(vma: *mut bindings::vm_area_struct);
    fn lemnos_io_remap_pfn_range(
        vma: *mut bindings::vm_area_struct,
        pfn: c_ulong,
        size: c_ulong,
    ) -> c_int;
}

/// One bound block. Zeroed memory is a valid, empty one.
#[repr(C)]
struct Block {
    dev: *mut bindings::device,
    pdev: *mut bindings::platform_device,
    id: u32,
    mappable: bool,
    /// Mappable through unconfined= alone.
    unconfined: bool,
    regions: u32,
    nirqs: u32,
    /// The PiSP back end, or NULL.
    pb: *mut c_void,
    /// The K230 graphics DMA engine, or NULL.
    gd: *mut c_void,
    /// Run through register jobs, or NULL.
    rj: *mut c_void,
    /// The VIP9000, or NULL.
    vip: *mut c_void,
    /// handle -> an import (rs/dmabuf.rs).
    imports: Opaque<bindings::xarray>,
    /// DMA buffers not yet freed.
    bufs: AtomicI32,
    power: *mut c_void,
    /// Each interrupt's Linux number, set at probe.
    line: [c_uint; MAX_IRQS],
    /// The interrupts' records: rs/irq.rs's, read and written under its lock.
    irq: UnsafeCell<[LemIrq; MAX_IRQS]>,
}

impl Block {
    /// The record of interrupt `index`.
    fn irq(&self, index: u32) -> *mut LemIrq {
        // The array's first record, then `index` more; callers keep `index` below MAX_IRQS.
        self.irq.get().cast::<LemIrq>().wrapping_add(index as usize)
    }
}

/// A static the kernel's own locking guards.
#[repr(transparent)]
struct Shared<T>(Opaque<T>);

// SAFETY: each use goes through the kernel object's own lock (a rw_semaphore, an xarray).
unsafe impl<T> Sync for Shared<T> {}

/// Held for reading across each request, for writing while a block goes away or the file closes.
static BLOCKS_SEM: Shared<bindings::rw_semaphore> = Shared(Opaque::uninit());
/// number -> the block. Numbers start at 1 and go round.
static BLOCKS: Shared<bindings::xarray> = Shared(Opaque::uninit());
/// Where the next block's number is looked for (xa_alloc_cyclic), under the table's lock.
static NEXT_ID: AtomicU32 = AtomicU32::new(0);
/// The open file's mapping; NULL when closed. Written under BLOCKS_SEM.
static MAPPING: AtomicPtr<bindings::address_space> = AtomicPtr::new(ptr::null_mut());
static OPENED: AtomicBool = AtomicBool::new(false);
/// What a reader of /dev/lemnos sleeps on: an interrupt or an event (rs/events.rs) wakes it.
pub(crate) static WAIT: StaticWq = StaticWq::new();

/// Makes the core's lock, table and wait queues.
///
/// # Safety
/// Called once, first thing at module load.
pub(crate) unsafe fn init() {
    // SAFETY: once, before any use; the name and the class are static (DECLARE_RWSEM).
    unsafe {
        bindings::__init_rwsem(
            BLOCKS_SEM.0.get(),
            c"lem_blocks_sem".as_ptr().cast(),
            kernel::static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: once, before any use (DEFINE_XARRAY_ALLOC1).
    unsafe { bindings::xa_init_flags(BLOCKS.0.get(), bindings::XA_FLAGS_ALLOC1) };
    // SAFETY: once, before any use.
    unsafe { WAIT.init(c"lem_wait", kernel::static_lock_class!()) };
    // SAFETY: once, before any block can be bound.
    unsafe { dmabuf::init() };
}

/// BLOCKS_SEM held for reading until dropped.
struct Reading;

impl Reading {
    fn new() -> Self {
        // SAFETY: the semaphore is initialised (init); process context.
        unsafe { bindings::down_read(BLOCKS_SEM.0.get()) };
        Reading
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        // SAFETY: held for reading since `new`.
        unsafe { bindings::up_read(BLOCKS_SEM.0.get()) };
    }
}

/// BLOCKS_SEM held for writing until dropped.
struct Writing;

impl Writing {
    fn new() -> Self {
        // SAFETY: the semaphore is initialised (init); process context.
        unsafe { bindings::down_write(BLOCKS_SEM.0.get()) };
        Writing
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        // SAFETY: held for writing since `new`.
        unsafe { bindings::up_write(BLOCKS_SEM.0.get()) };
    }
}

/// The block numbered `id`, while BLOCKS_SEM is held.
fn block<'a>(_held: &'a Reading, id: c_ulong) -> Option<&'a Block> {
    // SAFETY: the table is initialised; an entry is a bound block, which stays until its removal takes
    // the semaphore for writing.
    unsafe {
        bindings::xa_load(BLOCKS.0.get(), id)
            .cast::<Block>()
            .as_ref()
    }
}

fn errno(e: Error) -> c_long {
    e.to_errno() as c_long
}

// --- interrupts ---

unsafe extern "C" fn isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // SAFETY: `data` is the record this line was requested with, live until the line is freed; the clock
    // may be read in any context (ktime_get_ns).
    unsafe { irq::lem_irq_fired(data.cast(), bindings::ktime_get() as u64) };
    // SAFETY: wake_up_interruptible(&lem_wait): the queue is a static, initialised at load.
    unsafe { bindings::__wake_up(WAIT.get(), bindings::TASK_INTERRUPTIBLE, 1, ptr::null_mut()) };
    bindings::irqreturn::IRQ_HANDLED
}

fn any_ready() -> bool {
    // The event queue is initialised at load, before the node exists.
    irq::lem_irq_any() || lem_event_pending()
}

fn irq_enable(l: &Block, index: u32) -> c_long {
    if index >= l.nirqs {
        return errno(EINVAL);
    }
    let q = l.irq(index);
    // SAFETY: a requested record of a bound block.
    if unsafe { irq::lem_irq_unmask(q) } {
        // SAFETY: the line is requested for as long as the block is bound; its number never changes.
        unsafe { bindings::enable_irq(l.line[index as usize]) };
    }
    0
}

/// Masks every interrupt left enabled, as at bind, and forgets what was unread.
fn irq_mask_all(l: &Block) {
    for i in 0..l.nirqs {
        let q = l.irq(i);
        // SAFETY: a requested record of a bound block.
        if unsafe { irq::lem_irq_mask(q) } {
            // SAFETY: the line is requested for as long as the block is bound.
            unsafe { bindings::disable_irq(l.line[i as usize]) };
        }
    }
}

/// wait_event_interruptible(lem_wait, any_ready()): 0 once something is ready, -ERESTARTSYS for a signal.
fn wait_ready() -> c_long {
    if any_ready() {
        return 0;
    }
    let mut entry = MaybeUninit::<bindings::wait_queue_entry>::uninit();
    let entry = entry.as_mut_ptr();
    // SAFETY: initialises the entry, on this stack frame, before any use.
    unsafe { bindings::init_wait_entry(entry, 0) };
    loop {
        // SAFETY: the queue is initialised and the entry is the one made above.
        let intr = unsafe {
            bindings::prepare_to_wait_event(
                WAIT.get(),
                entry,
                bindings::TASK_INTERRUPTIBLE as c_int,
            )
        };
        if any_ready() {
            break;
        }
        if intr != 0 {
            // A signal: prepare_to_wait_event took the entry off the queue itself.
            return intr;
        }
        // SAFETY: sleeps in the state set above, in process context.
        unsafe { bindings::schedule() };
    }
    // SAFETY: the entry prepared above, on this queue.
    unsafe { bindings::finish_wait(WAIT.get(), entry) };
    0
}

unsafe extern "C" fn read(
    file: *mut bindings::file,
    ubuf: *mut c_char,
    len: usize,
    _ppos: *mut bindings::loff_t,
) -> isize {
    const EVENT: usize = size_of::<lb::lemnos_event>();
    // SAFETY: a struct lemnos_event of zeroes is an event.
    let mut ev: [lb::lemnos_event; READ_BATCH] = unsafe { core::mem::zeroed() };
    let want = core::cmp::min(len / EVENT, READ_BATCH);
    let mut n = 0;
    if want == 0 {
        return EINVAL.to_errno() as isize;
    }
    loop {
        let mut e = MaybeUninit::<LemIrqEvent>::uninit();
        // SAFETY: `e` is writable, and written when the pop says so.
        while n < want && unsafe { irq::lem_irq_pop(e.as_mut_ptr()) } {
            // SAFETY: just written by lem_irq_pop.
            let e = unsafe { e.assume_init_read() };
            ev[n] = lb::lemnos_event {
                block: e.block_id,
                index: e.index,
                count: e.count,
                kind: lb::LEMNOS_EVENT_IRQ,
                ns: e.ns,
                value: 0,
            };
            n += 1;
        }
        // SAFETY: `ev[n]` is a writable event.
        while n < want && unsafe { lem_event_take(&raw mut ev[n]) } {
            n += 1;
        }
        if n != 0 {
            break;
        }
        // SAFETY: the VFS's file, live across the call.
        if unsafe { (*file).f_flags } & bindings::O_NONBLOCK != 0 {
            return EAGAIN.to_errno() as isize;
        }
        let ret = wait_ready();
        if ret != 0 {
            return ret as isize;
        }
    }
    // SAFETY: the first `n` events, as bytes: a struct lemnos_event has no padding.
    let bytes = unsafe { core::slice::from_raw_parts(ev.as_ptr().cast::<u8>(), n * EVENT) };
    match UserSlice::new(UserPtr::from_addr(ubuf as usize), bytes.len())
        .writer()
        .write_slice(bytes)
    {
        Ok(()) => bytes.len() as isize,
        Err(_) => EFAULT.to_errno() as isize,
    }
}

unsafe extern "C" fn poll(
    file: *mut bindings::file,
    wait: *mut bindings::poll_table_struct,
) -> bindings::__poll_t {
    // SAFETY: the VFS's file and table; the queue is a static.
    unsafe { bindings::poll_wait(file, WAIT.get(), wait) };
    if any_ready() {
        // EPOLLIN | EPOLLRDNORM
        bindings::POLLIN | bindings::POLLRDNORM
    } else {
        0
    }
}

// --- registers ---

fn region_offset(block: u32, index: u32) -> i64 {
    ((block as i64) << REGION_BITS | index as i64) << PAGE_SHIFT
}

/// The block's memory resource `index`, or NULL.
fn resource(l: &Block, index: u32) -> *mut bindings::resource {
    // SAFETY: the bound block's platform device.
    unsafe { bindings::platform_get_resource(l.pdev, bindings::IORESOURCE_MEM, index) }
}

fn region(l: &Block, r: &mut lb::lemnos_region) -> c_long {
    if r.index >= l.regions {
        return errno(EINVAL);
    }
    let res = resource(l, r.index);
    if res.is_null() {
        return errno(EINVAL);
    }
    // SAFETY: a resource of the bound device, which keeps it.
    r.start = unsafe { (*res).start } as u64;
    // SAFETY: as above.
    r.size = unsafe { bindings::resource_size(res) } as u64;
    r.offset = region_offset(l.id, r.index) as u64;
    0
}

/// Maps region `index` of the block into `vma`.
///
/// # Safety
/// `vma` is the mapping being made, as the VFS gives it to mmap.
unsafe fn map_region(l: &Block, index: u32, vma: *mut bindings::vm_area_struct) -> c_int {
    // SAFETY: the caller's mapping, live for this call.
    let v = unsafe { &*vma };
    // SAFETY: a VMA's range is plain data in the first arm of its union.
    let start = unsafe { v.__bindgen_anon_1.__bindgen_anon_1.vm_start };
    // SAFETY: as start.
    let end = unsafe { v.__bindgen_anon_1.__bindgen_anon_1.vm_end };
    // SAFETY: the flags are plain data, readable through this arm of their union.
    let flags = unsafe { v.__bindgen_anon_2.vm_flags };
    let len = end - start;
    // SAFETY: asks about the calling task.
    if !l.mappable || !unsafe { bindings::capable(bindings::CAP_SYS_RAWIO as c_int) } {
        return EPERM.to_errno();
    }
    // A private mapping would lose the offset the unbind finds it by.
    if flags & bindings::VM_SHARED == 0 || index >= l.regions {
        return EINVAL.to_errno();
    }
    let res = resource(l, index);
    if res.is_null() {
        return EINVAL.to_errno();
    }
    // SAFETY: a resource of the bound device, which keeps it.
    let (at, size) = unsafe { ((*res).start as u64, bindings::resource_size(res) as u64) };
    if at & (PAGE_SIZE - 1) != 0 || len as u64 > (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1) {
        return EINVAL.to_errno();
    }
    // SAFETY: the caller's mapping, not yet in use (vm_flags_set).
    unsafe {
        lemnos_vm_flags_set(
            vma,
            bindings::VM_IO | bindings::VM_DONTEXPAND | bindings::VM_DONTDUMP,
        )
    };
    // SAFETY: as above (vma->vm_page_prot = pgprot_noncached(vma->vm_page_prot)).
    unsafe { lemnos_vma_noncached(vma) };
    // SAFETY: as above; the range is the block's own register region, checked above.
    unsafe { lemnos_io_remap_pfn_range(vma, (at >> PAGE_SHIFT) as c_ulong, len) }
}

unsafe extern "C" fn mmap(_file: *mut bindings::file, vma: *mut bindings::vm_area_struct) -> c_int {
    // SAFETY: the VFS's mapping.
    let pgoff = unsafe { (*vma).vm_pgoff };
    let index = (pgoff & ((1 << REGION_BITS) - 1)) as u32;
    let held = Reading::new();
    match block(&held, pgoff >> REGION_BITS) {
        // SAFETY: the VFS's mapping.
        Some(l) => unsafe { map_region(l, index, vma) },
        None => ENODEV.to_errno(),
    }
}

// --- the file ---

fn next(held: &Reading, n: &mut lb::lemnos_next) -> c_long {
    let mut id = n.after as c_ulong;
    // SAFETY: the table is initialised; BLOCKS_SEM is held, so the entry found stays.
    let l = unsafe {
        bindings::xa_find_after(
            BLOCKS.0.get(),
            &raw mut id,
            c_ulong::MAX,
            bindings::XA_PRESENT,
        )
    };
    let _ = held;
    // SAFETY: an entry is a bound block (see `block`).
    let Some(l) = (unsafe { l.cast::<Block>().as_ref() }) else {
        return errno(ENOENT);
    };
    n.block = id as u32;
    // strscpy(n->name, dev_name(l->dev), sizeof(n->name)): what fits, always terminated.
    // SAFETY: the bound device's name, a NUL-terminated string that lives as long as the device.
    let name = unsafe { core::ffi::CStr::from_ptr(bindings::dev_name(l.dev).cast()) }.to_bytes();
    let len = core::cmp::min(name.len(), n.name.len() - 1);
    for (to, from) in n.name.iter_mut().zip(&name[..len]) {
        *to = *from as c_char;
    }
    n.name[len] = 0;
    0
}

/// fls64(dma_get_mask(dev)): the bits of the device's DMA addresses.
fn dma_bits(dev: *mut bindings::device) -> u32 {
    // SAFETY: the bound device; its mask pointer is NULL or the device's own mask.
    let mask = unsafe { (*dev).dma_mask.as_ref() }
        .copied()
        .filter(|&m| m != 0)
        .unwrap_or(u32::MAX as u64);
    u64::BITS - mask.leading_zeros()
}

fn block_request(l: &Block, cmd: u32, u: &mut Args) -> c_long {
    let args: *mut c_void = ptr::from_mut(u).cast();
    match cmd {
        IOC_INFO => {
            u.info = lb::lemnos_info {
                block: l.id,
                abi: lb::LEMNOS_ABI,
                flags: (if l.mappable {
                    lb::LEMNOS_INFO_MMIO_MAPPABLE
                } else {
                    0
                }) | (if l.unconfined {
                    lb::LEMNOS_INFO_MMIO_UNCONFINED
                } else {
                    0
                }),
                regions: l.regions,
                irqs: l.nirqs,
                dma_bits: dma_bits(l.dev),
                reserved: [0; 2],
            };
            0
        }
        // SAFETY: the request was copied in as this command's struct.
        IOC_REGION => region(l, unsafe { &mut u.region }),
        // SAFETY: the bound block's device and count; the request is this command's struct.
        IOC_ALLOC => unsafe { dmabuf::lem_alloc(l.dev, &raw const l.bufs, &raw mut u.alloc) },
        // SAFETY: the bound block's device and table; the request is this command's struct.
        IOC_IMPORT => unsafe { dmabuf::lem_import(l.dev, l.imports.get(), &raw mut u.import) },
        // SAFETY: the bound block's table; the request is this command's struct.
        IOC_RELEASE => unsafe { dmabuf::lem_release_import(l.imports.get(), u.release.handle) },
        // SAFETY: the request is this command's struct.
        IOC_IRQ_ENABLE => irq_enable(l, unsafe { u.irq.index }),
        // SAFETY: the bound block's device; the request is this command's struct.
        IOC_STREAM_CREATE => unsafe { lem_stream_create(l.dev, args.cast()) as c_long },
        IOC_PISP_BE_CREATE if l.pb.is_null() => errno(ENOTTY),
        // SAFETY: the block's PiSP back end, live while the block is bound.
        IOC_PISP_BE_CREATE => unsafe { lem_pb_create(l.pb.cast(), l.dev, args) as c_long },
        IOC_PISP_BE_VERSION if l.pb.is_null() => errno(ENOTTY),
        // SAFETY: as above.
        IOC_PISP_BE_VERSION => unsafe { lem_pb_version(l.pb.cast(), args) as c_long },
        IOC_GSDMA_CREATE if l.gd.is_null() => errno(ENOTTY),
        // SAFETY: the block's engine, live while the block is bound.
        IOC_GSDMA_CREATE => unsafe { lem_gd_create(l.gd.cast(), l.dev, args) as c_long },
        IOC_REGJOB_OPEN if l.rj.is_null() => errno(ENOTTY),
        // SAFETY: the block's register jobs, live while the block is bound.
        IOC_REGJOB_OPEN => unsafe { lem_rj_open(l.rj.cast(), args.cast()) },
        IOC_VIP_CREATE if l.vip.is_null() => errno(ENOTTY),
        // SAFETY: the block's accelerator, live while the block is bound.
        IOC_VIP_CREATE => unsafe { lem_vip_create(l.vip.cast(), l.dev, args) as c_long },
        // A block the module drives keeps its own clocks.
        IOC_POWER if !(l.pb.is_null() && l.gd.is_null() && l.rj.is_null() && l.vip.is_null()) => {
            errno(ENOTTY)
        }
        // SAFETY: the block's power state, live while the block is bound.
        IOC_POWER => unsafe { lem_power_request(l.power.cast(), args.cast()) },
        IOC_TEST_IRQ => {
            // SAFETY: the request is this command's struct.
            let index = unsafe { u.irq.index };
            if !lists::test_irq_inject() {
                return errno(EPERM);
            }
            if index >= l.nirqs {
                return errno(EINVAL);
            }
            // SAFETY: a requested line of the bound block.
            (unsafe {
                bindings::irq_set_irqchip_state(
                    l.line[index as usize],
                    bindings::irqchip_irq_state_IRQCHIP_STATE_PENDING,
                    true,
                )
            }) as c_long
        }
        _ => errno(ENOTTY),
    }
}

/// Requests that don't name a block; -ENOIOCTLCMD for one that does.
fn unit_request(cmd: u32, u: &mut Args) -> c_long {
    let args: *mut c_void = ptr::from_mut(u).cast();
    // Each callee is given the request as the core copied it in, which is that command's struct.
    match cmd {
        // SAFETY: the request is this command's struct, copied in whole.
        IOC_CLAIM => unsafe { lem_claim(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_I2C_TRANSFER => unsafe { lem_i2c_transfer(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_I2C_SMBUS => unsafe { lem_i2c_smbus(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_SPI_TRANSFER => unsafe { lem_spi_transfer(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_SPI_INFO => unsafe { lem_spi_info(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_GPIO_REQUEST => unsafe { lem_gpio_request(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_GPIO_CONFIG => unsafe { lem_gpio_config(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_GPIO_GET => unsafe { lem_gpio_get(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_GPIO_SET => unsafe { lem_gpio_set(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PWM_REQUEST => unsafe { lem_pwm_request(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PWM_APPLY => unsafe { lem_pwm_apply(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PWM_GET => unsafe { lem_pwm_get(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PIO_OPEN => unsafe { lem_pio_open(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PIO_CALL => unsafe { lem_pio_call(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PIO_CONFIG_XFER => unsafe { lem_pio_config_xfer(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_PIO_XFER => unsafe { lem_pio_xfer(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_THERMAL_READ => unsafe { lem_thermal_read(args.cast()) },
        // SAFETY: as IOC_CLAIM.
        IOC_THERMAL_WATCH => unsafe { lem_thermal_watch(args.cast()) },
        _ => -(bindings::ENOIOCTLCMD as c_long),
    }
}

unsafe extern "C" fn ioctl(_file: *mut bindings::file, cmd: c_uint, arg: c_ulong) -> c_long {
    let size = _IOC_SIZE(cmd);
    let dir = _IOC_DIR(cmd);
    if _IOC_TYPE(cmd) != L || size > size_of::<Args>() || size < size_of::<u32>() {
        return errno(ENOTTY);
    }
    // SAFETY: every request is plain data, and zeroes are valid for each.
    let mut u: Args = unsafe { core::mem::zeroed() };
    // SAFETY: the first `size` bytes of the request, which is at least that large.
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(ptr::from_mut(&mut u).cast::<u8>(), size) };
    let user = UserPtr::from_addr(arg as usize);
    if dir & bindings::_IOC_WRITE != 0
        && UserSlice::new(user, size)
            .reader()
            .read_slice(bytes)
            .is_err()
    {
        return errno(EFAULT);
    }

    let mut ret = unit_request(cmd, &mut u);
    if ret == -(bindings::ENOIOCTLCMD as c_long) {
        let held = Reading::new();
        ret = if cmd == IOC_NEXT {
            // SAFETY: the request was copied in as this command's struct.
            next(&held, unsafe { &mut u.next })
        } else {
            // SAFETY: every request for a block starts with its number.
            match block(&held, unsafe { u.block } as c_ulong) {
                Some(l) => block_request(l, cmd, &mut u),
                None => errno(ENODEV),
            }
        };
    }

    // A failed copy leaves what was made to the close: the caller has lost it.
    // SAFETY: as above.
    let bytes = unsafe { core::slice::from_raw_parts(ptr::from_ref(&u).cast::<u8>(), size) };
    if ret == 0
        && dir & bindings::_IOC_READ != 0
        && UserSlice::new(user, size)
            .writer()
            .write_slice(bytes)
            .is_err()
    {
        return errno(EFAULT);
    }
    ret
}

unsafe extern "C" fn open(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    if OPENED.swap(true, Ordering::AcqRel) {
        return EBUSY.to_errno();
    }
    // The event queue is initialised at load.
    lem_events_reset();
    let _held = Writing::new();
    // SAFETY: the VFS's file, live until its release.
    MAPPING.store(unsafe { (*file).f_mapping }, Ordering::Release);
    0
}

unsafe extern "C" fn release(_inode: *mut bindings::inode, _file: *mut bindings::file) -> c_int {
    {
        let _held = Writing::new();
        MAPPING.store(ptr::null_mut(), Ordering::Release);
        let mut id: c_ulong = 0;
        // SAFETY: the table is initialised (xa_for_each: the first entry).
        let mut l = unsafe {
            bindings::xa_find(
                BLOCKS.0.get(),
                &raw mut id,
                c_ulong::MAX,
                bindings::XA_PRESENT,
            )
        };
        // SAFETY: an entry is a bound block, which stays while the semaphore is held.
        while let Some(b) = unsafe { l.cast::<Block>().as_ref() } {
            // The block may still be running what the driver set up: stop listening first.
            irq_mask_all(b);
            // SAFETY: the block's table; no request runs while the semaphore is held for writing.
            unsafe { dmabuf::lem_imports_free_all(b.imports.get()) };
            // SAFETY: the table is initialised (xa_for_each: the next entry).
            l = unsafe {
                bindings::xa_find_after(
                    BLOCKS.0.get(),
                    &raw mut id,
                    c_ulong::MAX,
                    bindings::XA_PRESENT,
                )
            };
        }
    }
    // The event queue is initialised at load.
    lem_events_reset();
    OPENED.store(false, Ordering::Release);
    0
}

/// `struct file_operations` of /dev/lemnos.
#[repr(transparent)]
struct Fops(bindings::file_operations);

// SAFETY: never written; the kernel only reads it.
unsafe impl Sync for Fops {}

static FOPS: Fops = Fops(bindings::file_operations {
    #[cfg(MODULE)]
    // `__this_module` is made by the kernel at load and lives until the module is unloaded.
    owner: (&raw const bindings::__this_module).cast_mut(),
    #[cfg(not(MODULE))]
    owner: ptr::null_mut(),
    open: Some(open),
    release: Some(release),
    read: Some(read),
    poll: Some(poll),
    mmap: Some(mmap),
    unlocked_ioctl: Some(ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: every other operation absent (null), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
});

/// A descriptor the kernel links into its own lists at registration.
#[repr(transparent)]
struct Registered<T>(UnsafeCell<T>);

// SAFETY: written by the kernel at registration and at unregistration, under its own locks; this
// module only passes its address.
unsafe impl<T> Sync for Registered<T> {}

/// The node: /dev/lemnos, with any free minor.
static MISC: Registered<bindings::miscdevice> = Registered(UnsafeCell::new(bindings::miscdevice {
    minor: bindings::MISC_DYNAMIC_MINOR as c_int,
    name: c"lemnos".as_ptr().cast(),
    fops: &raw const FOPS.0,
    // SAFETY: the rest of a struct miscdevice is pointers, a list head and a mode, zero by default.
    ..unsafe { core::mem::zeroed() }
}));

/// The platform driver. No match table: bound only through driver_override.
static DRIVER: Registered<bindings::platform_driver> =
    Registered(UnsafeCell::new(bindings::platform_driver {
        probe: Some(probe),
        remove: Some(remove),
        driver: bindings::device_driver {
            name: c"lemnos".as_ptr().cast(),
            // SAFETY: the rest of a struct device_driver is pointers, integers and flags, zero by default.
            ..unsafe { core::mem::zeroed() }
        },
        // SAFETY: every other callback absent (null) and no id table.
        ..unsafe { core::mem::zeroed() }
    }));

/// The node's descriptor, for misc_register and misc_deregister (lemnos_entry.rs).
pub(crate) fn misc() -> *mut bindings::miscdevice {
    MISC.0.get()
}

/// The platform driver's descriptor, for its registration (lemnos_entry.rs).
pub(crate) fn driver() -> *mut bindings::platform_driver {
    DRIVER.0.get()
}

// --- binding ---

/// dev_err_probe with a message that has no arguments: `err`, logged unless it is a deferral.
///
/// # Safety
/// `dev` is the device being probed.
unsafe fn probe_err(
    dev: *mut bindings::device,
    err: c_int,
    what: &'static core::ffi::CStr,
) -> c_int {
    // SAFETY: the caller's device; the format is a static string with no conversions.
    unsafe { bindings::dev_err_probe(dev, err, what.as_ptr().cast()) }
}

/// The error of an ERR_PTR, or 0 for any other pointer.
fn ptr_err(p: *mut c_void) -> c_int {
    match from_err_ptr(p) {
        Ok(_) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn probe(pdev: *mut bindings::platform_device) -> c_int {
    // SAFETY: the platform device being probed, live across the call.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: as above.
    if !unsafe { List::Owned.has(dev) } {
        // SAFETY: as above.
        return unsafe {
            probe_err(
                dev,
                ENODEV.to_errno(),
                c"not a block Lemnos owns (see owned=)\n",
            )
        };
    }
    // SAFETY: device-managed memory of the device being probed, zeroed (devm_kzalloc).
    let l = unsafe {
        bindings::devm_kmalloc(
            dev,
            size_of::<Block>(),
            bindings::GFP_KERNEL | bindings::__GFP_ZERO,
        )
    }
    .cast::<Block>();
    if l.is_null() {
        return ENOMEM.to_errno();
    }
    // SAFETY: the record is ours alone until it is published below, and zeroes are a valid Block.
    let b = unsafe { &mut *l };
    b.dev = dev;
    b.pdev = pdev;
    // SAFETY: the record's own table, before any use.
    unsafe { bindings::xa_init_flags(b.imports.get(), bindings::XA_FLAGS_ALLOC1) };
    // SAFETY: the device being probed (platform_set_drvdata).
    unsafe { bindings::dev_set_drvdata(dev, l.cast()) };

    while b.regions < MAX_REGIONS && !resource(b, b.regions).is_null() {
        b.regions += 1;
    }
    // SAFETY: the device being probed (device_iommu_mapped: it has an IOMMU group).
    b.mappable = !unsafe { (*dev).iommu_group }.is_null() && unsafe { List::Confined.has(dev) };
    // SAFETY: the device being probed.
    if !b.mappable && unsafe { List::Unconfined.has(dev) } {
        // SAFETY: the device being probed, live across the call.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(
            d,
            "unconfined: registers mappable though no IOMMU confines this block's DMA\n"
        );
        b.mappable = true;
        b.unconfined = true;
    }

    // SAFETY: the device being probed, and its record.
    let ret = unsafe { probe_kind(pdev, dev, b) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: the device being probed.
    b.power = unsafe { lem_power_create(dev.cast()) };
    let mut ret = ptr_err(b.power);
    if ret == 0 {
        let limit = bindings::xa_limit {
            max: MAX_BLOCK,
            min: 1,
        };
        // The table's lock is held across __xa_alloc_cyclic, which may drop it to allocate
        // (xa_alloc_cyclic). NEXT_ID is only used here, under that lock.
        // SAFETY: the table is initialised.
        unsafe { bindings::xa_lock(BLOCKS.0.get()) };
        // SAFETY: the table's lock is held; the id and the limit are valid for the call.
        ret = unsafe {
            bindings::__xa_alloc_cyclic(
                BLOCKS.0.get(),
                &raw mut b.id,
                l.cast(),
                limit,
                NEXT_ID.as_ptr(),
                bindings::GFP_KERNEL,
            )
        };
        // SAFETY: the lock taken above.
        unsafe { bindings::xa_unlock(BLOCKS.0.get()) };
        if ret < 0 {
            // SAFETY: the power state made above, which nothing else has seen.
            unsafe { lem_power_destroy(b.power) };
        }
    }
    if ret < 0 {
        // No session can exist yet: the block was never published.
        if !b.rj.is_null() {
            // SAFETY: the register jobs probed above.
            unsafe { lem_rj_remove(b.rj.cast()) };
        }
        return ret;
    }
    for i in 0..b.nirqs {
        // SAFETY: the block's own records, their lines still masked.
        unsafe { irq::lem_irq_set_block(b.irq(i), b.id) };
    }
    // SAFETY: the power state made above.
    unsafe { lem_power_set_block(b.power, b.id) };
    // The event queue is initialised at load.
    lem_event_post(lb::LEMNOS_EVENT_BLOCK_BOUND, b.id, 0, 0);
    // SAFETY: the device being probed, live across the call.
    let d: &Device = unsafe { Device::from_raw(dev) };
    dev_info!(
        d,
        "block {}: {} regions, {} interrupts, {}-bit DMA, registers {}mappable{}\n",
        b.id,
        b.regions,
        b.nirqs,
        dma_bits(dev),
        if b.mappable { "" } else { "not " },
        if b.unconfined { " (unconfined)" } else { "" }
    );
    0
}

/// What kind of block this is: one the module drives itself (the PiSP back end, the K230 graphics DMA
/// engine, the VIP9000, register jobs), in that order, or one whose clock and interrupts the core keeps
/// for the driver in user space. 0, or the probe's errno.
///
/// # Safety
/// `pdev` and `dev` are the device being probed and `b` its record, not yet published.
unsafe fn probe_kind(
    pdev: *mut bindings::platform_device,
    dev: *mut bindings::device,
    b: &mut Block,
) -> c_int {
    // The module drives this block itself: nothing of it reaches user space.
    // SAFETY: the device being probed.
    if unsafe { List::PispBe.has(dev) } {
        b.mappable = false;
        b.unconfined = false;
        // SAFETY: the device being probed.
        b.pb = unsafe { lem_pb_probe(pdev) as *mut c_void };
        return match ptr_err(b.pb) {
            0 => 0,
            // SAFETY: the device being probed.
            e => unsafe { probe_err(dev, e, c"PiSP back end\n") },
        };
    }
    // SAFETY: the device being probed.
    if unsafe { List::Gsdma.has(dev) } {
        b.mappable = false;
        b.unconfined = false;
        // SAFETY: the device being probed.
        b.gd = unsafe { lem_gd_probe(pdev) as *mut c_void };
        return match ptr_err(b.gd) {
            0 => 0,
            // SAFETY: the device being probed.
            e => unsafe { probe_err(dev, e, c"K230 graphics DMA engine\n") },
        };
    }
    // SAFETY: the device being probed.
    if unsafe { List::Vip9000.has(dev) } {
        b.mappable = false;
        b.unconfined = false;
        // SAFETY: the device being probed.
        b.vip = unsafe { lem_vip_probe(pdev) as *mut c_void };
        return match ptr_err(b.vip) {
            0 => 0,
            // SAFETY: the device being probed.
            e => unsafe { probe_err(dev, e, c"VIP9000\n") },
        };
    }
    // SAFETY: the device being probed.
    b.rj = unsafe { lem_rj_probe(pdev) as *mut c_void };
    let e = ptr_err(b.rj);
    if e != 0 {
        // SAFETY: the device being probed.
        return unsafe { probe_err(dev, e, c"register jobs\n") };
    }
    if !b.rj.is_null() {
        b.mappable = false;
        return 0;
    }
    if lists::test_irq_inject() {
        // SAFETY: the device being probed, live across the call.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(
            d,
            "test_irq_inject: interrupts can be raised from user space\n"
        );
    }

    // Clocked while bound, as the stock driver keeps it.
    // SAFETY: the device being probed; the clock is device-managed.
    let clk = unsafe { bindings::devm_clk_get_optional_enabled(dev, ptr::null()) };
    let e = ptr_err(clk.cast());
    if e != 0 {
        // SAFETY: the device being probed.
        return unsafe { probe_err(dev, e, c"no clock\n") };
    }

    while (b.nirqs as usize) < MAX_IRQS {
        let q = b.irq(b.nirqs);
        // SAFETY: the device being probed.
        let line = unsafe { bindings::platform_get_irq_optional(pdev, b.nirqs) };
        if line <= 0 {
            break;
        }
        // SAFETY: the block's own record, before its line is requested.
        unsafe { irq::lem_irq_init(q, line, b.nirqs) };
        b.line[b.nirqs as usize] = line as c_uint;
        // Not shared: a forwarded interrupt is masked until user space clears it.
        // SAFETY: the record lives in the device-managed block, which outlives this device-managed
        // request (devm_request_irq); the name is the device's.
        let ret = unsafe {
            bindings::devm_request_threaded_irq(
                dev,
                line as c_uint,
                Some(isr),
                None,
                bindings::IRQF_NO_AUTOEN as c_ulong,
                bindings::dev_name(dev),
                q.cast(),
            )
        };
        if ret != 0 {
            return ret;
        }
        b.nirqs += 1;
    }
    0
}

unsafe extern "C" fn remove(pdev: *mut bindings::platform_device) {
    // SAFETY: the platform device being unbound, live across the call.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: the record the probe set (platform_get_drvdata), device-managed: it outlives this call.
    let l = unsafe { &*bindings::dev_get_drvdata(dev).cast::<Block>() };

    {
        let _held = Writing::new();
        // SAFETY: the table is initialised; the block is at its number.
        unsafe { bindings::xa_erase(BLOCKS.0.get(), l.id as c_ulong) };
        let mapping = MAPPING.load(Ordering::Acquire);
        if !mapping.is_null() {
            // SAFETY: the open file's mapping, which stays while the semaphore is held for writing.
            unsafe {
                bindings::unmap_mapping_range(
                    mapping,
                    region_offset(l.id, 0),
                    (1i64 << REGION_BITS) << PAGE_SHIFT,
                    1,
                )
            };
        }
        for i in 0..l.nirqs {
            let q = l.irq(i);
            // SAFETY: the line the probe requested with this record.
            unsafe { bindings::devm_free_irq(dev, l.line[i as usize], q.cast()) };
            // SAFETY: the record of a block being removed, its line freed.
            unsafe { irq::lem_irq_forget(q) };
        }
        // SAFETY: the block's table; no request runs while the semaphore is held for writing.
        unsafe { dmabuf::lem_imports_free_all(l.imports.get()) };
        // SAFETY: the block's table, now empty.
        unsafe { bindings::xa_destroy(l.imports.get()) };
        // A stream pins its dma-bufs mapped to this device.
        // SAFETY: the device being unbound.
        unsafe { lem_streams_block_gone(dev) };
        // After the streams: every job has stopped or the block is wedged.
        if !l.pb.is_null() {
            // SAFETY: the block's PiSP back end.
            unsafe { lem_pb_remove(l.pb.cast()) };
        }
        if !l.gd.is_null() {
            // SAFETY: the block's engine.
            unsafe { lem_gd_remove(l.gd.cast()) };
        }
        if !l.rj.is_null() {
            // SAFETY: the block's register jobs.
            unsafe { lem_rj_remove(l.rj.cast()) };
        }
        if !l.vip.is_null() {
            // SAFETY: the block's accelerator.
            unsafe { lem_vip_remove(l.vip.cast()) };
        }
        // SAFETY: the block's power state.
        unsafe { lem_power_destroy(l.power) };
    }
    // The event queue is initialised at load.
    lem_event_post(lb::LEMNOS_EVENT_BLOCK_UNBOUND, l.id, 0, 0);

    // A buffer can only be freed while the device still has its DMA setup, and mappings of it may exist
    // in user space, so the unbind waits for them. Killing their holder ends the wait.
    // SAFETY: the device being unbound and its block's count.
    unsafe { dmabuf::lem_bufs_wait(dev, &raw const l.bufs) };
}
