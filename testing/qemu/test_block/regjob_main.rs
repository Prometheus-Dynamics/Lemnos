// SPDX-License-Identifier: GPL-2.0
//! Fake blocks for lemnos.ko's register jobs (kernel/rs/regjob.rs), so the engine can be tested in
//! a guest with no hardware: a register window of ordinary memory (the module is loaded with
//! test_rj_fake_regs), an interrupt from a simulated controller and a reset line this module
//! provides and counts. The port of lemnos_test_regjob.c: the same devices, files and order.
//!
//! Two blocks: the A733 video engine's (a 4 KiB window, its interrupt first) and the K230 ISP's (a
//! 36 KiB window, three interrupts, jobs completing on the second). Each device has, in sysfs:
//!   poke     "offset value" (hex): sets a register, as the hardware would
//!   irq      raises the block's interrupt
//!   irq_line "n": raises the block's interrupt n (the K230 ISP's core is 0)
//!   regs     the register window, read-only
//!   stamp    "offset byte" (hex): writes the byte at the bus address the register at offset
//!            holds, as the block's DMA would. A bus address is a physical one here: nothing
//!            translates it.
//!   resets   how many times the block's reset line was pulsed
//!
//! Copyright (c) 2026 the Lemnos authors.

#[path = "fixture.rs"]
mod fixture;

use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering},
};
use fixture::{
    attr, dev_attr, get_order, hex_pair, kobj_to_dev, lb, property_end, property_string,
    property_u32, res_irq, res_mem, this_module, Fixed, Shared,
};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr, to_result},
    ffi::{c_char, c_int, c_uint, c_ulong},
    prelude::*,
};

const TR_BLOCKS: usize = 2;
/// Per block; `irq` raises the one jobs complete on.
const TR_IRQS: usize = 3;

struct TrBlock {
    name: &'static core::ffi::CStr,
    compatible: &'static core::ffi::CStr,
    window: u32,
    job_irq: usize,
    page: AtomicPtr<bindings::page>,
    pdev: AtomicPtr<bindings::platform_device>,
    irq: [AtomicU32; TR_IRQS],
    resets: AtomicI32,
}

impl TrBlock {
    const fn new(
        name: &'static core::ffi::CStr,
        compatible: &'static core::ffi::CStr,
        window: u32,
        job_irq: usize,
    ) -> Self {
        Self {
            name,
            compatible,
            window,
            job_irq,
            page: AtomicPtr::new(ptr::null_mut()),
            pdev: AtomicPtr::new(ptr::null_mut()),
            irq: [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
            resets: AtomicI32::new(0),
        }
    }

    /// The register window: the block's pages.
    fn regs(&self) -> *mut u8 {
        // SAFETY: page_address of the pages tr_add allocated, held until teardown.
        unsafe { fixture::lemnos_test_page_address(self.page.load(Ordering::Relaxed)) }.cast()
    }
}

/// Each block's window and interrupts as its profile in lemnos.ko expects them.
static TR_BLOCKS_TABLE: [TrBlock; TR_BLOCKS] = [
    TrBlock::new(c"lemnos-test-ve", c"lemnos,test-ve", 0x1000, 0),
    TrBlock::new(c"lemnos-test-isp", c"lemnos,test-isp", 0x9000, 1),
];

static TR_FWNODE: AtomicPtr<bindings::fwnode_handle> = AtomicPtr::new(ptr::null_mut());
static TR_DOMAIN: AtomicPtr<bindings::irq_domain> = AtomicPtr::new(ptr::null_mut());
static TR_RST_PDEV: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TR_RST_DRIVER_UP: AtomicBool = AtomicBool::new(false);

// --- the reset line ---

static TR_RST_PROPS: Fixed<[bindings::property_entry; 2]> =
    Fixed([property_u32(c"#reset-cells", 1), property_end()]);

static TR_RST_NODE: Fixed<bindings::software_node> = Fixed(bindings::software_node {
    name: c"lemnos-test-rj-reset".as_ptr().cast(),
    parent: ptr::null(),
    properties: TR_RST_PROPS.get().cast(),
});

unsafe extern "C" fn tr_rst_reset(_rc: *mut lb::reset_controller_dev, id: c_ulong) -> c_int {
    match TR_BLOCKS_TABLE.get(id as usize) {
        Some(b) => {
            b.resets.fetch_add(1, Ordering::Relaxed);
            0
        }
        None => EINVAL.to_errno(),
    }
}

unsafe extern "C" fn tr_rst_nop(_rc: *mut lb::reset_controller_dev, _id: c_ulong) -> c_int {
    0
}

static TR_RST_OPS: Fixed<lb::reset_control_ops> = Fixed(lb::reset_control_ops {
    reset: Some(tr_rst_reset),
    assert: Some(tr_rst_nop),
    deassert: Some(tr_rst_nop),
    // SAFETY: the status operation absent (null).
    ..unsafe { core::mem::zeroed() }
});

/// Written by the reset driver's probe, before the controller is registered, and by the reset core.
static TR_RST: Shared<lb::reset_controller_dev> = Shared::new(lb::reset_controller_dev {
    ops: TR_RST_OPS.get(),
    owner: this_module(),
    nr_resets: TR_BLOCKS as c_uint,
    fwnode_reset_n_cells: 1,
    // SAFETY: the rest of a struct reset_controller_dev is the core's, zero in a static.
    ..unsafe { core::mem::zeroed() }
});

unsafe extern "C" fn tr_rst_probe(pdev: *mut bindings::platform_device) -> c_int {
    // SAFETY: the driver core passes the device being bound; the address of its device is taken.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: the controller is not registered yet: nothing else reads it.
    unsafe { (*TR_RST.get()).dev = dev };
    // SAFETY: dev_fwnode(dev), as its macro calls this.
    let fwnode = unsafe { bindings::__dev_fwnode(dev) };
    // SAFETY: as above: not registered yet.
    unsafe { (*TR_RST.get()).fwnode = fwnode };
    // SAFETY: the controller is a static, filled in above, and unregistered with the device.
    unsafe { lb::devm_reset_controller_register(dev, TR_RST.get()) }
}

static TR_RST_DRIVER: Shared<bindings::platform_driver> = Shared::new(bindings::platform_driver {
    probe: Some(tr_rst_probe),
    driver: bindings::device_driver {
        name: c"lemnos-test-rj-reset".as_ptr().cast(),
        // SAFETY: the rest of a struct device_driver is pointers, integers and flags, zero by default.
        ..unsafe { core::mem::zeroed() }
    },
    // SAFETY: every other callback absent (null) and no id table.
    ..unsafe { core::mem::zeroed() }
});

// --- what the test reaches through sysfs ---

fn tr_of(dev: *mut bindings::device) -> Option<&'static TrBlock> {
    TR_BLOCKS_TABLE.iter().find(|b| {
        let pdev = b.pdev.load(Ordering::Relaxed);
        // SAFETY: a block's device is registered until teardown; the address of its device is taken.
        !pdev.is_null() && unsafe { &raw mut (*pdev).dev } == dev
    })
}

/// The block and the "offset value" pair of a store, when the offset is a register of the window.
///
/// # Safety
///
/// `buf` is a NUL-terminated string.
unsafe fn tr_register(
    dev: *mut bindings::device,
    buf: *const c_char,
) -> Option<(&'static TrBlock, usize, u64)> {
    let b = tr_of(dev)?;
    // SAFETY: the caller's: `buf` is NUL-terminated.
    let (offset, value) = unsafe { hex_pair(buf) }?;
    if offset >= u64::from(b.window) || offset % 4 != 0 {
        return None;
    }
    Some((b, offset as usize, value))
}

unsafe extern "C" fn poke_store(
    dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const c_char,
    count: usize,
) -> isize {
    // SAFETY: sysfs passes the store's text, NUL-terminated.
    let Some((b, offset, value)) = (unsafe { tr_register(dev, buf) }) else {
        return EINVAL.to_errno() as isize;
    };
    // SAFETY: WRITE_ONCE of the register at `offset`, inside the window and aligned (checked above).
    unsafe {
        b.regs()
            .add(offset)
            .cast::<u32>()
            .write_volatile(value as u32)
    };
    count as isize
}

static DEV_ATTR_POKE: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"poke", 0o200, None, Some(poke_store)));

/// Raises interrupt `irq`: the count on success, as a store returns it.
fn tr_raise(irq: u32, count: usize) -> isize {
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

unsafe extern "C" fn irq_store(
    dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    _buf: *const c_char,
    count: usize,
) -> isize {
    let Some(b) = tr_of(dev) else {
        return ENODEV.to_errno() as isize;
    };
    tr_raise(b.irq[b.job_irq].load(Ordering::Relaxed), count)
}

static DEV_ATTR_IRQ: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"irq", 0o200, None, Some(irq_store)));

unsafe extern "C" fn irq_line_store(
    dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const c_char,
    count: usize,
) -> isize {
    let Some(b) = tr_of(dev) else {
        return ENODEV.to_errno() as isize;
    };
    let mut n: c_uint = 0;
    // SAFETY: sysfs passes the store's text, NUL-terminated.
    if unsafe { bindings::kstrtouint(buf, 0, &raw mut n) } != 0 || n as usize >= TR_IRQS {
        return EINVAL.to_errno() as isize;
    }
    tr_raise(b.irq[n as usize].load(Ordering::Relaxed), count)
}

static DEV_ATTR_IRQ_LINE: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"irq_line", 0o200, None, Some(irq_line_store)));

unsafe extern "C" fn stamp_store(
    dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const c_char,
    count: usize,
) -> isize {
    // SAFETY: sysfs passes the store's text, NUL-terminated.
    let Some((b, offset, value)) = (unsafe { tr_register(dev, buf) }) else {
        return EINVAL.to_errno() as isize;
    };
    // SAFETY: READ_ONCE of the register at `offset`, inside the window and aligned (checked by tr_register).
    let at = u64::from(unsafe { b.regs().add(offset).cast::<u32>().read_volatile() });
    // SAFETY: pfn_valid(PHYS_PFN(at)): any number may be asked about.
    if unsafe { fixture::lemnos_test_pfn_valid((at >> bindings::PAGE_SHIFT) as c_ulong) } == 0 {
        return EFAULT.to_errno() as isize;
    }
    // SAFETY: the address is of a page the kernel has (checked above); phys_to_virt is its direct mapping.
    let byte = unsafe { fixture::lemnos_test_phys_to_virt(at) }.cast::<u8>();
    // SAFETY: as the C does: the byte the block's DMA would write, in memory the check lent to the job.
    unsafe { byte.write(value as u8) };
    count as isize
}

static DEV_ATTR_STAMP: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"stamp", 0o200, None, Some(stamp_store)));

unsafe extern "C" fn resets_show(
    dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *mut c_char,
) -> isize {
    let Some(b) = tr_of(dev) else {
        return ENODEV.to_errno() as isize;
    };
    let resets: c_int = b.resets.load(Ordering::Relaxed);
    // SAFETY: sysfs passes a page for the text.
    unsafe { bindings::sysfs_emit(buf, c"%d\n".as_ptr().cast(), resets) as isize }
}

static DEV_ATTR_RESETS: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"resets", 0o444, Some(resets_show), None));

unsafe extern "C" fn regs_read(
    _f: *mut bindings::file,
    kobj: *mut bindings::kobject,
    _a: *const bindings::bin_attribute,
    buf: *mut c_char,
    off: bindings::loff_t,
    count: usize,
) -> isize {
    // SAFETY: the group is made on a platform device's kobject, which is its device's.
    let Some(b) = tr_of(unsafe { kobj_to_dev(kobj) }) else {
        return ENODEV.to_errno() as isize;
    };
    let window = i64::from(b.window);
    if off >= window {
        return 0;
    }
    let count = count.min((window - off) as usize);
    // SAFETY: `count` bytes at `off` are inside the window, and sysfs passes room for `count` bytes.
    unsafe { ptr::copy_nonoverlapping(b.regs().add(off as usize), buf.cast::<u8>(), count) };
    count as isize
}

static BIN_ATTR_REGS: Fixed<bindings::bin_attribute> = Fixed(bindings::bin_attribute {
    attr: attr(c"regs", 0o444),
    size: 0x9000,
    read: Some(regs_read),
    // SAFETY: no private data, no write, no mapping: zero.
    ..unsafe { core::mem::zeroed() }
});

static TR_ATTRS: Fixed<[*mut bindings::attribute; 6]> = Fixed([
    // The address of each attribute's `attr`, its first field.
    DEV_ATTR_POKE.get().cast(),
    DEV_ATTR_IRQ.get().cast(),
    DEV_ATTR_IRQ_LINE.get().cast(),
    DEV_ATTR_STAMP.get().cast(),
    DEV_ATTR_RESETS.get().cast(),
    ptr::null_mut(),
]);
static TR_BIN_ATTRS: Fixed<[*const bindings::bin_attribute; 2]> =
    Fixed([BIN_ATTR_REGS.get(), ptr::null()]);
static TR_GROUP: Fixed<bindings::attribute_group> = Fixed(bindings::attribute_group {
    __bindgen_anon_2: bindings::attribute_group__bindgen_ty_2 {
        attrs: TR_ATTRS.get().cast_mut().cast(),
    },
    bin_attrs: TR_BIN_ATTRS.get().cast(),
    // SAFETY: no name and no visibility callbacks: zero.
    ..unsafe { core::mem::zeroed() }
});

// --- setup ---

fn tr_teardown() {
    for b in &TR_BLOCKS_TABLE {
        let pdev = b.pdev.swap(ptr::null_mut(), Ordering::Relaxed);
        if !pdev.is_null() {
            // SAFETY: the device is registered; removing a group that was not made does nothing.
            unsafe { bindings::sysfs_remove_group(&raw mut (*pdev).dev.kobj, TR_GROUP.get()) };
            // SAFETY: the device was registered by tr_add, and this is its one unregistration.
            unsafe { bindings::platform_device_unregister(pdev) };
        }
        for irq in &b.irq {
            let irq = irq.swap(0, Ordering::Relaxed);
            if irq != 0 {
                // SAFETY: the mapping was made by tr_add, and this is its one disposal.
                unsafe { bindings::irq_dispose_mapping(irq) };
            }
        }
        let page = b.page.swap(ptr::null_mut(), Ordering::Relaxed);
        if !page.is_null() {
            // SAFETY: the pages were allocated by tr_add with this order, and nothing maps them now.
            unsafe { bindings::__free_pages(page, get_order(b.window as usize)) };
        }
    }
    let rst = TR_RST_PDEV.swap(ptr::null_mut(), Ordering::Relaxed);
    if !rst.is_null() {
        // SAFETY: the reset device was registered by init, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(rst) };
    }
    if TR_RST_DRIVER_UP.swap(false, Ordering::Relaxed) {
        // SAFETY: the driver was registered by init.
        unsafe { bindings::platform_driver_unregister(TR_RST_DRIVER.get()) };
    }
    let domain = TR_DOMAIN.swap(ptr::null_mut(), Ordering::Relaxed);
    if !domain.is_null() {
        // SAFETY: the domain was made by init, and every mapping in it is disposed of above.
        unsafe { lb::irq_domain_remove_sim(domain) };
    }
    let fwnode = TR_FWNODE.swap(ptr::null_mut(), Ordering::Relaxed);
    if !fwnode.is_null() {
        // SAFETY: the node was made by init, and its domain is gone.
        unsafe { bindings::irq_domain_free_fwnode(fwnode) };
    }
}

fn tr_add(i: usize) -> Result {
    let b = &TR_BLOCKS_TABLE[i];
    // PROPERTY_ENTRY_REF("resets", &tr_rst_node, i): the reset line of this block.
    let reference = bindings::software_node_ref_args {
        swnode: TR_RST_NODE.get(),
        nargs: 1,
        args: {
            let mut args = [0u64; 16];
            args[0] = i as u64;
            args
        },
        // SAFETY: no firmware node: the reference is by software node.
        ..unsafe { core::mem::zeroed() }
    };
    let props = [
        property_string(c"compatible", b.compatible),
        bindings::property_entry {
            name: c"resets".as_ptr().cast(),
            length: core::mem::size_of::<bindings::software_node_ref_args>(),
            is_inline: false,
            type_: bindings::dev_prop_type_DEV_PROP_REF,
            __bindgen_anon_1: bindings::property_entry__bindgen_ty_1 {
                pointer: (&raw const reference).cast(),
            },
        },
        property_end(),
    ];
    // SAFETY: resources of zeros, each filled in below before the device is registered.
    let mut res: [bindings::resource; 1 + TR_IRQS] = unsafe { core::mem::zeroed() };

    // SAFETY: alloc_pages(GFP_KERNEL | __GFP_ZERO, get_order(window)).
    let page = unsafe {
        bindings::alloc_pages(
            bindings::GFP_KERNEL | bindings::__GFP_ZERO,
            get_order(b.window as usize),
        )
    };
    if page.is_null() {
        return Err(ENOMEM);
    }
    b.page.store(page, Ordering::Relaxed);
    // SAFETY: page_to_phys of the pages just allocated.
    res[0] = res_mem(
        unsafe { fixture::lemnos_test_page_to_phys(page) },
        u64::from(b.window),
    );
    for n in 0..TR_IRQS {
        // SAFETY: irq_create_mapping(domain, hwirq), as its inline calls this; the domain is init's.
        let irq = unsafe {
            bindings::irq_create_mapping_affinity(
                TR_DOMAIN.load(Ordering::Relaxed),
                (i * TR_IRQS + n) as bindings::irq_hw_number_t,
                ptr::null(),
            )
        };
        if irq == 0 {
            return Err(EINVAL);
        }
        b.irq[n].store(irq, Ordering::Relaxed);
        res[1 + n] = res_irq(irq);
    }
    let info = bindings::platform_device_info {
        name: b.name.as_ptr().cast(),
        id: bindings::PLATFORM_DEVID_NONE,
        res: res.as_ptr(),
        num_res: res.len() as c_uint,
        // DMA_BIT_MASK(64).
        dma_mask: u64::MAX,
        properties: props.as_ptr(),
        // SAFETY: no parent, no firmware node and no data: zero.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: the information, its resources and its properties live for the call, which copies them.
    let pdev = from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const info) })?;
    b.pdev.store(pdev, Ordering::Relaxed);
    // SAFETY: the device is registered; the group is a static.
    to_result(unsafe { bindings::sysfs_create_group(&raw mut (*pdev).dev.kobj, TR_GROUP.get()) })
}

fn tr_setup() -> Result {
    // SAFETY: irq_domain_alloc_named_fwnode("lemnos-test-regjob"), as its inline calls this.
    let fwnode = unsafe {
        bindings::__irq_domain_alloc_fwnode(
            bindings::IRQCHIP_FWNODE_NAMED,
            0,
            c"lemnos-test-regjob".as_ptr().cast(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if fwnode.is_null() {
        return Err(ENOMEM);
    }
    TR_FWNODE.store(fwnode, Ordering::Relaxed);
    // SAFETY: the node was made just above.
    let domain = from_err_ptr(unsafe {
        lb::irq_domain_create_sim(fwnode, (TR_BLOCKS * TR_IRQS) as c_uint)
    })?;
    TR_DOMAIN.store(domain, Ordering::Relaxed);
    // SAFETY: platform_driver_register, as its macro calls this with THIS_MODULE and the module's name; the
    // driver is a static.
    to_result(unsafe {
        bindings::__platform_driver_register(
            TR_RST_DRIVER.get(),
            this_module(),
            c"lemnos_test_regjob".as_ptr().cast(),
        )
    })?;
    TR_RST_DRIVER_UP.store(true, Ordering::Relaxed);
    let info = bindings::platform_device_info {
        name: c"lemnos-test-rj-reset".as_ptr().cast(),
        id: bindings::PLATFORM_DEVID_NONE,
        swnode: TR_RST_NODE.get(),
        // SAFETY: no parent, no resources and no data: zero.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: the information lives for the call; the node is a static.
    let rst = from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const info) })?;
    TR_RST_PDEV.store(rst, Ordering::Relaxed);
    // SAFETY: the device is registered: its driver is read once, as the C does.
    if unsafe { (*rst).dev.driver }.is_null() {
        return Err(ENODEV);
    }
    for i in 0..TR_BLOCKS {
        tr_add(i)?;
    }
    Ok(())
}

struct TestRegjob;

impl kernel::Module for TestRegjob {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        if let Err(e) = tr_setup() {
            tr_teardown();
            return Err(e);
        }
        Ok(TestRegjob)
    }
}

impl Drop for TestRegjob {
    fn drop(&mut self) {
        tr_teardown();
    }
}

module! {
    type: TestRegjob,
    name: "lemnos_test_regjob",
    authors: ["Lemnos"],
    description: "Fake blocks for the Lemnos module's register jobs",
    license: "GPL",
}
