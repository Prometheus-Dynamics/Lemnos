// SPDX-License-Identifier: GPL-2.0
//! A block as the producer of a stream (the stream port): the calls a block makes on its
//! stream, by the stream's pointer or through rs/stream.rs's `Handle`.
//!
//! A block attaches as the stream's producer and calls `lem_stream_next()` for the buffer it arms and
//! `lem_stream_complete()` when the block is finished with it. Everything those touch is the kernel's
//! own copy; the block only ever sees device addresses computed here from the pool, or from the
//! buffer's own dma-buf.
//!
//! Locking: the stream's lock is the frame path's, taken here by the block (its interrupt handler) and
//! held for memory reads and writes only. The consumer is woken after it is dropped: the eventfd has a
//! lock of its own. Nothing here allocates.

use crate::bindings as lb;
use crate::stream::{JOB_HOLDS, Job, JobRange, Producer, Slot, Stream};
use crate::stream_region::Region;
use crate::stream_frame::{
    POOL, REGIONS, buffer_addr, buffer_free, check_frame, drain, finish_locked, now_ns,
    resolve, take, unhold,
};
use crate::stream_ring::{BUF_ACTIVE, BUF_FREE, Frame, enforce, fault_locked, reclaim};
use core::{
    ffi::{c_int, c_void},
    ptr,
};
use kernel::{error::code::*, prelude::*};

/// A live stream, as the calls below are given it.
pub(crate) type St<'a> = &'a Stream;

/// The errno of a stream that is over: the block went or the file closed, or what ended it.
pub(crate) fn over(f: &Frame) -> Option<c_int> {
    if f.dead {
        Some(ENODEV.to_errno())
    } else if f.fault != 0 {
        Some(f.fault)
    } else {
        None
    }
}

/// One producer per stream. `bytes` is what it may write per buffer. The stream knows bytes only: a
/// producer whose frames are lines or planes checks that they fit in `bytes` itself, before attaching.
///
/// # Safety
/// `s` is a live stream; `producer` is valid for the call.
pub(crate) unsafe extern "C" fn lem_stream_attach(s: *mut Stream, producer: *const Producer, bytes: u32) -> c_int {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let producer = unsafe { &*producer };
    let (g, mut f) = (st.geom(), st.lock());
    if let Some(e) = over(&f) {
        return e;
    }
    if f.producer.stop.is_some() || f.left != 0 {
        return EBUSY.to_errno();
    }
    if producer.job_bytes != 0 && producer.job_bytes != g.job_bytes {
        return EINVAL.to_errno();
    }
    if let Err(e) = check_frame(g, &f, bytes) {
        return e.to_errno();
    }
    f.producer = *producer;
    f.frame_bytes = bytes;
    0
}

/// Removes the producer; true when it was there and this call took it off.
///
/// # Safety
/// `s` is a live stream.
pub(crate) unsafe extern "C" fn lem_stream_detach(s: *mut Stream) -> bool {
    // SAFETY: the caller's contract.
    let mut f = unsafe { &*s }.lock();
    f.producer.stop.take().is_some()
}

/// The next frame's buffer, any context: 0, or -ENOBUFS when none is free.
///
/// # Safety
/// `s` is a live stream; `slot` is the caller's to write.
pub(crate) unsafe extern "C" fn lem_stream_next(s: *mut Stream, slot: *mut Slot) -> c_int {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let slot = unsafe { &mut *slot };
    let g = st.geom();
    let mut next = |f: &mut Frame| -> Result<(), c_int> {
        if let Some(e) = over(f) {
            return Err(e);
        }
        drain(g, f).map_err(|e| e.to_errno())?;
        slot.job = take(g, f).ok_or(ENOBUFS.to_errno())?;
        match buffer_addr(g, f, slot.job.buffer, u64::from(f.frame_bytes)) {
            Ok(addr) => {
                slot.addr = addr;
                slot.bytes = f.frame_bytes;
                Ok(())
            }
            Err(_) => {
                // The bounds were checked when the memory came; never hand this out.
                finish_locked(g, f, &slot.job, 0, 0, 0, 0);
                Err(EFAULT.to_errno())
            }
        }
    };
    // The lock is dropped at the end of this statement.
    let ret = next(&mut st.lock()).err().unwrap_or(0);
    if ret != 0 && ret != ENOBUFS.to_errno() && ret != ENODEV.to_errno() {
        st.wake();
    }
    ret
}

/// Done with a slot: publish `bytes` of it, or 0 to say the frame went wrong.
///
/// # Safety
/// `s` is a live stream; `slot` is one it gave and has not had back.
pub(crate) unsafe extern "C" fn lem_stream_complete(s: *mut Stream, slot: *const Slot, bytes: u32, sequence: u64, timestamp_ns: u64) {
    // SAFETY: the caller's contract.
    unsafe { lem_stream_complete_at(s, slot, bytes, sequence, timestamp_ns, 0) };
}

/// As `lem_stream_complete`, with the frame's end time (0 when the block stamps none).
///
/// # Safety
/// `s` is a live stream; `slot` is one it gave and has not had back.
pub(crate) unsafe extern "C" fn lem_stream_complete_at(
    s: *mut Stream,
    slot: *const Slot,
    bytes: u32,
    sequence: u64,
    timestamp_ns: u64,
    end_ns: u64,
) {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let slot = unsafe { &*slot };
    let g = st.geom();
    if slot.job.buffer >= g.buffers {
        return;
    }
    {
        let mut f = st.lock();
        // Never more than this buffer was given for, whatever the block says.
        finish_locked(g, &mut f, &slot.job, bytes.min(slot.bytes), sequence, timestamp_ns, end_ns);
    }
    st.wake();
}

/// The frame never ran. Returns its buffer and what its job held, without publishing.
///
/// # Safety
/// `s` is a live stream; `slot` is one it gave and has not had back.
pub(crate) unsafe extern "C" fn lem_stream_cancel(s: *mut Stream, slot: *const Slot) {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let slot = unsafe { &*slot };
    if slot.job.buffer >= st.geom().buffers {
        return;
    }
    let mut f = st.lock();
    unhold(&mut f, &slot.job);
    f.buf(slot.job.buffer).state = BUF_FREE;
}

/// The registered region a range names, or None for the pool, a buffer's own dma-buf, an empty slot
/// or a range that is not inside it.
fn range_region<'a>(f: &'a Frame, r: &JobRange) -> Option<&'a Region> {
    if r.region as usize == POOL || r.region as usize >= REGIONS {
        return None;
    }
    let reg = &f.region[r.region as usize];
    if reg.buf.is_null() || reg.users == 0 {
        return None;
    }
    match r.offset.checked_add(r.len) {
        Some(end) if end <= reg.size => Some(reg),
        _ => None,
    }
}

/// Reads a job's range through the kernel's mapping of its registered region, any context, exactly
/// `bytes`. The pool and a buffer's own dma-buf aren't readable here. Needs a stream that has had
/// `lem_stream_cpu_read_enable()` and a job still held.
///
/// # Safety
/// `s` is a live stream; `r` is valid for the call; `dst` is valid for `bytes` bytes.
pub(crate) unsafe extern "C" fn lem_stream_range_read(s: *mut Stream, r: *const JobRange, dst: *mut c_void, bytes: usize) -> c_int {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let r = unsafe { &*r };
    let f = st.lock();
    let Some(reg) = range_region(&f, r).filter(|reg| !reg.vaddr().is_null()) else {
        return EFAULT.to_errno();
    };
    if r.len != bytes as u64 {
        return EINVAL.to_errno();
    }
    // SAFETY: the range is inside the region's mapping (range_region), which stays while the job holds
    // the region and the lock is held; `dst` is the caller's, for `bytes`.
    unsafe { ptr::copy_nonoverlapping(reg.vaddr().wrapping_add(r.offset as usize), dst.cast::<u8>(), bytes) };
    0
}

/// A held job's range as a device address, computed here. The pool and a buffer's own dma-buf are refused.
///
/// # Safety
/// `s` is a live stream; `r` is valid for the call; `addr` is the caller's to write.
pub(crate) unsafe extern "C" fn lem_stream_range_addr(s: *mut Stream, r: *const JobRange, addr: *mut u64) -> c_int {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let r = unsafe { &*r };
    let f = st.lock();
    match range_region(&f, r).and_then(|reg| reg.addr.checked_add(r.offset)) {
        Some(a) => {
            // SAFETY: the caller's contract: `addr` is valid for a write.
            unsafe { addr.write(a) };
            0
        }
        None => EACCES.to_errno(),
    }
}

/// Test only: writes a word through a held range's kernel mapping, for the module's checks.
///
/// # Safety
/// `s` is a live stream; `r` is valid for the call.
pub(crate) unsafe extern "C" fn lem_stream_range_poke(s: *mut Stream, r: *const JobRange, offset: usize, val: u32) {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let r = unsafe { &*r };
    let f = st.lock();
    let Some(reg) = range_region(&f, r).filter(|reg| !reg.vaddr().is_null()) else {
        return;
    };
    if (offset as u64).saturating_add(4) > r.len {
        return;
    }
    // SAFETY: the four bytes are inside the range (checked above), which is inside the region's mapping.
    unsafe { reg.vaddr().wrapping_add(r.offset as usize + offset).cast::<u32>().write_unaligned(val) };
}

/// The block's DMA can't be shown to have stopped: keep everything pinned for good.
///
/// # Safety
/// `s` is a live stream.
pub(crate) unsafe extern "C" fn lem_stream_wedge(s: *mut Stream) {
    // SAFETY: the caller's contract.
    unsafe { &*s }.lock().wedged = true;
}

/// For a producer with job_bytes set. Reads the job at the front of the ring into `job` (job_bytes),
/// once: 1 with a job, 0 with none, -errno for a lying consumer. Doesn't take it: the block copies and
/// checks what it names first.
///
/// # Safety
/// `s` is a live stream; `job` is valid for `bytes` bytes.
pub(crate) unsafe extern "C" fn lem_stream_job_peek(s: *mut Stream, job: *mut c_void, bytes: usize) -> c_int {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    let (g, f) = (st.geom(), st.lock());
    let slots = u64::from(g.job_slots);
    if bytes != f.producer.job_bytes as usize {
        return EINVAL.to_errno();
    }
    if let Some(e) = over(&f) {
        return e;
    }
    let count = g.read_acquire(g.w_jobs_head).wrapping_sub(f.jobs_tail);
    if count == 0 {
        return 0;
    }
    if count > slots {
        return ERANGE.to_errno();
    }
    let at = g.jobs_at + (f.jobs_tail & (slots - 1)) as usize * g.job_words;
    if g.read(at) != bytes as u64 {
        return EMSGSIZE.to_errno();
    }
    for i in 0..bytes / 8 {
        // SAFETY: the caller's contract: word `i` of `job` is inside its `bytes`.
        unsafe { job.cast::<u64>().wrapping_add(i).write_unaligned(g.read(at + 1 + i)) };
    }
    1
}

/// The job at the front of the ring passed the block's checks. Resolves every range against the job's
/// buffer and the registered regions and, only if all resolve and the buffer is free, takes the job
/// and holds them, in one step: nothing can be unregistered between the check and the hold. `slot` is
/// then as from `lem_stream_next()` and ends with `lem_stream_complete()` or `_cancel()`.
///
/// # Safety
/// `s` is a live stream; `range` holds `count` entries; `slot` is the caller's to write.
pub(crate) unsafe extern "C" fn lem_stream_job_take(
    s: *mut Stream,
    buffer: u32,
    identity: u64,
    range: *const lb::lemnos_stream_range,
    count: u32,
    slot: *mut Slot,
) -> c_int {
    if count as usize > JOB_HOLDS {
        return EINVAL.to_errno();
    }
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // SAFETY: the caller's contract.
    let slot = unsafe { &mut *slot };
    let g = st.geom();
    // SAFETY: the caller's contract: `count` entries, or none (a null `range` is never read).
    let ranges = if count == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(range, count as usize) } };
    slot.job = Job::default();
    let mut job_take = |f: &mut Frame| -> Result<(), c_int> {
        if let Some(e) = over(f) {
            return Err(e);
        }
        if buffer >= g.buffers {
            return Err(EINVAL.to_errno());
        }
        enforce(g, f, now_ns());
        reclaim(g, f, buffer);
        for (out, r) in slot.job.range.iter_mut().zip(ranges) {
            *out = resolve(g, f, buffer, r.addr, r.len).map_err(|e| e.to_errno())?;
        }
        buffer_free(f, buffer).map_err(|e| e.to_errno())?;
        let addr = buffer_addr(g, f, buffer, u64::from(f.frame_bytes)).map_err(|_| EFAULT.to_errno())?;
        for r in &slot.job.range[..count as usize] {
            f.region_at(r.region).users += 1;
        }
        slot.job.identity = identity;
        slot.job.buffer = buffer;
        slot.job.count = count;
        slot.addr = addr;
        slot.bytes = f.frame_bytes;
        f.buf(buffer).state = BUF_ACTIVE;
        f.jobs += 1;
        f.jobs_tail += 1;
        g.write_release(g.w_jobs_tail, f.jobs_tail);
        Ok(())
    };
    // The lock is dropped at the end of this statement.
    job_take(&mut st.lock()).err().unwrap_or(0)
}

/// The block found it can't go on: the consumer learns with `err` and the stream ends.
///
/// # Safety
/// `s` is a live stream.
pub(crate) unsafe extern "C" fn lem_stream_fault(s: *mut Stream, err: c_int) {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    // The lock is dropped at the end of this statement.
    fault_locked(st.geom(), &mut st.lock(), err);
    st.wake();
}
