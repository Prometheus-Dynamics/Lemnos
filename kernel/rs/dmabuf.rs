// SPDX-License-Identifier: GPL-2.0
//! A block's DMA memory, as the opener of /dev/lemnos is given it: buffers allocated for the block and
//! exported as dma-bufs (LEMNOS_IOC_ALLOC), and dma-bufs mapped to the block's device addresses
//! (LEMNOS_IOC_IMPORT, LEMNOS_IOC_RELEASE).
//!
//! A buffer can only be freed while its device still has its DMA setup, and mappings of it may exist in
//! user space, so a block's unbind waits for its buffers (`wait_drained`). Each buffer counts in its
//! block's `bufs`; the last free wakes the unbind. That wake is global, not the block's: the last
//! buffer's free may race the block being freed.

use crate::bindings as lb;
use core::{
    ffi::c_void,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicI32, Ordering},
};
use kernel::{
    bindings,
    device::Device,
    error::{from_err_ptr, to_result},
    ffi::{c_int, c_long, c_ulong},
    prelude::*,
    sync::LockClassKey,
    types::Opaque,
};

/// A `struct wait_queue_head` that is a static: made at module load (`init`), never freed.
pub(crate) struct StaticWq(Opaque<bindings::wait_queue_head>);

// SAFETY: a wait queue head is made to be shared: every use goes through the kernel's own lock in it.
unsafe impl Sync for StaticWq {}

impl StaticWq {
    pub(crate) const fn new() -> Self {
        Self(Opaque::uninit())
    }

    /// init_waitqueue_head, under the given name and class.
    ///
    /// # Safety
    /// Called once, at module load, before any other use.
    pub(crate) unsafe fn init(&self, name: &'static core::ffi::CStr, key: Pin<&'static LockClassKey>) {
        // SAFETY: initialises the queue in place, before any use; the name and the class are static.
        unsafe { bindings::__init_waitqueue_head(self.0.get(), name.as_ptr().cast(), key.as_ptr()) };
    }

    /// The queue, for the kernel's calls.
    pub(crate) fn get(&self) -> *mut bindings::wait_queue_head {
        self.0.get()
    }

    /// wake_up_all.
    fn wake_all(&self) {
        // SAFETY: the queue is initialised (init); __wake_up is the exported wake, for any context.
        unsafe { bindings::__wake_up(self.get(), bindings::TASK_NORMAL, 0, ptr::null_mut()) };
    }

    /// wait_event_timeout(q, done(), timeout): true when `done` held, false when `timeout` jiffies passed
    /// and it still did not. Process context only.
    fn wait_timeout(&self, timeout: c_ulong, mut done: impl FnMut() -> bool) -> bool {
        if done() {
            return true;
        }
        let mut entry = core::mem::MaybeUninit::<bindings::wait_queue_entry>::uninit();
        let entry = entry.as_mut_ptr();
        // SAFETY: initialises the entry, on this stack frame, before any use.
        unsafe { bindings::init_wait_entry(entry, 0) };
        let mut left = timeout as c_long;
        let held = loop {
            // SAFETY: the queue is initialised and the entry is the one made above.
            unsafe { bindings::prepare_to_wait_event(self.get(), entry, bindings::TASK_UNINTERRUPTIBLE as c_int) };
            if done() {
                break true;
            }
            if left <= 0 {
                break false;
            }
            // SAFETY: sleeps in the state set above, in process context.
            left = unsafe { bindings::schedule_timeout(left) };
        };
        // SAFETY: the entry prepared above, on this queue.
        unsafe { bindings::finish_wait(self.get(), entry) };
        held
    }
}

/// Woken when a block's last buffer is freed.
static DRAINED: StaticWq = StaticWq::new();

/// Makes the wait queue of the unbinds.
///
/// # Safety
/// Called once, at module load, before any block can be bound.
pub(crate) unsafe fn init() {
    // SAFETY: the caller's contract.
    unsafe { DRAINED.init(c"lem_drained", kernel::static_lock_class!()) };
}

/// THIS_MODULE: the kernel's own `__this_module` for a module, NULL built in.
fn this_module() -> *mut bindings::module {
    #[cfg(MODULE)]
    {
        // `__this_module` is made by the kernel at load and lives until the module is unloaded.
        (&raw const bindings::__this_module).cast_mut()
    }
    #[cfg(not(MODULE))]
    {
        ptr::null_mut()
    }
}

// --- DMA buffers exported as dma-bufs ---

/// One buffer: what its dma-buf's `priv` points at.
struct Buf {
    /// The block's count of buffers not yet freed.
    bufs: *const AtomicI32,
    /// The block's device, held (get_device) for the buffer's life.
    dev: *mut bindings::device,
    cpu: *mut c_void,
    addr: bindings::dma_addr_t,
    size: usize,
}

/// The buffer of a dma-buf this file exported.
///
/// # Safety
/// `buf` is a dma-buf exported by `alloc`, not yet released.
unsafe fn buf_of<'a>(buf: *mut bindings::dma_buf) -> &'a Buf {
    // SAFETY: the caller's dma-buf, whose priv is the Buf `alloc` gave it, live until its release.
    unsafe { &*((*buf).priv_ as *const Buf) }
}

/// Frees a table made by `buf_map`.
///
/// # Safety
/// `sgt` came from `KBox::into_raw` in `buf_map`, and nothing else holds it.
unsafe fn table_free(sgt: *mut bindings::sg_table) {
    // SAFETY: the caller's contract.
    drop(unsafe { KBox::from_raw(sgt) });
}

unsafe extern "C" fn buf_map(
    att: *mut bindings::dma_buf_attachment,
    dir: bindings::dma_data_direction,
) -> *mut bindings::sg_table {
    // SAFETY: the dma-buf core calls this with a live attachment of one of our dma-bufs.
    let b = unsafe { buf_of((*att).dmabuf) };
    // SAFETY: a struct sg_table of zeroes is an empty table.
    let sgt = match KBox::new(unsafe { core::mem::zeroed::<bindings::sg_table>() }, GFP_KERNEL) {
        Ok(t) => KBox::into_raw(t),
        Err(e) => return Error::from(e).to_ptr(),
    };
    // SAFETY: the buffer's memory, as dma_alloc_attrs gave it for this device (dma_get_sgtable).
    let ret = unsafe { bindings::dma_get_sgtable_attrs(b.dev, sgt, b.cpu, b.addr, b.size, 0) };
    if ret != 0 {
        // SAFETY: the table made above, never filled.
        unsafe { table_free(sgt) };
        return Error::from_errno(ret).to_ptr();
    }
    // SAFETY: the attachment's device is live for the attachment's life; the table was filled above.
    let ret = unsafe { bindings::dma_map_sgtable((*att).dev, sgt, dir, 0) };
    if ret != 0 {
        // SAFETY: the table filled above, not mapped.
        unsafe { bindings::sg_free_table(sgt) };
        // SAFETY: the table made above, now empty.
        unsafe { table_free(sgt) };
        return Error::from_errno(ret).to_ptr();
    }
    sgt
}

unsafe extern "C" fn buf_unmap(
    att: *mut bindings::dma_buf_attachment,
    sgt: *mut bindings::sg_table,
    dir: bindings::dma_data_direction,
) {
    // SAFETY: the table `buf_map` mapped for this attachment's device, in this direction.
    unsafe { bindings::dma_unmap_sgtable((*att).dev, sgt, dir, 0) };
    // SAFETY: the table `buf_map` filled.
    unsafe { bindings::sg_free_table(sgt) };
    // SAFETY: the table `buf_map` made; the dma-buf core gives it back exactly once.
    unsafe { table_free(sgt) };
}

unsafe extern "C" fn buf_mmap(buf: *mut bindings::dma_buf, vma: *mut bindings::vm_area_struct) -> c_int {
    // SAFETY: the dma-buf core calls this with one of our dma-bufs.
    let b = unsafe { buf_of(buf) };
    // SAFETY: the buffer's memory, as dma_alloc_attrs gave it for this device (dma_mmap_coherent).
    unsafe { bindings::dma_mmap_attrs(b.dev, vma, b.cpu, b.addr, b.size, 0) }
}

/// The stream test source writes through this; nothing else in the kernel needs it.
unsafe extern "C" fn buf_vmap(buf: *mut bindings::dma_buf, map: *mut bindings::iosys_map) -> c_int {
    // SAFETY: the dma-buf core calls this with one of our dma-bufs.
    let b = unsafe { buf_of(buf) };
    // SAFETY: the map is the caller's to fill (iosys_map_set_vaddr: the address, not I/O memory).
    unsafe {
        (*map).__bindgen_anon_1.vaddr = b.cpu;
        (*map).is_iomem = false;
    }
    0
}

/// Frees a buffer and its memory; the block's last one wakes its unbind.
///
/// # Safety
/// `b` came from `alloc`, and nothing else refers to it any more.
unsafe fn buf_free(b: *mut Buf) {
    // SAFETY: the caller's contract.
    let b = unsafe { KBox::from_raw(b) };
    // SAFETY: the memory dma_alloc_attrs gave for this device, with no mapping left (dma_free_coherent).
    unsafe { bindings::dma_free_attrs(b.dev, b.size, b.cpu, b.addr, 0) };
    // SAFETY: the reference `alloc` took.
    unsafe { bindings::put_device(b.dev) };
    let bufs = b.bufs;
    drop(b);
    // The last touch of the block: once this reaches zero the unbind may free it.
    // SAFETY: the block lives until its count is zero, which this buffer kept it from.
    if unsafe { (*bufs).fetch_sub(1, Ordering::AcqRel) } == 1 {
        DRAINED.wake_all();
    }
}

unsafe extern "C" fn buf_release(buf: *mut bindings::dma_buf) {
    // SAFETY: the dma-buf's last reference is gone; its priv is the Buf `alloc` gave it.
    unsafe { buf_free((*buf).priv_.cast()) };
}

/// `struct dma_buf_ops` of the buffers.
#[repr(transparent)]
struct BufOps(bindings::dma_buf_ops);

// SAFETY: never written; the dma-buf core only reads it.
unsafe impl Sync for BufOps {}

static BUF_OPS: BufOps = BufOps(bindings::dma_buf_ops {
    map_dma_buf: Some(buf_map),
    unmap_dma_buf: Some(buf_unmap),
    mmap: Some(buf_mmap),
    vmap: Some(buf_vmap),
    release: Some(buf_release),
    // SAFETY: every other operation absent (null), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
});

const PAGE_SIZE: u64 = kernel::page::PAGE_SIZE as u64;

/// LEMNOS_IOC_ALLOC: a buffer of the block's DMA memory, as a dma-buf.
///
/// # Safety
/// `dev` is the bound block's device and `bufs` its count, both live until the count is zero again; `a`
/// is the request, in kernel memory.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_alloc(
    dev: *mut bindings::device,
    bufs: *const AtomicI32,
    a: *mut lb::lemnos_alloc,
) -> c_long {
    // SAFETY: the caller's request.
    let a = unsafe { &mut *a };
    if a.flags != 0 || a.size == 0 || a.size > bindings::SZ_1G as u64 {
        return EINVAL.to_errno() as c_long;
    }
    let size = ((a.size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) as usize;
    let b = Buf { bufs, dev, cpu: ptr::null_mut(), addr: 0, size };
    let b = match KBox::new(b, GFP_KERNEL) {
        Ok(b) => KBox::into_raw(b),
        Err(_) => return ENOMEM.to_errno() as c_long,
    };
    // SAFETY: the caller's device; the reference is put by buf_free, or below.
    unsafe { bindings::get_device(dev) };
    // SAFETY: the device is bound, so its DMA is set up; `b` is ours (dma_alloc_coherent).
    let cpu = unsafe { bindings::dma_alloc_attrs(dev, size, &raw mut (*b).addr, bindings::GFP_KERNEL, 0) };
    if cpu.is_null() {
        // SAFETY: the reference taken above.
        unsafe { bindings::put_device(dev) };
        // SAFETY: the box made above, which nothing else has seen.
        drop(unsafe { KBox::from_raw(b) });
        return ENOMEM.to_errno() as c_long;
    }
    // SAFETY: `b` is ours until it is exported.
    unsafe { (*b).cpu = cpu };
    // SAFETY: the caller's count.
    unsafe { (*bufs).fetch_add(1, Ordering::AcqRel) };
    // DEFINE_DMA_BUF_EXPORT_INFO: the module's name and the module itself.
    let info = bindings::dma_buf_export_info {
        exp_name: c"lemnos".as_ptr().cast(),
        owner: this_module(),
        ops: &raw const BUF_OPS.0,
        size,
        flags: bindings::O_RDWR as c_int,
        resv: ptr::null_mut(),
        priv_: b.cast(),
    };
    // SAFETY: the description above, read only during the call.
    let buf = match from_err_ptr(unsafe { bindings::dma_buf_export(&raw const info) }) {
        Ok(buf) => buf,
        Err(e) => {
            // SAFETY: never exported, so still ours.
            unsafe { buf_free(b) };
            return e.to_errno() as c_long;
        }
    };
    // SAFETY: the dma-buf made above, whose reference the fd takes.
    let fd = unsafe { bindings::dma_buf_fd(buf, bindings::O_CLOEXEC as c_int) };
    if fd < 0 {
        // SAFETY: the reference dma_buf_export gave; its release frees `b`.
        unsafe { bindings::dma_buf_put(buf) };
        return fd as c_long;
    }
    a.size = size as u64;
    a.fd = fd;
    // SAFETY: the buffer lives while its dma-buf does; the address is read only.
    a.addr = unsafe { (*b).addr };
    0
}

/// The unbind's wait for the block's buffers: until every one is freed. Killing their holder ends it.
///
/// # Safety
/// `dev` is the device being unbound and `bufs` its block's count.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_bufs_wait(dev: *mut bindings::device, bufs: *const AtomicI32) {
    // SAFETY: the caller's count, live until this returns.
    let bufs = unsafe { &*bufs };
    while !DRAINED.wait_timeout(10 * bindings::HZ as c_ulong, || bufs.load(Ordering::Acquire) == 0) {
        // SAFETY: the caller's device, live across the unbind.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "unbind waits for {} DMA buffers still held\n", bufs.load(Ordering::Acquire));
    }
}

// --- dma-bufs imported to device addresses ---

/// An imported dma-buf: the entry of a block's `imports` at its handle.
struct Import {
    buf: *mut bindings::dma_buf,
    att: *mut bindings::dma_buf_attachment,
    sgt: *mut bindings::sg_table,
}

const BIDIRECTIONAL: bindings::dma_data_direction = bindings::dma_data_direction::DMA_BIDIRECTIONAL;

/// Unmaps and lets go of an import.
///
/// # Safety
/// `im` came from `lem_import` and is in no table any more.
unsafe fn import_free(im: *mut Import) {
    // SAFETY: the caller's contract.
    let im = unsafe { KBox::from_raw(im) };
    // SAFETY: the mapping `lem_import` made of this attachment.
    unsafe { bindings::dma_buf_unmap_attachment_unlocked(im.att, im.sgt, BIDIRECTIONAL) };
    // SAFETY: the attachment `lem_import` made.
    unsafe { bindings::dma_buf_detach(im.buf, im.att) };
    // SAFETY: the reference `lem_import` took.
    unsafe { bindings::dma_buf_put(im.buf) };
}

/// Frees every import of a block: its file closed, or it is being unbound.
///
/// # Safety
/// `imports` is the block's table (an allocating xarray of `Import`), with no request running on it.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_imports_free_all(imports: *mut bindings::xarray) {
    let mut handle: c_ulong = 0;
    // SAFETY: the caller's table (xa_for_each: the first entry).
    let mut im = unsafe { bindings::xa_find(imports, &raw mut handle, c_ulong::MAX, bindings::XA_PRESENT) };
    while !im.is_null() {
        // SAFETY: the caller's table; the entry at `handle` is `im`.
        unsafe { bindings::xa_erase(imports, handle) };
        // SAFETY: an entry is an Import from lem_import, and it is out of the table now.
        unsafe { import_free(im.cast()) };
        // SAFETY: the caller's table (xa_for_each: the next entry).
        im = unsafe { bindings::xa_find_after(imports, &raw mut handle, c_ulong::MAX, bindings::XA_PRESENT) };
    }
}

/// Attaches and maps the dma-buf `fd` for `dev`.
///
/// # Safety
/// `dev` is a bound block's device.
unsafe fn import_map(dev: *mut bindings::device, fd: c_int) -> Result<Import> {
    // SAFETY: any fd may be asked for; a dma-buf comes back with a reference, anything else as an error.
    let buf = from_err_ptr(unsafe { bindings::dma_buf_get(fd) })?;
    // SAFETY: the dma-buf held above and the caller's device.
    let att = match from_err_ptr(unsafe { bindings::dma_buf_attach(buf, dev) }) {
        Ok(att) => att,
        Err(e) => {
            // SAFETY: the reference taken above.
            unsafe { bindings::dma_buf_put(buf) };
            return Err(e);
        }
    };
    // SAFETY: the attachment made above.
    let sgt = match from_err_ptr(unsafe { bindings::dma_buf_map_attachment_unlocked(att, BIDIRECTIONAL) }) {
        Ok(sgt) => sgt,
        Err(e) => {
            // SAFETY: the attachment made above.
            unsafe { bindings::dma_buf_detach(buf, att) };
            // SAFETY: the reference taken above.
            unsafe { bindings::dma_buf_put(buf) };
            return Err(e);
        }
    };
    Ok(Import { buf, att, sgt })
}

/// LEMNOS_IOC_IMPORT: a dma-buf mapped to the block's device addresses, as one contiguous range.
///
/// # Safety
/// `dev` is the bound block's device and `imports` its table; `a` is the request, in kernel memory.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_import(
    dev: *mut bindings::device,
    imports: *mut bindings::xarray,
    a: *mut lb::lemnos_import,
) -> c_long {
    // SAFETY: the caller's request.
    let a = unsafe { &mut *a };
    // SAFETY: the caller's device.
    let im = match unsafe { import_map(dev, a.fd) } {
        Ok(im) => im,
        Err(e) => return e.to_errno() as c_long,
    };
    let (buf, att, sgt) = (im.buf, im.att, im.sgt);
    let im = match KBox::new(im, GFP_KERNEL) {
        Ok(im) => KBox::into_raw(im),
        Err(_) => {
            // A box that could not be made: undo the mapping by hand, as import_free does.
            // SAFETY: the mapping, the attachment and the reference import_map made.
            unsafe {
                bindings::dma_buf_unmap_attachment_unlocked(att, sgt, BIDIRECTIONAL);
                bindings::dma_buf_detach(buf, att);
                bindings::dma_buf_put(buf);
            }
            return ENOMEM.to_errno() as c_long;
        }
    };
    // One contiguous range in the block's address space, or refused.
    // SAFETY: the mapped table: `nents` entries from `sgl` carry device addresses.
    let (first, nents) = unsafe { ((*sgt).sgl, (*sgt).nents) };
    // SAFETY: a mapped table has a first entry.
    let start = unsafe { bindings::sg_dma_address(first) };
    let (mut next, mut size, mut sg) = (start, 0u64, first);
    for _ in 0..nents {
        // SAFETY: `sg` is one of the table's mapped entries.
        let (addr, len) = unsafe { (bindings::sg_dma_address(sg), bindings::sg_dma_len(sg)) };
        if addr != next {
            // SAFETY: never put in the table, so still ours.
            unsafe { import_free(im) };
            return EINVAL.to_errno() as c_long;
        }
        next += len as bindings::dma_addr_t;
        size += len as u64;
        // SAFETY: the entry after one of the table's entries, or NULL after the last.
        sg = unsafe { bindings::sg_next(sg) };
    }
    let mut handle = 0u32;
    // xa_alloc: any free handle of 32 bits (the table starts at 1).
    let limit = bindings::xa_limit { max: u32::MAX, min: 0 };
    // SAFETY: the caller's table; its lock is held across __xa_alloc, which may drop it to allocate.
    let ret = unsafe {
        bindings::xa_lock(imports);
        let ret = bindings::__xa_alloc(imports, &raw mut handle, im.cast(), limit, bindings::GFP_KERNEL);
        bindings::xa_unlock(imports);
        ret
    };
    if let Err(e) = to_result(ret) {
        // SAFETY: never put in the table, so still ours.
        unsafe { import_free(im) };
        return e.to_errno() as c_long;
    }
    a.handle = handle;
    a.addr = start as u64;
    a.size = size;
    0
}

/// LEMNOS_IOC_RELEASE: unmaps one import.
///
/// # Safety
/// `imports` is the bound block's table.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_release_import(imports: *mut bindings::xarray, handle: u32) -> c_long {
    // SAFETY: the caller's table.
    let im = unsafe { bindings::xa_erase(imports, handle as c_ulong) };
    if im.is_null() {
        return ENOENT.to_errno() as c_long;
    }
    // SAFETY: an entry is an Import from lem_import, and it is out of the table now.
    unsafe { import_free(im.cast()) };
    0
}
