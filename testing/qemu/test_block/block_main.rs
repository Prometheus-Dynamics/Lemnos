// SPDX-License-Identifier: GPL-2.0
//! Two platform devices with nothing behind them, so the module can be tested in a guest with no
//! hardware to claim. One has the compatible the guest's owned= lists, a page of ordinary memory for
//! its register region, two interrupts from a simulated controller and a reset line this module
//! provides. The other has a compatible nothing lists, and must be refused. The fake PiSP back end,
//! the K230 graphics DMA engine (gsdma) and the VIP9000 are devices of their own, each a page of
//! ordinary memory its driver is told is registers. When `csi2_phys` names memory the guest set
//! aside (memmap=), the fake camera receiver (csi2) is a fourth: see block_csi2.rs.
//!
//! The port of lemnos_test_block.c: the same devices, files, ioctls, init order and teardown order.
//! Every module here is one part of the same crate: this file is the root.
//!
//! Copyright (c) 2026 the Lemnos authors.

#[path = "fixture.rs"]
mod fixture;

#[path = "block_csi2.rs"]
mod csi2;

#[path = "block_gsdma.rs"]
mod gsdma;

#[path = "block_vip.rs"]
mod vip;

use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};
use fixture::{
    attr, dev_attr, lb, param_typed, parm_desc, property_end, property_string, property_u32,
    res_irq, res_mem, this_module, Fixed, Shared,
};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr, to_result},
    ffi::{c_char, c_int, c_uint, c_ulong, c_void},
    prelude::*,
};

pub(crate) const PAGE_SIZE: usize = 1 << bindings::PAGE_SHIFT;
pub(crate) const SZ_4K: usize = 4096;
const TB_IRQS: usize = 6;
/// The interrupts the platform devices with registers are given (the rest are mapped only).
const TB_BLOCK_IRQS: usize = 2;
const TB_MAGIC: u32 = 0x4c45_4d4e;
/// The fake PiSP back end: a window of ordinary memory its driver is told is registers.
pub(crate) const TB_PISP_IRQ: usize = 2;
const TB_PISP_ORDER: u32 = 1;
const TB_PISP_BYTES: usize = PAGE_SIZE << TB_PISP_ORDER;
const TB_PISP_VERSION: u32 = 0x0225_2700;
/// The fake K230 graphics DMA engine's interrupt.
pub(crate) const TB_GSDMA_IRQ: usize = 3;
/// The fake receiver's interrupt.
pub(crate) const TB_TCSI_IRQ: usize = 4;
/// The fake VIP9000's interrupt.
pub(crate) const TB_VIP_IRQ: usize = 5;

/// The version register the fake PiSP back end reports. The default is the family the module knows; a
/// foreign value makes the probe refuse the block.
static PISP_VERSION: AtomicU32 = AtomicU32::new(TB_PISP_VERSION);
param_typed!(
    pisp_version,
    0o444,
    param_ops_uint,
    PISP_VERSION.as_ptr().cast(),
    "uint"
);
parm_desc!(
    pisp_version,
    "Test only: the fake PiSP back end's version register"
);

/// Physical address of 16K set aside for the fake receiver's registers; 0: no receiver.
static CSI2_PHYS: AtomicUsize = AtomicUsize::new(0);
param_typed!(
    csi2_phys,
    0o444,
    param_ops_ulong,
    CSI2_PHYS.as_ptr().cast(),
    "ulong"
);
parm_desc!(
    csi2_phys,
    "Physical address of 16K set aside for the fake receiver's registers"
);

/// Each interrupt of the simulated controller, mapped at init; 0: not mapped.
pub(crate) static TB_IRQ: [AtomicU32; TB_IRQS] = [const { AtomicU32::new(0) }; TB_IRQS];
static TB_FWNODE: AtomicPtr<bindings::fwnode_handle> = AtomicPtr::new(ptr::null_mut());
static TB_DOMAIN: AtomicPtr<bindings::irq_domain> = AtomicPtr::new(ptr::null_mut());
/// The page with TB_MAGIC in it, that the owned block's register window is.
static TB_PAGE: AtomicPtr<bindings::page> = AtomicPtr::new(ptr::null_mut());
static TB_PISP_PAGES: AtomicPtr<bindings::page> = AtomicPtr::new(ptr::null_mut());
static TB_OWNED: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TB_OTHER: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TB_PISP: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TB_RST_PDEV: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TB_RST_DRIVER_UP: AtomicBool = AtomicBool::new(false);
/// The pisp's page_refs file: the address the test wrote, as a page number is looked up from it.
static TB_PISP_PAGE_ADDR: AtomicU64 = AtomicU64::new(0);

// --- helpers the block fixtures share ---

/// readl(base + off): a register of a window the module mapped or allocated, `off` inside it.
///
/// # Safety
///
/// `base` is a window of at least `off + 4` bytes, live until the caller's teardown.
pub(crate) unsafe fn rd(base: *mut u8, off: usize) -> u32 {
    // SAFETY: the caller's: `base + off` is a register of a live window.
    unsafe { bindings::readl(base.wrapping_add(off).cast()) }
}

/// writel(v, base + off): as `rd`.
///
/// # Safety
///
/// As `rd`.
pub(crate) unsafe fn wr(base: *mut u8, off: usize, v: u32) {
    // SAFETY: the caller's: `base + off` is a register of a live window.
    unsafe { bindings::writel(v, base.wrapping_add(off).cast()) };
}

/// A little-endian word of ordinary memory at byte `off` (a plain load, as the C reads it).
///
/// # Safety
///
/// `off + 4` bytes from `p` are readable memory.
pub(crate) unsafe fn mem_ld32(p: *const u8, off: usize) -> u32 {
    // SAFETY: the caller's: the four bytes are readable; the load is unaligned-safe.
    unsafe { p.wrapping_add(off).cast::<u32>().read_unaligned() }
}

/// READ_ONCE of a word of ordinary memory at byte `off`: a volatile load.
///
/// # Safety
///
/// `off + 4` bytes from `p` are readable, aligned memory.
pub(crate) unsafe fn mem_ld32_once(p: *const u8, off: usize) -> u32 {
    // SAFETY: the caller's: the word is readable and aligned, and the load is volatile.
    unsafe { p.wrapping_add(off).cast::<u32>().read_volatile() }
}

/// memcpy(dst, src, n) of ordinary memory: `n` bytes readable at `src`, writable at `dst`.
///
/// # Safety
///
/// As above, and the two do not overlap.
pub(crate) unsafe fn mem_copy(src: *const u8, dst: *mut u8, n: usize) {
    // SAFETY: the caller's: `n` bytes at each, and no overlap.
    unsafe { ptr::copy_nonoverlapping(src, dst, n) };
}

/// memset(phys_to_virt(phys), pat, n): fills the kernel's mapping of a physical address.
///
/// # Safety
///
/// `phys` is a page the kernel has, with `n` bytes of it in the page.
pub(crate) unsafe fn fill_phys(phys: u64, pat: u8, n: usize) {
    // SAFETY: the caller's: phys_to_virt of a page the kernel has, with `n` bytes in it.
    let p = unsafe { fixture::lemnos_test_phys_to_virt(phys) }.cast::<u8>();
    // SAFETY: as above: `n` bytes at `p` are the caller's to write.
    unsafe { ptr::write_bytes(p, pat, n) };
}

/// The register window of the fake PiSP back end: its page's address.
///
/// # Safety
///
/// The pages are allocated by init and held until teardown.
unsafe fn pisp_regs() -> *mut u8 {
    // SAFETY: the pages TB_PISP_PAGES holds, allocated by init and freed only in teardown.
    unsafe { fixture::lemnos_test_page_address(TB_PISP_PAGES.load(Ordering::Relaxed)) }.cast()
}

/// INIT_DELAYED_WORK(dw, func), with a class of its own for lockdep, as the kernel's stream module does it.
///
/// # Safety
///
/// `dw` is a static this module owns, initialised once before it is queued.
pub(crate) unsafe fn init_delayed_work(
    dw: *mut bindings::delayed_work,
    func: unsafe extern "C" fn(*mut bindings::work_struct),
    name: &'static core::ffi::CStr,
) {
    // SAFETY: `dw` is the caller's, initialised here and before any queueing; its work is its own.
    unsafe {
        bindings::init_work_with_key(
            &raw mut (*dw).work,
            Some(func),
            false,
            name.as_ptr().cast(),
            kernel::static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: the timer is the work's own, initialised in place and before any queueing.
    unsafe {
        bindings::timer_init_key(
            &raw mut (*dw).timer,
            Some(bindings::delayed_work_timer_fn),
            bindings::TIMER_IRQSAFE,
            name.as_ptr().cast(),
            kernel::static_lock_class!().as_ptr(),
        )
    };
}

/// schedule_delayed_work_on(cpu, dw, delay): on the system workqueue; `cpu` WORK_CPU_UNBOUND is any.
///
/// # Safety
///
/// `dw` is initialised by `init_delayed_work`, and lives until teardown cancels it.
pub(crate) unsafe fn schedule_on(cpu: c_int, dw: *mut bindings::delayed_work, delay: c_ulong) {
    // SAFETY: the kernel's own system workqueue, set before any module runs.
    let wq = unsafe { bindings::system_wq };
    // SAFETY: as the caller's: `dw` is initialised and live.
    unsafe { bindings::queue_delayed_work_on(cpu, wq, dw, delay) };
}

/// The workqueue's "any CPU" for a queue, WORK_CPU_UNBOUND.
pub(crate) const WORK_CPU_UNBOUND: c_int = bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int;

/// msecs_to_jiffies(ms), as the kernel computes it for any value.
pub(crate) fn jiffies_of(ms: u32) -> c_ulong {
    // SAFETY: a pure conversion, with no pointers.
    unsafe { bindings::__msecs_to_jiffies(ms) }
}

/// The mutex this module's own lock: mutex_lock, then released by `unlock`.
///
/// # Safety
///
/// `m` is a mutex initialised by init.
pub(crate) unsafe fn lock(m: *mut bindings::mutex) {
    // SAFETY: the caller's: `m` is an initialised mutex, and this may sleep as the C does.
    unsafe { bindings::mutex_lock(m) };
}

/// mutex_unlock, as `lock`.
///
/// # Safety
///
/// `m` is held by the caller.
pub(crate) unsafe fn unlock(m: *mut bindings::mutex) {
    // SAFETY: the caller's: `m` is held.
    unsafe { bindings::mutex_unlock(m) };
}

/// Copies one `T` from user memory at `arg`: EFAULT where it cannot be copied.
///
/// # Safety
///
/// `arg` is the user address the ioctl was passed.
pub(crate) unsafe fn copy_in<T>(arg: c_ulong) -> kernel::error::Result<T> {
    let mut v = core::mem::MaybeUninit::<T>::uninit();
    // SAFETY: `v` is room for one `T`; `arg` is the caller's user address, checked by the kernel's copy.
    let left = unsafe {
        bindings::copy_from_user(
            v.as_mut_ptr().cast(),
            arg as *const c_void,
            core::mem::size_of::<T>() as c_ulong,
        )
    };
    if left != 0 {
        return Err(EFAULT);
    }
    // SAFETY: the copy wrote every byte of `v`.
    Ok(unsafe { v.assume_init() })
}

/// Copies one `T` to user memory at `arg`: EFAULT where it cannot be copied.
///
/// # Safety
///
/// `arg` is the user address the ioctl was passed.
pub(crate) unsafe fn copy_out<T>(arg: c_ulong, val: &T) -> kernel::error::Result {
    // SAFETY: `val` is one `T` the caller holds; `arg` is the user address, checked by the kernel's copy.
    let left = unsafe {
        bindings::copy_to_user(
            arg as *mut c_void,
            (val as *const T).cast(),
            core::mem::size_of::<T>() as c_ulong,
        )
    };
    if left != 0 {
        return Err(EFAULT);
    }
    Ok(())
}

// --- the reset line ---

static TB_RST_PROPS: Fixed<[bindings::property_entry; 2]> =
    Fixed([property_u32(c"#reset-cells", 1), property_end()]);

static TB_RST_NODE: Fixed<bindings::software_node> = Fixed(bindings::software_node {
    name: c"lemnos-test-reset".as_ptr().cast(),
    parent: ptr::null(),
    properties: TB_RST_PROPS.get().cast(),
});

/// Set by the reset line's assert, cleared by its deassert; the status reads it.
static TB_ASSERTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn tb_rst_assert(_rc: *mut lb::reset_controller_dev, _id: c_ulong) -> c_int {
    TB_ASSERTED.store(true, Ordering::Relaxed);
    0
}

unsafe extern "C" fn tb_rst_deassert(_rc: *mut lb::reset_controller_dev, _id: c_ulong) -> c_int {
    TB_ASSERTED.store(false, Ordering::Relaxed);
    0
}

unsafe extern "C" fn tb_rst_status(_rc: *mut lb::reset_controller_dev, _id: c_ulong) -> c_int {
    TB_ASSERTED.load(Ordering::Relaxed) as c_int
}

unsafe extern "C" fn tb_rst_reset(_rc: *mut lb::reset_controller_dev, _id: c_ulong) -> c_int {
    0
}

static TB_RST_OPS: Fixed<lb::reset_control_ops> = Fixed(lb::reset_control_ops {
    assert: Some(tb_rst_assert),
    deassert: Some(tb_rst_deassert),
    status: Some(tb_rst_status),
    reset: Some(tb_rst_reset),
    // SAFETY: every other field is zero: no such operation.
    ..unsafe { core::mem::zeroed() }
});

/// The reset controller: written by the reset driver's probe, before it is registered.
static TB_RST: Shared<lb::reset_controller_dev> = Shared::new(lb::reset_controller_dev {
    ops: TB_RST_OPS.get(),
    owner: this_module(),
    nr_resets: 1,
    fwnode_reset_n_cells: 1,
    // SAFETY: the rest of a struct reset_controller_dev is the core's, zero in a static.
    ..unsafe { core::mem::zeroed() }
});

/// The reset line's provider is a device with a driver, as a real one is: a consumer that refers to a
/// node nothing has probed is deferred.
unsafe extern "C" fn tb_rst_probe(pdev: *mut bindings::platform_device) -> c_int {
    // SAFETY: the driver core passes the device being bound; the address of its device is taken.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: the controller is not registered yet: nothing else reads it.
    unsafe { (*TB_RST.get()).dev = dev };
    // SAFETY: dev_fwnode(dev), as its macro calls this.
    let fwnode = unsafe { bindings::__dev_fwnode(dev) };
    // SAFETY: as above: not registered yet.
    unsafe { (*TB_RST.get()).fwnode = fwnode };
    // SAFETY: the controller is a static, filled in above, and unregistered with the device.
    unsafe { lb::devm_reset_controller_register(dev, TB_RST.get()) }
}

static TB_RST_DRIVER: Shared<bindings::platform_driver> = Shared::new(bindings::platform_driver {
    probe: Some(tb_rst_probe),
    driver: bindings::device_driver {
        name: c"lemnos-test-reset".as_ptr().cast(),
        // SAFETY: the rest of a struct device_driver is pointers, integers and flags, zero by default.
        ..unsafe { core::mem::zeroed() }
    },
    // SAFETY: every other callback absent (null) and no id table.
    ..unsafe { core::mem::zeroed() }
});

// --- the owned and the other device ---

static TB_OWNED_REF: Fixed<bindings::software_node_ref_args> =
    Fixed(bindings::software_node_ref_args {
        swnode: TB_RST_NODE.get(),
        nargs: 1,
        args: [0; 16],
        // SAFETY: no firmware node: the reference is by software node.
        ..unsafe { core::mem::zeroed() }
    });

static TB_OWNED_PROPS: Fixed<[bindings::property_entry; 4]> = Fixed([
    property_string(c"compatible", c"lemnos,test-block"),
    bindings::property_entry {
        name: c"resets".as_ptr().cast(),
        length: core::mem::size_of::<bindings::software_node_ref_args>(),
        is_inline: false,
        type_: bindings::dev_prop_type_DEV_PROP_REF,
        __bindgen_anon_1: bindings::property_entry__bindgen_ty_1 {
            pointer: TB_OWNED_REF.get().cast(),
        },
    },
    fixture::property_string(c"reset-names", c"rst"),
    property_end(),
]);

static TB_OTHER_PROPS: Fixed<[bindings::property_entry; 2]> = Fixed([
    property_string(c"compatible", c"lemnos,not-owned"),
    property_end(),
]);

static TB_PISP_PROPS: Fixed<[bindings::property_entry; 2]> = Fixed([
    property_string(c"compatible", c"lemnos,test-pisp-be"),
    property_end(),
]);

// --- the fake PiSP back end's sysfs ---

/// "offset value" (hex) as the poke store reads it: a 32-byte buffer, split at its first space.
///
/// # Safety
///
/// `buf` is a NUL-terminated string.
unsafe fn poke_pair(buf: *const c_char) -> Option<(u64, u64)> {
    let mut tmp = [0 as c_char; 32];
    // SAFETY: strscpy(tmp, buf, sizeof(tmp)): `buf` is NUL-terminated (the caller's), `tmp` holds 32 bytes.
    unsafe { bindings::sized_strscpy(tmp.as_mut_ptr(), buf, tmp.len()) };
    // SAFETY: `tmp` is NUL-terminated by the copy above.
    let sep = unsafe { bindings::strchr(tmp.as_ptr(), c_int::from(b' ')) };
    if sep.is_null() {
        return None;
    }
    // SAFETY: `sep` points at the space found in `tmp`: the first half ends there.
    unsafe { sep.write(0) };
    // SAFETY: the byte after the space is in `tmp`, at or before its terminating NUL.
    let second = unsafe { sep.add(1) };
    // SAFETY: the first half, NUL-terminated just above.
    let first = unsafe { bindings::strim(tmp.as_mut_ptr()) };
    // SAFETY: the second half, NUL-terminated by the copy.
    let second = unsafe { bindings::strim(second) };
    let (mut a, mut b) = (0u64, 0u64);
    // SAFETY: `first` is a NUL-terminated string in `tmp`; `a` is written on success.
    if unsafe { bindings::kstrtoull(first, 16, &raw mut a) } != 0 {
        return None;
    }
    // SAFETY: `second` is a NUL-terminated string in `tmp`; `b` is written on success.
    if unsafe { bindings::kstrtoull(second, 16, &raw mut b) } != 0 {
        return None;
    }
    Some((a, b))
}

/// "offset value": sets a register of the fake block, as hardware would (hex).
unsafe extern "C" fn poke_store(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const c_char,
    count: usize,
) -> isize {
    // SAFETY: sysfs passes the store's text, NUL-terminated.
    let Some((offset, value)) = (unsafe { poke_pair(buf) }) else {
        return EINVAL.to_errno() as isize;
    };
    if offset >= TB_PISP_BYTES as u64 || offset % 4 != 0 {
        return EINVAL.to_errno() as isize;
    }
    // SAFETY: the window's pages are allocated by init and live until teardown.
    let regs = unsafe { pisp_regs() };
    // SAFETY: the register at `offset`: inside the window and aligned (checked above).
    unsafe { wr(regs, offset as usize, value as u32) };
    count as isize
}

static DEV_ATTR_POKE: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"poke", 0o200, None, Some(poke_store)));

/// Raises the fake block's interrupt; `ret ?: count`.
unsafe extern "C" fn irq_store(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    _buf: *const c_char,
    count: usize,
) -> isize {
    let irq = TB_IRQ[TB_PISP_IRQ].load(Ordering::Relaxed);
    // SAFETY: the interrupt is one of the simulated domain's, mapped until teardown.
    let ret = unsafe {
        bindings::irq_set_irqchip_state(
            irq,
            bindings::irqchip_irq_state_IRQCHIP_STATE_PENDING,
            true,
        )
    };
    if ret != 0 {
        ret as isize
    } else {
        count as isize
    }
}

static DEV_ATTR_IRQ: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"irq", 0o200, None, Some(irq_store)));

/// Whether a page the driver gave the block is still allocated: write its address, read its count.
unsafe extern "C" fn page_refs_store(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const c_char,
    count: usize,
) -> isize {
    let mut v: u64 = 0;
    // SAFETY: sysfs passes the store's text, NUL-terminated; `v` is written on success.
    if unsafe { bindings::kstrtoull(buf, 16, &raw mut v) } != 0 {
        return EINVAL.to_errno() as isize;
    }
    TB_PISP_PAGE_ADDR.store(v, Ordering::Relaxed);
    count as isize
}

unsafe extern "C" fn page_refs_show(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *mut c_char,
) -> isize {
    let pfn = TB_PISP_PAGE_ADDR.load(Ordering::Relaxed) >> bindings::PAGE_SHIFT;
    // SAFETY: pfn_valid is any number's question.
    let refs = if unsafe { fixture::lemnos_test_pfn_valid(pfn as c_ulong) } != 0 {
        // SAFETY: the page number is valid (checked above): its count is read.
        unsafe { fixture::lemnos_test_page_refcount(pfn as c_ulong) }
    } else {
        -1
    };
    // SAFETY: sysfs passes a page for the text.
    unsafe { bindings::sysfs_emit(buf, c"%d\n".as_ptr().cast(), refs) as isize }
}

static DEV_ATTR_PAGE_REFS: Shared<bindings::device_attribute> = Shared::new(dev_attr(
    c"page_refs",
    0o644,
    Some(page_refs_show),
    Some(page_refs_store),
));

/// The fake block's register window, for the test to compare with what it asked for.
unsafe extern "C" fn regs_read(
    _f: *mut bindings::file,
    _kobj: *mut bindings::kobject,
    _a: *const bindings::bin_attribute,
    buf: *mut c_char,
    off: bindings::loff_t,
    count: usize,
) -> isize {
    if off >= TB_PISP_BYTES as bindings::loff_t {
        return 0;
    }
    let count = count.min((TB_PISP_BYTES as bindings::loff_t - off) as usize);
    // SAFETY: the window's pages are allocated by init and live until teardown.
    let regs = unsafe { pisp_regs() };
    // SAFETY: `count` bytes at `off` are inside the window, and sysfs passes room for `count` bytes.
    unsafe { bindings::memcpy_fromio(buf.cast(), regs.wrapping_add(off as usize).cast(), count) };
    count as isize
}

static BIN_ATTR_REGS: Fixed<bindings::bin_attribute> = Fixed(bindings::bin_attribute {
    attr: attr(c"regs", 0o444),
    size: TB_PISP_BYTES,
    read: Some(regs_read),
    // SAFETY: no private data, no write, no mapping: zero.
    ..unsafe { core::mem::zeroed() }
});

static TB_PISP_ATTRS: Fixed<[*mut bindings::attribute; 4]> = Fixed([
    // The address of each attribute's `attr`, its first field.
    DEV_ATTR_POKE.get().cast(),
    DEV_ATTR_IRQ.get().cast(),
    DEV_ATTR_PAGE_REFS.get().cast(),
    ptr::null_mut(),
]);
static TB_PISP_BIN_ATTRS: Fixed<[*const bindings::bin_attribute; 2]> =
    Fixed([BIN_ATTR_REGS.get(), ptr::null()]);
static TB_PISP_GROUP: Fixed<bindings::attribute_group> = Fixed(bindings::attribute_group {
    __bindgen_anon_2: bindings::attribute_group__bindgen_ty_2 {
        attrs: TB_PISP_ATTRS.get().cast_mut().cast(),
    },
    bin_attrs: TB_PISP_BIN_ATTRS.get().cast(),
    // SAFETY: no name and no visibility callbacks: zero.
    ..unsafe { core::mem::zeroed() }
});

// --- setup and teardown ---

/// A platform device's information with the resources, the properties and DMA_BIT_MASK(64).
pub(crate) fn device_info(
    name: &'static core::ffi::CStr,
    res: *const bindings::resource,
    num_res: usize,
    props: *const bindings::property_entry,
) -> bindings::platform_device_info {
    bindings::platform_device_info {
        name: name.as_ptr().cast(),
        id: bindings::PLATFORM_DEVID_NONE,
        res,
        num_res: num_res as c_uint,
        dma_mask: u64::MAX,
        properties: props,
        // SAFETY: no parent, no firmware node and no data: zero.
        ..unsafe { core::mem::zeroed() }
    }
}

/// A window of whole pages for a fake block's registers, zeroed (alloc_pages, as alloc_page does for order 0).
pub(crate) fn alloc_block_page(order: u32) -> Result<*mut bindings::page> {
    // SAFETY: alloc_pages(GFP_KERNEL | __GFP_ZERO, order), as its inline calls this.
    let page = unsafe { bindings::alloc_pages(bindings::GFP_KERNEL | bindings::__GFP_ZERO, order) };
    if page.is_null() {
        return Err(ENOMEM);
    }
    Ok(page)
}

/// Frees the pages of a window allocated by `alloc_block_page`.
///
/// # Safety
///
/// `page` came from `alloc_block_page(order)`, nothing maps it now, and it is freed once.
pub(crate) unsafe fn free_block_page(page: *mut bindings::page, order: u32) {
    // SAFETY: the caller's: the pages were allocated with this order and are unmapped.
    unsafe { bindings::__free_pages(page, order) };
}

/// Init, in the C's order: the statics that need it, the simulated domain, the reset line, the owned
/// and other devices, the receiver (when named), the PiSP back end, the DMA engine and the VIP.
fn tb_setup() -> Result {
    // The hard timer and the delayed works are the statics of their modules: set up before any device.
    csi2::init_statics();
    gsdma::init_statics();
    vip::init_statics();

    let page = alloc_block_page(0)?;
    TB_PAGE.store(page, Ordering::Relaxed);
    // SAFETY: page_address of the page just allocated.
    let magic = unsafe { fixture::lemnos_test_page_address(page) }.cast::<u32>();
    // SAFETY: the first word of the page, which is zeroed and at least a word long.
    unsafe { magic.write(TB_MAGIC) };

    // SAFETY: irq_domain_alloc_named_fwnode("lemnos-test-block"), as its inline calls this.
    let fwnode = unsafe {
        bindings::__irq_domain_alloc_fwnode(
            bindings::IRQCHIP_FWNODE_NAMED,
            0,
            c"lemnos-test-block".as_ptr().cast(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if fwnode.is_null() {
        return Err(ENOMEM);
    }
    TB_FWNODE.store(fwnode, Ordering::Relaxed);
    // SAFETY: the node was made just above.
    let domain = from_err_ptr(unsafe { lb::irq_domain_create_sim(fwnode, TB_IRQS as c_uint) })?;
    TB_DOMAIN.store(domain, Ordering::Relaxed);

    let mut res: [bindings::resource; 1 + TB_BLOCK_IRQS] =
        // SAFETY: resources of zeros, each filled in below before the device is registered.
        unsafe { core::mem::zeroed() };
    // SAFETY: page_to_phys of the page just allocated.
    res[0] = res_mem(
        unsafe { fixture::lemnos_test_page_to_phys(page) },
        PAGE_SIZE as u64,
    );
    for i in 0..TB_IRQS {
        // SAFETY: irq_create_mapping(domain, i), as its inline calls this; the domain is made above.
        let irq = unsafe {
            bindings::irq_create_mapping_affinity(
                domain,
                i as bindings::irq_hw_number_t,
                ptr::null(),
            )
        };
        if irq == 0 {
            return Err(EINVAL);
        }
        TB_IRQ[i].store(irq, Ordering::Relaxed);
        if i < TB_BLOCK_IRQS {
            res[1 + i] = res_irq(irq);
        }
    }

    // SAFETY: platform_driver_register, as its macro calls this with THIS_MODULE and the module's name;
    // the driver is a static.
    to_result(unsafe {
        bindings::__platform_driver_register(
            TB_RST_DRIVER.get(),
            this_module(),
            c"lemnos_test_block".as_ptr().cast(),
        )
    })?;
    TB_RST_DRIVER_UP.store(true, Ordering::Relaxed);

    let rst_info = bindings::platform_device_info {
        name: c"lemnos-test-reset".as_ptr().cast(),
        id: bindings::PLATFORM_DEVID_NONE,
        swnode: TB_RST_NODE.get(),
        // SAFETY: no parent, no resources and no data: zero.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: the information lives for the call; the node is a static.
    let rst =
        from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const rst_info) })?;
    TB_RST_PDEV.store(rst, Ordering::Relaxed);
    // SAFETY: the device is registered; its driver is read once, as the C does.
    if unsafe { (*rst).dev.driver }.is_null() {
        return Err(ENODEV);
    }

    let owned_info = device_info(
        c"lemnos-test-block",
        res.as_ptr(),
        res.len(),
        TB_OWNED_PROPS.get().cast(),
    );
    // SAFETY: the information, its resources and its properties live for the call, which copies them.
    let owned =
        from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const owned_info) })?;
    TB_OWNED.store(owned, Ordering::Relaxed);

    let other_info = device_info(
        c"lemnos-test-other",
        ptr::null(),
        0,
        TB_OTHER_PROPS.get().cast(),
    );
    // SAFETY: as above.
    let other =
        from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const other_info) })?;
    TB_OTHER.store(other, Ordering::Relaxed);

    let csi2_phys = CSI2_PHYS.load(Ordering::Relaxed);
    if csi2_phys != 0 {
        csi2::setup(
            csi2_phys as u64,
            TB_IRQ[TB_TCSI_IRQ].load(Ordering::Relaxed),
        )?;
    }

    let pages = alloc_block_page(TB_PISP_ORDER)?;
    TB_PISP_PAGES.store(pages, Ordering::Relaxed);
    // SAFETY: the pages just allocated are the window: its register 0 is written.
    let regs = unsafe { pisp_regs() };
    // SAFETY: register 0 of the window, which is a page long.
    unsafe { wr(regs, 0, PISP_VERSION.load(Ordering::Relaxed)) };
    let mut pisp_res: [bindings::resource; 2] =
        // SAFETY: resources of zeros, each filled in below before the device is registered.
        unsafe { core::mem::zeroed() };
    // SAFETY: page_to_phys of the pages just allocated.
    pisp_res[0] = res_mem(
        unsafe { fixture::lemnos_test_page_to_phys(pages) },
        TB_PISP_BYTES as u64,
    );
    pisp_res[1] = res_irq(TB_IRQ[TB_PISP_IRQ].load(Ordering::Relaxed));
    let pisp_info = device_info(
        c"lemnos-test-pisp",
        pisp_res.as_ptr(),
        pisp_res.len(),
        TB_PISP_PROPS.get().cast(),
    );
    // SAFETY: as above.
    let pisp =
        from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const pisp_info) })?;
    TB_PISP.store(pisp, Ordering::Relaxed);
    // SAFETY: the device is registered; the group is a static.
    to_result(unsafe {
        bindings::sysfs_create_group(&raw mut (*pisp).dev.kobj, TB_PISP_GROUP.get())
    })?;

    gsdma::setup()?;
    vip::setup()?;
    Ok(())
}

/// Teardown, in the C's order. Everything it reaches is either set up by `tb_setup` or null.
fn tb_teardown() {
    vip::teardown();
    gsdma::teardown();

    let pisp = TB_PISP.swap(ptr::null_mut(), Ordering::Relaxed);
    if !pisp.is_null() {
        // SAFETY: the device is registered; removing a group that was not made does nothing.
        unsafe { bindings::sysfs_remove_group(&raw mut (*pisp).dev.kobj, TB_PISP_GROUP.get()) };
        // SAFETY: the device was registered by setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(pisp) };
    }
    let pages = TB_PISP_PAGES.swap(ptr::null_mut(), Ordering::Relaxed);
    if !pages.is_null() {
        // SAFETY: the pages were allocated with TB_PISP_ORDER and nothing maps them now.
        unsafe { free_block_page(pages, TB_PISP_ORDER) };
    }

    csi2::teardown();

    let other = TB_OTHER.swap(ptr::null_mut(), Ordering::Relaxed);
    if !other.is_null() {
        // SAFETY: the device was registered by setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(other) };
    }
    let owned = TB_OWNED.swap(ptr::null_mut(), Ordering::Relaxed);
    if !owned.is_null() {
        // SAFETY: as above.
        unsafe { bindings::platform_device_unregister(owned) };
    }
    let rst = TB_RST_PDEV.swap(ptr::null_mut(), Ordering::Relaxed);
    if !rst.is_null() {
        // SAFETY: the reset device was registered by setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(rst) };
    }
    if TB_RST_DRIVER_UP.swap(false, Ordering::Relaxed) {
        // SAFETY: the driver was registered by setup.
        unsafe { bindings::platform_driver_unregister(TB_RST_DRIVER.get()) };
    }
    for irq in &TB_IRQ {
        let irq = irq.swap(0, Ordering::Relaxed);
        if irq != 0 {
            // SAFETY: the mapping was made by setup, and this is its one disposal.
            unsafe { bindings::irq_dispose_mapping(irq) };
        }
    }
    let domain = TB_DOMAIN.swap(ptr::null_mut(), Ordering::Relaxed);
    if !domain.is_null() {
        // SAFETY: the domain was made by setup, and every mapping in it is disposed of above.
        unsafe { lb::irq_domain_remove_sim(domain) };
    }
    let fwnode = TB_FWNODE.swap(ptr::null_mut(), Ordering::Relaxed);
    if !fwnode.is_null() {
        // SAFETY: the node was made by setup, and its domain is gone.
        unsafe { bindings::irq_domain_free_fwnode(fwnode) };
    }
    let page = TB_PAGE.swap(ptr::null_mut(), Ordering::Relaxed);
    if !page.is_null() {
        // SAFETY: the page was allocated by setup with order 0, and nothing maps it now.
        unsafe { free_block_page(page, 0) };
    }
}

struct TestBlock;

impl kernel::Module for TestBlock {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        if let Err(e) = tb_setup() {
            tb_teardown();
            return Err(e);
        }
        Ok(TestBlock)
    }
}

impl Drop for TestBlock {
    fn drop(&mut self) {
        tb_teardown();
    }
}

module! {
    type: TestBlock,
    name: "lemnos_test_block",
    authors: ["Lemnos"],
    description: "Devices with nothing behind them, for testing the Lemnos module",
    license: "GPL",
}
