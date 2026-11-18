// SPDX-License-Identifier: GPL-2.0
//! dma-bufs on the block (the stream port): the pool, the regions a consumer registers, a buffer's own
//! memory, and the lend of another block's output buffer as this block's input.
//!
//! A region is a dma-buf attached to the block and mapped for it: one contiguous range of device
//! addresses, or it is refused. It stays pinned while the stream holds it, and leaves a stream only
//! when no job uses it (`users`). A region overlapping another would be another path past the checks,
//! so one is refused.
//!
//! The lend (LEMNOS_STREAM_IOC_REGISTER_LENT) pins a dma-buf the block only reads: the output buffer
//! of another block's stream, with no copy. Unregistering it keeps the pin in a small cache, keyed by
//! the dma-buf and the generation of its memory, so the next lend of the same buffer attaches and
//! maps nothing. A pin leaves the cache once: taken back by a lend, pushed out by a newer pin, found
//! stale (another generation), or unmapped with the stream.
//!
//! Mapping and unmapping sleep, so they happen under the stream's control mutex and never under its
//! lock; the table itself changes only under both.

use crate::bindings as lb;
use crate::stream_frame::{DMA_ALIGN, POOL, REGIONS, Region};
use crate::stream_producer::St;
use crate::stream_ring::{BUF_ABSENT, BUF_ACTIVE, BUF_FREE, BUF_QUEUED, Frame, Geom};
use crate::stream::Stream;
use core::ffi::{c_int, c_long};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr, to_result},
    prelude::*,
};

/// What the stream's control mutex guards beside the order of registration and teardown.
pub(crate) type Ctl = lb::lem_ctl;

/// Unregistered lent regions kept pinned for the next register of the same dma-buf.
pub(crate) const PINS: usize = lb::LEM_STREAM_PINS as usize;

unsafe extern "C" {
    /// lemnos_stream.c: the test_stream_source parameter, until the file's port takes it.
    #[link_name = "test_stream_source"]
    static TEST_STREAM_SOURCE: bool;
}

/// Whether the test source is allowed, and with it the kernel mapping of every region it fills.
fn test_stream_source() -> bool {
    // SAFETY: a 0444 parameter: written by the kernel's parser before init, only read after.
    unsafe { TEST_STREAM_SOURCE }
}

impl Region {
    /// Unmaps and unpins whatever part of the region was made, and leaves it empty. Sleeps.
    pub(crate) fn unmap(&mut self) {
        if self.buf.is_null() {
            return;
        }
        if !self.vaddr().is_null() {
            // SAFETY: the mapping dma_buf_vmap_unlocked made of this dma-buf, unmapped once.
            unsafe { bindings::dma_buf_vunmap_unlocked(self.buf, &raw mut self.map) };
        }
        if !self.sgt.is_null() {
            // SAFETY: the table this attachment was mapped to, for the same direction, unmapped once.
            unsafe { bindings::dma_buf_unmap_attachment_unlocked(self.att, self.sgt, self.dir) };
        }
        if !self.att.is_null() {
            // SAFETY: the attachment dma_buf_attach made on this dma-buf, detached once.
            unsafe { bindings::dma_buf_detach(self.buf, self.att) };
        }
        // SAFETY: the reference the region was given, dropped once.
        unsafe { bindings::dma_buf_put(self.buf) };
        *self = Region::default();
    }

    /// Maps `buf`, a reference the caller gives, to the block for `dir`: one contiguous range in its
    /// address space, or refused. Either way the reference is the region's: it is dropped by `unmap`,
    /// here when the mapping is refused.
    pub(crate) fn attach(dev: *mut bindings::device, buf: *mut bindings::dma_buf, dir: bindings::dma_data_direction) -> Result<Region> {
        let mut r = Region { buf, dir, ..Region::default() };
        match r.map_for(dev) {
            Ok(()) => Ok(r),
            Err(e) => {
                r.unmap();
                Err(e)
            }
        }
    }

    fn map_for(&mut self, dev: *mut bindings::device) -> Result {
        // SAFETY: `buf` is a dma-buf this region holds a reference on; `dev` is the stream's block.
        self.att = from_err_ptr(unsafe { bindings::dma_buf_attach(self.buf, dev) })?;
        // SAFETY: the attachment just made, mapped once for the region's direction.
        self.sgt = from_err_ptr(unsafe { bindings::dma_buf_map_attachment_unlocked(self.att, self.dir) })?;
        // SAFETY: the table the exporter returned, live until it is unmapped.
        let (mut sg, nents) = unsafe { ((*self.sgt).sgl, (*self.sgt).nents) };
        if nents == 0 {
            return Err(EINVAL);
        }
        // SAFETY: the table has at least one mapped entry (checked above).
        let mut next = unsafe { bindings::sg_dma_address(sg) };
        self.addr = next;
        let mut size = 0u64;
        for _ in 0..nents {
            // SAFETY: `sg` is one of the table's `nents` mapped entries.
            let (addr, len) = unsafe { (bindings::sg_dma_address(sg), bindings::sg_dma_len(sg)) };
            if addr != next {
                return Err(EINVAL);
            }
            next = next.checked_add(u64::from(len)).ok_or(EINVAL)?;
            size += u64::from(len);
            // SAFETY: as above; the entry after the last is not read.
            sg = unsafe { bindings::sg_next(sg) };
        }
        if size == 0 {
            return Err(EINVAL);
        }
        self.end = self.addr.checked_add(size).ok_or(EINVAL)?;
        self.size = size;
        if test_stream_source() {
            self.vmap()?;
        }
        Ok(())
    }

    /// Pins `fd`, mapped to the block: one contiguous range in its address space, or refused.
    pub(crate) fn map(dev: *mut bindings::device, fd: c_int) -> Result<Region> {
        // SAFETY: takes a reference on the dma-buf `fd` names, or answers an error pointer.
        let buf = from_err_ptr(unsafe { bindings::dma_buf_get(fd) })?;
        Region::attach(dev, buf, bindings::dma_data_direction::DMA_BIDIRECTIONAL)
    }

    /// A region's kernel mapping, for `lem_stream_range_read()` and the test source. Not for iomem.
    pub(crate) fn vmap(&mut self) -> Result {
        // SAFETY: a dma-buf this region holds; the map is the region's own, cleared on failure.
        to_result(unsafe { bindings::dma_buf_vmap_unlocked(self.buf, &raw mut self.map) })?;
        if self.map.is_iomem {
            // SAFETY: the mapping just made, unmapped once.
            unsafe { bindings::dma_buf_vunmap_unlocked(self.buf, &raw mut self.map) };
            self.map = bindings::iosys_map::default();
            return Err(EINVAL);
        }
        Ok(())
    }

    /// With `cpu_read`, the kernel mapping a registered region keeps; a refusal unmaps the region.
    fn readable(mut self, cpu_read: bool) -> Result<Region> {
        if cpu_read && self.vaddr().is_null() {
            if let Err(e) = self.vmap() {
                self.unmap();
                return Err(e);
            }
        }
        Ok(self)
    }
}

/// Whether `r` is, or overlaps, memory the stream already has, other than buffer `except`'s own: a
/// region overlapping another would be another path past the checks. Lock held.
fn taken(g: &Geom, f: &mut Frame, r: &Region, except: Option<u32>) -> bool {
    let clash = |other: &Region| !other.buf.is_null() && (other.buf == r.buf || (r.addr < other.end && other.addr < r.end));
    if f.region.iter().any(clash) {
        return true;
    }
    f.own_mode() && (0..g.buffers).any(|b| Some(b) != except && clash(f.own_at(b)))
}

/// Puts a mapped region in a free slot of the table, in one step under the lock; a region that is
/// taken (EBUSY) or has no slot (ENOSPC) is unmapped.
fn insert(st: St<'_>, mut r: Region) -> Result<u32> {
    let slot = {
        let mut f = st.lock();
        if taken(st.geom(), &mut f, &r, None) {
            Err(EBUSY)
        } else {
            match (POOL + 1..REGIONS).find(|&i| f.region[i].buf.is_null()) {
                Some(i) => {
                    f.region[i] = r;
                    Ok(i as u32)
                }
                None => Err(ENOSPC),
            }
        }
    };
    if slot.is_err() {
        r.unmap();
    }
    slot
}

/// LEMNOS_STREAM_IOC_REGISTER: pins a dma-buf the block may be sent to. The control mutex is held.
pub(crate) fn register(st: St<'_>, c: &mut Ctl, a: &mut lb::lemnos_stream_register) -> Result {
    let r = Region::map(st.dev(), a.fd)?.readable(c.cpu_read)?;
    a.region = insert(st, r)?;
    a.addr = r.addr;
    a.size = r.size;
    Ok(())
}

/// Keeps an unregistered lent region pinned in the pin cache. A full cache gives up its next pin,
/// which is unmapped. The control mutex is held.
fn pin_keep(c: &mut Ctl, r: Region) {
    let at = match c.pins.iter().position(|p| p.buf.is_null()) {
        Some(free) => free,
        None => {
            let next = c.pin_next as usize % PINS;
            c.pin_next = ((next + 1) % PINS) as u32;
            next
        }
    };
    core::mem::replace(&mut c.pins[at], r).unmap();
}

/// LEMNOS_STREAM_IOC_REGISTER_LENT: pins a dma-buf the block only reads, and gives its range. A pin
/// kept for this dma-buf and generation is taken back as it is: its reference is the cache's, so the
/// one this call got is dropped and nothing is attached or mapped again. Pins of another generation
/// are no longer this stream's memory and are unmapped first. The control mutex is held.
pub(crate) fn register_lent(st: St<'_>, c: &mut Ctl, a: &mut lb::lemnos_stream_register_lent) -> Result {
    // SAFETY: takes a reference on the dma-buf `fd` names, or answers an error pointer.
    let buf = from_err_ptr(unsafe { bindings::dma_buf_get(a.fd) })?;
    for pin in c.pins.iter_mut().filter(|p| !p.buf.is_null() && p.gen_ != a.generation) {
        core::mem::take(pin).unmap();
    }
    let r = match c.pins.iter().rposition(|p| p.buf == buf && p.gen_ == a.generation) {
        Some(hit) => {
            // SAFETY: the reference dma_buf_get took above; the pin keeps the one it was mapped with.
            unsafe { bindings::dma_buf_put(buf) };
            core::mem::take(&mut c.pins[hit])
        }
        None => {
            let mut r = Region::attach(st.dev(), buf, bindings::dma_data_direction::DMA_TO_DEVICE)?;
            r.lent = true;
            r.gen_ = a.generation;
            r
        }
    };
    let r = r.readable(c.cpu_read)?;
    a.region = insert(st, r)?;
    a.addr = r.addr;
    a.size = r.size;
    Ok(())
}

/// LEMNOS_STREAM_IOC_UNREGISTER: releases a region; refused (EBUSY) while a job names it. A lent
/// region is kept pinned for its generation; the rest is unmapped now. The control mutex is held.
pub(crate) fn unregister(st: St<'_>, c: &mut Ctl, region: u32) -> Result {
    let region = region as usize;
    if region == POOL || region >= REGIONS {
        return Err(EINVAL);
    }
    let (mut r, dead) = {
        let mut f = st.lock();
        let reg = &mut f.region[region];
        if reg.buf.is_null() {
            return Err(ENOENT);
        }
        if reg.users != 0 {
            return Err(EBUSY);
        }
        (core::mem::take(reg), f.dead)
    };
    if r.lent && !dead {
        pin_keep(c, r);
    } else {
        r.unmap();
    }
    Ok(())
}

/// LEMNOS_STREAM_IOC_BUFFER: gives buffer `a.buffer` its memory, or swaps it. The new dma-buf is
/// pinned and checked first; it takes the buffer's place in one step under the lock, only while
/// nothing can be writing the buffer; the old one is unpinned after. The control mutex is held.
pub(crate) fn set_buffer(st: St<'_>, a: &mut lb::lemnos_stream_buffer) -> Result {
    let g = st.geom();
    if !st.lock().own_mode() {
        return Err(EOPNOTSUPP);
    }
    if a.buffer >= g.buffers {
        return Err(EINVAL);
    }
    let mut r = Region::map(st.dev(), a.fd)?;
    // Only what the buffer uses is reachable, however big the dma-buf.
    if r.size < u64::from(g.buffer_bytes) || r.addr & (DMA_ALIGN - 1) != 0 {
        r.unmap();
        return Err(EINVAL);
    }
    r.size = u64::from(g.buffer_bytes);
    r.end = r.addr + r.size;

    // What is unpinned after the lock: the new pin when it is refused or is what the buffer already
    // has, the old one when it was replaced.
    let (mut out, ret) = {
        let mut f = st.lock();
        let state = f.buf(a.buffer).state;
        let at = *f.own_at(a.buffer);
        if f.wedged {
            (r, Err(EIO))
        } else if at.buf == r.buf {
            // What it already has: nothing changes, and the new pin is dropped.
            a.addr = at.addr;
            a.size = at.size;
            (r, Ok(()))
        } else if state == BUF_QUEUED || state == BUF_ACTIVE || at.users != 0 || taken(g, &mut f, &r, Some(a.buffer)) {
            (r, Err(EBUSY))
        } else {
            *f.own_at(a.buffer) = r;
            if state == BUF_ABSENT {
                f.buf(a.buffer).state = BUF_FREE;
            }
            a.addr = r.addr;
            a.size = r.size;
            (at, Ok(()))
        }
    };
    out.unmap();
    ret
}

/// Registered regions keep a kernel mapping from here on, so a block can read through
/// `lem_stream_range_read()`. The control mutex is held, so never under the block's locks.
pub(crate) fn cpu_read_enable(st: St<'_>, c: &mut Ctl) -> Result {
    if st.lock().dead {
        return Err(ENODEV);
    }
    c.cpu_read = true;
    for i in POOL + 1..REGIONS {
        // The table changes only under the control mutex, so the region read here is still slot i's
        // when its mapping is written back; mapping sleeps, so it is done outside the lock.
        let mut r = st.lock().region[i];
        if r.buf.is_null() || !r.vaddr().is_null() {
            continue;
        }
        r.vmap()?;
        st.lock().region[i].map = r.map;
    }
    Ok(())
}

// What lemnos_stream.c still calls until its file is ported: the C names and signatures it had
// (lemnos_stream_priv.h). Each sleeps, so none is called under the stream's lock.

fn errno(r: Result) -> c_long {
    r.err().map_or(0, |e| c_long::from(e.to_errno()))
}

/// # Safety
/// `r` is a region no one else can reach (taken out of its table, or not yet in one).
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_region_unmap(r: *mut Region) {
    // SAFETY: the caller's contract.
    unsafe { &mut *r }.unmap();
}

/// # Safety
/// `dev` is the stream's block; `r` is the caller's to write, and is left empty on a refusal.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_region_map(dev: *mut bindings::device, fd: c_int, r: *mut Region) -> c_int {
    let (region, ret) = match Region::map(dev, fd) {
        Ok(region) => (region, 0),
        Err(e) => (Region::default(), e.to_errno()),
    };
    // SAFETY: the caller's contract: `r` is valid for a write.
    unsafe { r.write(region) };
    ret
}

/// # Safety
/// `s` is a live stream whose control mutex the caller holds; `a` is the caller's copy of the request.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_register(s: *mut Stream, a: *mut lb::lemnos_stream_register) -> c_long {
    // SAFETY: the caller's contract; the mutex makes this the only reference to what it guards.
    errno(register(unsafe { St::new(s) }, unsafe { &mut (*s).c }, unsafe { &mut *a }))
}

/// # Safety
/// As `lem_stream_register`.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_register_lent(s: *mut Stream, a: *mut lb::lemnos_stream_register_lent) -> c_long {
    // SAFETY: the caller's contract; the mutex makes this the only reference to what it guards.
    errno(register_lent(unsafe { St::new(s) }, unsafe { &mut (*s).c }, unsafe { &mut *a }))
}

/// # Safety
/// `s` is a live stream whose control mutex the caller holds.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_unregister(s: *mut Stream, region: u32) -> c_long {
    // SAFETY: the caller's contract; the mutex makes this the only reference to what it guards.
    errno(unregister(unsafe { St::new(s) }, unsafe { &mut (*s).c }, region))
}

/// # Safety
/// As `lem_stream_register`.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_set_buffer(s: *mut Stream, a: *mut lb::lemnos_stream_buffer) -> c_long {
    // SAFETY: the caller's contract.
    errno(set_buffer(unsafe { St::new(s) }, unsafe { &mut *a }))
}

/// Registered regions keep a kernel mapping from here on, so a block can read through
/// `lem_stream_range_read()`. Takes the control mutex, so never under the block's locks.
///
/// # Safety
/// `s` is a live stream.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_cpu_read_enable(s: *mut Stream) -> c_int {
    // SAFETY: the caller's contract: the stream's own mutex, initialised at creation.
    unsafe { bindings::mutex_lock(&raw mut (*s).ctl) };
    // SAFETY: the caller's contract; the mutex, held, makes this the only reference to what it guards.
    let ret = cpu_read_enable(unsafe { St::new(s) }, unsafe { &mut (*s).c });
    // SAFETY: the mutex taken above.
    unsafe { bindings::mutex_unlock(&raw mut (*s).ctl) };
    ret.err().map_or(0, |e| e.to_errno())
}
