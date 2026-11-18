// SPDX-License-Identifier: GPL-2.0
//! The consumers' own pages (the stream port): a reader's marks page and the consumer's doorbell.
//!
//! A reader holds a buffer by marking it in its marks page, a memfd of its own. LEMNOS_STREAM_IOC_READER
//! pins that page for the stream's life and maps it read-only here: the module reads the marks before
//! it reuses a buffer (rs/stream_ring.rs) and never writes them. The registration is a file: closing it
//! (an exit closes it too) or LEMNOS_READER_IOC_UNREGISTER frees the slot, and the reader's marks hold
//! nothing from then on.
//!
//! The doorbell (LEMNOS_STREAM_IOC_BELL, shm/ring-doorbell's publisher side) is the eventfd the
//! consumer sleeps on and its page, mapped read-only. Word 0 of the page is sleeps << 1 | asleep,
//! written by the consumer only. The module rings the eventfd once per sleep, after a publish that
//! finds the consumer asleep, and remembers the sleep it rang for, so no sleep is rung twice.
//!
//! Both pages must be shmem files. `shmem_file()` is an inline in the header through 7.3.3 and an
//! unexported function from 7.3.5 on, so a module cannot link it there; its body through 7.3.3 was the
//! test below, and `shmem_mapping()` is exported in both, so the module keeps the test here.

use crate::bindings as lb;
use crate::stream_file::{Stream, this_module};
use crate::stream_ring::{MARKS_AT, MARKS_LAYOUT, Reader, smp_mb, smp_rmb};
use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};
use kernel::{
    ffi::{c_int, c_long, c_uint, c_ulong},
    alloc::{KBox, KVVec, KVec, flags::GFP_KERNEL},
    bindings,
    error::{code::*, from_err_ptr},
    ioctl::_IO,
    prelude::*,
    sync::{Arc, ArcBorrow},
};

const LEMNOS_READER_IOC_UNREGISTER: c_uint = _IO(b'L' as u32, 0x88);

// The kernel's own types, by pointer; bindgen makes some of what they hold empty structs.
#[allow(improper_ctypes)]
unsafe extern "C" {
    /// lemnos_helpers.c: i_size_read().
    fn lemnos_i_size_read(inode: *const bindings::inode) -> bindings::loff_t;
    /// lemnos_helpers.c: SHMEM_I(inode)->seals.
    fn lemnos_shmem_seals(inode: *mut bindings::inode) -> c_uint;
    /// lemnos_helpers.c: shmem_read_mapping_page().
    fn lemnos_shmem_read_mapping_page(mapping: *mut bindings::address_space, index: c_ulong) -> *mut bindings::page;
    /// lemnos_helpers.c: put_page().
    fn lemnos_put_page(page: *mut bindings::page);
    /// lemnos_helpers.c: vmap(pages, count, VM_MAP, PAGE_KERNEL_RO).
    fn lemnos_vmap_ro(pages: *mut *mut bindings::page, count: c_uint) -> *mut c_void;
}

/// A file from fget(), put when dropped.
pub(crate) struct FileRef(*mut bindings::file);

impl FileRef {
    /// fget(fd): the file, or EBADF.
    pub(crate) fn get(fd: c_int) -> Result<Self> {
        // SAFETY: takes a reference on the file `fd` names in this process, or answers null.
        let file = unsafe { bindings::fget(fd as c_uint) };
        if file.is_null() { Err(EBADF) } else { Ok(Self(file)) }
    }

    fn inode(&self) -> *mut bindings::inode {
        // SAFETY: the file is held; its inode is set for its whole life.
        unsafe { (*self.0).f_inode }
    }

    fn mapping(&self) -> *mut bindings::address_space {
        // SAFETY: the file is held; its mapping is set when it is opened.
        unsafe { (*self.0).f_mapping }
    }

    /// The file's size (i_size_read).
    fn size(&self) -> i64 {
        // SAFETY: the inode of a held file.
        unsafe { lemnos_i_size_read(self.inode()) }
    }

    /// Whether the file is a shmem file: the kernel's shmem_file(), which a module cannot call on 7.3.5.
    fn is_shmem(&self) -> bool {
        if !cfg!(CONFIG_SHMEM) || self.mapping().is_null() {
            return false;
        }
        // SAFETY: the mapping of a held file.
        unsafe { lb::shmem_mapping(self.mapping()) }
    }

    /// The seals of a shmem file's inode. Only for a file `is_shmem` says is one.
    fn seals(&self) -> c_uint {
        // SAFETY: the inode of a held shmem file, so it is inside a struct shmem_inode_info.
        unsafe { lemnos_shmem_seals(self.inode()) }
    }

    /// Page `index` of a shmem file, read in if need be and returned with a reference.
    fn page(&self, index: usize) -> Result<*mut bindings::page> {
        // SAFETY: the mapping of a held shmem file; the page comes back referenced, or an error pointer.
        from_err_ptr(unsafe { lemnos_shmem_read_mapping_page(self.mapping(), index as c_ulong) })
    }
}

impl Drop for FileRef {
    fn drop(&mut self) {
        // SAFETY: the reference fget() took, dropped once.
        unsafe { bindings::fput(self.0) };
    }
}

impl Reader {
    /// Pins the marks page of `file` for `buffers` buffers, read-only: whole pages up to what the
    /// marks need, or the file's end when it is shorter (a page of an older layout is smaller).
    fn pin(&mut self, file: &FileRef, bytes: usize) -> Result {
        let npages = bytes.min(file.size().max(0) as usize).div_ceil(bindings::PAGE_SIZE);
        self.pages = KVec::with_capacity(npages, GFP_KERNEL)?;
        for i in 0..npages {
            let page = file.page(i)?;
            // The capacity was reserved above, so this neither allocates nor fails.
            let _ = self.pages.push_within_capacity(page);
        }
        // SAFETY: `npages` pages, each referenced until this reader is dropped; mapped read-only.
        self.marks = unsafe { lemnos_vmap_ro(self.pages.as_mut_ptr(), npages as c_uint) }.cast();
        if self.marks.is_null() { Err(ENOMEM) } else { Ok(()) }
    }
}

impl Drop for Reader {
    /// Unpins the marks page, whatever part of it was pinned. Sleeps: never under the stream's lock.
    fn drop(&mut self) {
        if !self.marks.is_null() {
            // SAFETY: the mapping `pin` made, unmapped once; nothing reads it once the slot is cleared.
            unsafe { lb::vunmap(self.marks.cast()) };
        }
        for page in self.pages.iter() {
            // SAFETY: the reference shmem_read_mapping_page() gave, dropped once.
            unsafe { lemnos_put_page(*page) };
        }
    }
}

/// What a reader's file holds: the registration it made. Closing the file or
/// LEMNOS_READER_IOC_UNREGISTER frees the slot, if the slot still carries this registration. The file
/// holds a reference on the stream.
struct ReaderHandle {
    s: Arc<Stream>,
    slot: usize,
    /// The registration's id; 0 once it is dropped.
    id: AtomicU64,
}

/// `struct file_operations` of a table the kernel only reads.
#[repr(transparent)]
pub(crate) struct Fops(pub(crate) bindings::file_operations);

// SAFETY: never written after it is made; the kernel only reads it.
unsafe impl Sync for Fops {}

static READER_FOPS: Fops = Fops(bindings::file_operations {
    owner: this_module(),
    release: Some(reader_file_release),
    unlocked_ioctl: Some(reader_file_ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: a file_operations of null pointers is the table with nothing else set.
    ..unsafe { core::mem::zeroed() }
});

/// LEMNOS_STREAM_IOC_READER: pins a reader's marks page and gives the reader a slot, and a file
/// (`a.reader_fd`) that holds the registration. The page must be a shmem file, sealed against
/// shrinking and growing, and big enough for the buffers. Its pages are mapped read-only and never
/// written. The slot is published under the stream's lock, where the frame path reads it. The control
/// mutex is held, which orders the table's changes.
pub(crate) fn add(st: ArcBorrow<'_, Stream>, a: &mut lb::lemnos_stream_reader) -> Result {
    const WANT: c_uint = bindings::F_SEAL_SHRINK | bindings::F_SEAL_GROW;
    let g = st.geom();
    let bytes = (MARKS_AT + 2 * g.buffers as usize) * 8;
    let slot = st.lock().reader.iter().position(|r| r.marks.is_null()).ok_or(ENOSPC)?;

    let marks = FileRef::get(a.fd)?;
    // The page's size is checked after its layout: a page of an older layout is smaller.
    if !marks.is_shmem() || marks.seals() & WANT != WANT || marks.size() < 8 {
        return Err(EINVAL);
    }
    // Dropped on every refusal below, which unpins whatever of it was pinned.
    let mut rd = Reader::empty();
    rd.pin(&marks, bytes)?;
    // The layout first: a page of another layout is refused as that, with both values in the
    // caller's argument.
    let found = rd.layout();
    if found != MARKS_LAYOUT {
        pr_err!("lemnos: a reader's marks page is layout {}; this build reads layout {}\n", found, MARKS_LAYOUT);
        a.refused = lb::LEMNOS_REFUSED_LAYOUT;
        a.layout = lb::LEMNOS_LAYOUT_MARKS;
        a.found = found;
        a.expected = MARKS_LAYOUT;
        return Err(EPROTO);
    }
    if marks.size() < bytes as i64 {
        return Err(EINVAL);
    }
    rd.max_held = a.max_held;
    rd.max_hold_ns = a.max_hold_ns;
    rd.seen = KVVec::from_elem(0u64, 2 * g.buffers as usize, GFP_KERNEL)?;

    let handle = KBox::new(ReaderHandle { s: st.into(), slot, id: AtomicU64::new(0) }, GFP_KERNEL)?;
    // SAFETY: reserves a descriptor in this process; given back below if the file can't be made.
    let fd = unsafe { bindings::get_unused_fd_flags(bindings::O_CLOEXEC) };
    if fd < 0 {
        return Err(Error::from_errno(fd));
    }
    let handle = KBox::into_raw(handle);
    // SAFETY: the name and the table are static; the handle is the file's from here, freed by its release.
    let file = unsafe {
        lb::anon_inode_getfile(c"[lemnos-reader]".as_ptr().cast(), &raw const READER_FOPS.0, handle.cast(), bindings::O_RDWR as c_int)
    };
    let file = match from_err_ptr(file) {
        Ok(file) => file,
        Err(e) => {
            // SAFETY: the descriptor reserved above, never installed.
            unsafe { bindings::put_unused_fd(fd as c_uint) };
            // SAFETY: the handle made above, which no file took.
            drop(unsafe { KBox::from_raw(handle) });
            return Err(e);
        }
    };

    // A new slot's cut-off word says nothing about the reader it is given to.
    g.write_release(g.cut_off_at(slot), 0);
    // The slot was empty (found above, and the control mutex is held); what was in it is dropped
    // with the lock let go all the same.
    let _empty = {
        let mut f = st.lock();
        f.reader_ids += 1;
        rd.id = f.reader_ids;
        // SAFETY: the handle is the new file's, which nothing else can reach before fd_install below.
        unsafe { &*handle }.id.store(rd.id, Ordering::Relaxed);
        core::mem::replace(&mut f.reader[slot], rd)
    };
    a.slot = slot as u32;
    a.reader_fd = fd;
    // SAFETY: the descriptor and the file made above; the file is the descriptor's from here.
    unsafe { bindings::fd_install(fd as c_uint, file) };
    Ok(())
}

/// Frees a reader's slot if its handle still holds the registration: its marks stop holding at once,
/// and the slot can be given out again. Takes the control mutex, so the caller must not hold it.
fn drop_registration(h: &ReaderHandle) -> bool {
    let taken = {
        let _ctl = h.s.ctl();
        let mut f = h.s.lock();
        let id = h.id.load(Ordering::Relaxed);
        if id != 0 && f.reader[h.slot].id == id {
            Some(core::mem::replace(&mut f.reader[h.slot], Reader::empty()))
        } else {
            None
        }
    };
    h.id.store(0, Ordering::Relaxed);
    // The page is unpinned here, with both locks dropped.
    taken.is_some()
}

unsafe extern "C" fn reader_file_release(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    // SAFETY: the VFS calls this with the file being released, still live.
    let h = unsafe { (*file).private_data }.cast::<ReaderHandle>();
    // SAFETY: the handle `add` gave this file, taken back once, when its last reference closes.
    let h = unsafe { KBox::from_raw(h) };
    drop_registration(&h);
    // Dropping the handle drops its reference on the stream.
    0
}

/// LEMNOS_READER_IOC_UNREGISTER: the explicit form of closing the file.
unsafe extern "C" fn reader_file_ioctl(file: *mut bindings::file, cmd: c_uint, _arg: c_ulong) -> c_long {
    if cmd != LEMNOS_READER_IOC_UNREGISTER {
        return ENOTTY.to_errno() as c_long;
    }
    // SAFETY: the VFS calls this with a live, open file.
    let h = unsafe { (*file).private_data }.cast::<ReaderHandle>();
    // SAFETY: the handle `add` gave this file, live while the file is open.
    let h = unsafe { &*h };
    if drop_registration(h) { 0 } else { ENOENT.to_errno() as c_long }
}

/// The consumer's doorbell: the eventfd it sleeps on, and its page, mapped read-only. Set once.
pub(crate) struct Bell {
    efd: AtomicPtr<bindings::eventfd_ctx>,
    /// The page, kept referenced for the stream's life.
    page: AtomicPtr<bindings::page>,
    /// vmap of the page, read-only; set last, with release. Null: no bell.
    map: AtomicPtr<u64>,
    /// The state the consumer was last rung for.
    rung: AtomicU64,
}

impl Bell {
    pub(crate) const fn new() -> Self {
        Self {
            efd: AtomicPtr::new(ptr::null_mut()),
            page: AtomicPtr::new(ptr::null_mut()),
            map: AtomicPtr::new(ptr::null_mut()),
            rung: AtomicU64::new(0),
        }
    }

    /// Rings the consumer's bell once for a sleep it has gone to, if it is asleep now. The full
    /// barrier orders the publish (the head store, before the caller's wake) before the read of the
    /// page; the consumer orders its arm before its look (protocol.rs), so either the consumer sees
    /// the frame or this sees it asleep. Never under the stream's lock.
    pub(crate) fn ring(&self) {
        let map = self.map.load(Ordering::Acquire);
        if map.is_null() {
            return;
        }
        smp_mb();
        // SAFETY: the bell's page stays mapped, read-only, until the stream is freed; word 0 is the
        // consumer's, so it may change at any moment.
        let state = unsafe { ptr::read_volatile(map) };
        smp_rmb();
        if state & 1 == 0 || self.rung.swap(state, Ordering::SeqCst) == state {
            return;
        }
        // SAFETY: the eventfd was pinned before the mapping was published, and is held until the
        // stream is freed.
        unsafe { bindings::eventfd_signal_mask(self.efd.load(Ordering::Relaxed), 0) };
    }

    /// LEMNOS_STREAM_IOC_BELL: the consumer's doorbell, once per stream. The eventfd and the page are
    /// both pinned here; the page is mapped read-only and never written. The control mutex is held.
    /// The eventfd is set before the mapping is published (`ring` reads the mapping first, with acquire).
    pub(crate) fn set(&self, a: &lb::lemnos_stream_bell) -> Result {
        if !self.map.load(Ordering::Relaxed).is_null() {
            return Err(EBUSY);
        }
        let efd = EventFd::get(a.eventfd)?;
        let file = FileRef::get(a.page)?;
        if !file.is_shmem() || file.size() < 8 {
            return Err(EINVAL);
        }
        let mut page = file.page(0)?;
        // SAFETY: one page, referenced until the stream is freed; mapped read-only.
        let map = unsafe { lemnos_vmap_ro(&raw mut page, 1) };
        if map.is_null() {
            // SAFETY: the reference file.page() gave, dropped once.
            unsafe { lemnos_put_page(page) };
            return Err(ENOMEM);
        }
        self.efd.store(efd.into_raw(), Ordering::Relaxed);
        self.page.store(page, Ordering::Relaxed);
        self.map.store(map.cast(), Ordering::Release);
        Ok(())
    }
}

impl Drop for Bell {
    fn drop(&mut self) {
        let (map, page, efd) = (*self.map.get_mut(), *self.page.get_mut(), *self.efd.get_mut());
        if !map.is_null() {
            // SAFETY: the mapping `set` made, unmapped once, with the stream.
            unsafe { lb::vunmap(map.cast()) };
        }
        if !page.is_null() {
            // SAFETY: the reference `set` kept, dropped once.
            unsafe { lemnos_put_page(page) };
        }
        if !efd.is_null() {
            // SAFETY: the reference `set` kept, dropped once.
            unsafe { bindings::eventfd_ctx_put(efd) };
        }
    }
}

/// An eventfd context from eventfd_ctx_fdget(), put when dropped.
pub(crate) struct EventFd(*mut bindings::eventfd_ctx);

impl EventFd {
    /// eventfd_ctx_fdget(fd): the context, or its errno.
    pub(crate) fn get(fd: c_int) -> Result<Self> {
        // SAFETY: takes a reference on the eventfd `fd` names, or answers an error pointer.
        Ok(Self(from_err_ptr(unsafe { bindings::eventfd_ctx_fdget(fd) })?))
    }

    /// The context, still this guard's.
    pub(crate) fn as_raw(&self) -> *mut bindings::eventfd_ctx {
        self.0
    }

    /// The context, kept: the caller puts it.
    pub(crate) fn into_raw(self) -> *mut bindings::eventfd_ctx {
        let efd = self.0;
        core::mem::forget(self);
        efd
    }
}

impl Drop for EventFd {
    fn drop(&mut self) {
        // SAFETY: the reference eventfd_ctx_fdget() took, dropped once.
        unsafe { bindings::eventfd_ctx_put(self.0) };
    }
}
