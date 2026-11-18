// SPDX-License-Identifier: GPL-2.0
//! The frame path (the stream port): jobs in, buffers out, frames published.
//!
//! A stream is jobless until the first job is accepted: frames take free buffers. From then on it is
//! job-driven for good: a frame needs a queued job, and nothing else takes a buffer, so a consumer can
//! let go of a buffer's marks and post a job on it without losing it in between. The window before
//! that first job is the consumer's to close, by posting it before frames start (lemnos.h).
//!
//! Only a block's own memory is ever named: the pool and dma-bufs registered on the stream are pinned
//! while the stream holds them, and removed from a stream only when no job uses them. A buffer's
//! device address is computed in one place, `buffer_addr`, for either kind of stream.
//!
//! Every function here runs under the stream's lock and allocates nothing.

use crate::bindings as lb;
use crate::stream::{Job, JobRange, ktime_now};
use crate::stream_ring::{
    BUF_ABSENT, BUF_ACTIVE, BUF_CONSUMER, BUF_FREE, BUF_QUEUED, Buf, Frame, Geom, enforce,
    fault_locked, newest_read, publish, reclaim,
};
use kernel::{error::code::*, prelude::*};

/// A dma-buf pinned and mapped for the block: the pool, a registered region, or a buffer's own memory.
pub(crate) type Region = lb::lem_stream_region;

/// The pool, then the dma-bufs the consumer registers. A range's region at or past REGIONS is a
/// buffer's own dma-buf: `own[region - REGIONS]`.
pub(crate) const REGIONS: usize = 16;
pub(crate) const POOL: usize = 0;
/// A device address a block is programmed with is aligned this much (LEM_STREAM_DMA_ALIGN).
pub(crate) const DMA_ALIGN: u64 = 16;
/// Words a job occupies: its bytes, and nothing about what it means.
const JOB_WORDS: usize = core::mem::size_of::<lb::lemnos_stream_job>() / 8;
const JOB_RANGES: usize = lb::LEMNOS_STREAM_JOB_RANGES as usize;

const _: () = assert!(core::mem::size_of::<lb::lemnos_stream_job>() % 8 == 0);
const _: () = assert!(REGIONS == lb::LEM_STREAM_REGIONS as usize);

/// The monotonic clock, in ns (ktime_get_ns).
pub(crate) fn now_ns() -> u64 {
    ktime_now() as u64
}

impl Frame {
    /// The state of `buffer`, which is below the stream's buffers.
    pub(crate) fn buf(&mut self, buffer: u32) -> &mut Buf {
        // SAFETY: `buf` holds one entry per buffer for the stream's life, every caller has checked
        // `buffer` against the stream's buffers, and the lock makes this the only reference.
        unsafe { &mut *self.buf.wrapping_add(buffer as usize) }
    }

    /// Whether each buffer has a dma-buf of its own (a stream made without a pool).
    pub(crate) fn own_mode(&self) -> bool {
        !self.own.is_null()
    }

    /// The dma-buf of `buffer` on a stream without a pool; `buffer` is below the stream's buffers.
    pub(crate) fn own_at(&mut self, buffer: u32) -> &mut Region {
        // SAFETY: on such a stream `own` holds one region per buffer for the stream's life, the
        // callers check `own_mode` and `buffer`, and the lock makes this the only reference.
        unsafe { &mut *self.own.wrapping_add(buffer as usize) }
    }

    /// The region a resolved range names: the table, or a buffer's own dma-buf.
    pub(crate) fn region_at(&mut self, region: u32) -> &mut Region {
        if (region as usize) < REGIONS {
            &mut self.region[region as usize]
        } else {
            self.own_at(region - REGIONS as u32)
        }
    }

    /// Slot `at` of the job queue, which has `job_slots` (a power of two) entries.
    fn queue_slot(&mut self, at: u64, job_slots: u32) -> &mut Job {
        let i = (at & (u64::from(job_slots) - 1)) as usize;
        // SAFETY: `queue` holds `job_slots` jobs for the stream's life, `i` is below that, and the
        // lock makes this the only reference. `struct lem_job` is `Job` (rs/stream.rs).
        unsafe { &mut *self.queue.wrapping_add(i) }
    }
}

/// Where `buffer` is for the block, and that `bytes` from there are inside memory this stream has
/// pinned: its slice of the pool, or its own dma-buf. The only place a buffer's address comes from.
pub(crate) fn buffer_addr(g: &Geom, f: &mut Frame, buffer: u32, bytes: u64) -> Result<u64> {
    if buffer >= g.buffers {
        return Err(EINVAL);
    }
    let (reg, offset) = if f.own_mode() {
        (*f.own_at(buffer), 0)
    } else {
        (f.region[POOL], u64::from(buffer).checked_mul(g.stride).ok_or(EFAULT)?)
    };
    if reg.buf.is_null() {
        return Err(EFAULT);
    }
    let addr = reg.addr.checked_add(offset).ok_or(EFAULT)?;
    match addr.checked_add(bytes) {
        Some(end) if end <= reg.end => Ok(addr),
        _ => Err(EFAULT),
    }
}

/// Checks one range and resolves it to a region and offset. Touches nothing. `addr` and `len` are
/// kernel locals: the job was copied before this.
pub(crate) fn resolve(g: &Geom, f: &mut Frame, buffer: u32, addr: u64, len: u64) -> Result<JobRange> {
    if len == 0 {
        return Err(EINVAL);
    }
    let end = addr.checked_add(len).ok_or(EOVERFLOW)?;
    let inside = |reg: &Region| !reg.buf.is_null() && addr >= reg.addr && end <= reg.end;

    // Without a pool: inside one buffer's memory, which must be the job's own.
    if f.own_mode() {
        for b in 0..g.buffers {
            let reg = *f.own_at(b);
            if !inside(&reg) {
                continue;
            }
            if b != buffer {
                return Err(EACCES);
            }
            return Ok(JobRange { region: REGIONS as u32 + b, offset: addr - reg.addr, len });
        }
    }
    let pool = &f.region[POOL];
    if inside(pool) {
        let offset = addr - pool.addr;
        let index = offset / g.stride;
        let within = offset - index * g.stride;
        if index != u64::from(buffer) {
            return Err(EACCES);
        }
        if within.saturating_add(len) > u64::from(g.buffer_bytes) {
            return Err(EFAULT);
        }
        return Ok(JobRange { region: POOL as u32, offset, len });
    }
    for (r, reg) in f.region.iter().enumerate().skip(POOL + 1) {
        if inside(reg) {
            return Ok(JobRange { region: r as u32, offset: addr - reg.addr, len });
        }
    }
    Err(EFAULT)
}

/// What a job may be posted on: a buffer that is free. EFAULT without memory, EBUSY while a job names
/// it or the source has it, EACCES while the consumer has it.
pub(crate) fn buffer_free(f: &mut Frame, buffer: u32) -> Result {
    match f.buf(buffer).state {
        BUF_FREE => Ok(()),
        BUF_ABSENT => Err(EFAULT),
        BUF_QUEUED | BUF_ACTIVE => Err(EBUSY),
        _ => Err(EACCES),
    }
}

/// Checks a job and, only when all of it is good, holds what it names and queues it.
fn accept(g: &Geom, f: &mut Frame, posted: &lb::lemnos_stream_job) -> Result {
    if posted.buffer >= g.buffers {
        return Err(EINVAL);
    }
    // A consumer that let go of the buffer's marks gets it back to post a job on it.
    enforce(g, f, now_ns());
    reclaim(g, f, posted.buffer);
    buffer_free(f, posted.buffer)?;
    let count = posted.count as usize;
    if count > JOB_RANGES {
        return Err(EINVAL);
    }
    if posted.range[count..].iter().any(|r| r.addr != 0 || r.len != 0) {
        return Err(EINVAL);
    }

    let mut job = Job { identity: posted.identity, buffer: posted.buffer, count: posted.count, ..Job::default() };
    for (out, r) in job.range.iter_mut().zip(&posted.range[..count]) {
        *out = resolve(g, f, posted.buffer, r.addr, r.len)?;
    }
    for r in &job.range[..count] {
        f.region_at(r.region).users += 1;
    }
    f.buf(posted.buffer).state = BUF_QUEUED;
    *f.queue_slot(f.queue_head, g.job_slots) = job;
    f.queue_head += 1;
    f.jobs += 1;
    Ok(())
}

/// Copies each posted job once to a local and accepts it.
fn collect_jobs(g: &Geom, f: &mut Frame) -> Result {
    let slots = u64::from(g.job_slots);
    for _ in 0..slots {
        let head = g.read_acquire(g.w_jobs_head);
        let count = head.wrapping_sub(f.jobs_tail);
        if count == 0 {
            return Ok(());
        }
        if count > slots {
            return Err(ERANGE);
        }
        // Full here: wait for one to finish rather than refuse it.
        if f.queue_head - f.queue_tail == slots {
            return Ok(());
        }
        let at = g.jobs_at + (f.jobs_tail & (slots - 1)) as usize * g.job_words;
        if g.read(at) != core::mem::size_of::<lb::lemnos_stream_job>() as u64 {
            return Err(EMSGSIZE);
        }
        let mut words = [0u64; JOB_WORDS];
        for (i, word) in words.iter_mut().enumerate() {
            *word = g.read(at + 1 + i);
        }
        // SAFETY: a `struct lemnos_stream_job` is JOB_WORDS words of plain integers with no padding,
        // so any words are a value of it; this is the kernel's one copy of what the consumer posted.
        let posted: lb::lemnos_stream_job = unsafe { core::mem::transmute(words) };
        accept(g, f, &posted)?;
        f.jobs_tail += 1;
        g.write_release(g.w_jobs_tail, f.jobs_tail);
    }
    Ok(())
}

/// Everything the consumer has posted, bounded. A refusal ends the stream.
pub(crate) fn drain(g: &Geom, f: &mut Frame) -> Result {
    collect_jobs(g, f).inspect_err(|e| fault_locked(g, f, e.to_errno()))
}

/// The next job. With none queued, a stream that has never had a job takes the next free buffer; one
/// that has is job-driven and takes nothing, or a frame could take the buffer its consumer just let
/// go of to post a job on.
pub(crate) fn take(g: &Geom, f: &mut Frame) -> Option<Job> {
    if f.queue_tail != f.queue_head {
        let job = *f.queue_slot(f.queue_tail, g.job_slots);
        f.queue_tail += 1;
        f.buf(job.buffer).state = BUF_ACTIVE;
        return Some(job);
    }
    if f.jobs != 0 {
        return None;
    }
    let jobless = |f: &mut Frame, b: u32| {
        f.cursor = (b + 1) % g.buffers;
        f.buf(b).state = BUF_ACTIVE;
        Some(Job { buffer: b, ..Job::default() })
    };
    for i in 0..g.buffers {
        let b = (f.cursor + i) % g.buffers;
        if f.buf(b).state == BUF_FREE {
            return jobless(f, b);
        }
    }
    // Take back one no reader holds; the newest frame's buffer only once every reader has read it.
    enforce(g, f, now_ns());
    for i in 0..g.buffers {
        let b = (f.cursor + i) % g.buffers;
        if (b == f.newest && !newest_read(g, f, b)) || f.buf(b).state != BUF_CONSUMER || !reclaim(g, f, b) {
            continue;
        }
        return jobless(f, b);
    }
    None
}

/// Lets go of what a job's ranges held.
pub(crate) fn unhold(f: &mut Frame, job: &Job) {
    for r in job.range.iter().take(job.count as usize) {
        let reg = f.region_at(r.region);
        reg.users = reg.users.wrapping_sub(1);
    }
}

/// Publishes a filled buffer, or gives it back when the stream has ended. `bytes` is the kernel's own
/// figure; 0 says the block flagged the frame as bad. A publish always fits: a reader that falls
/// behind skips ahead, and buffers are taken back by their marks.
pub(crate) fn finish_locked(g: &Geom, f: &mut Frame, job: &Job, bytes: u32, sequence: u64, timestamp_ns: u64, end_ns: u64) {
    unhold(f, job);
    if f.dead || f.fault != 0 || (bytes == 0 && f.jobs == 0) {
        // With no job behind it nobody is waiting for this buffer. A job-driven stream's consumer
        // is told, so it isn't left waiting on an identity that never comes.
        f.buf(job.buffer).state = BUF_FREE;
        return;
    }
    let b = f.buf(job.buffer);
    b.fence = b.fence.wrapping_add(1);
    let fence = b.fence;
    publish(g, f, job.buffer, bytes, sequence, job.identity, fence, timestamp_ns, end_ns);
    f.buf(job.buffer).state = BUF_CONSUMER;
    f.newest = job.buffer;
    f.newest_seq = sequence;
    f.published += 1;
}

/// A free-running stream with no free buffer for its next frame drops that frame and counts it
/// (docs/streams.md, "The replacement rule"; shm/README.md, "When there is none, it drops the frame").
/// Only when a published frame's buffer is held or is the newest one: a stream whose buffers have no
/// memory yet makes no frame, and a job-driven stream refuses the job instead.
pub(crate) fn drop_frame_locked(g: &Geom, f: &mut Frame) -> bool {
    if f.jobs != 0 {
        return false;
    }
    if (0..g.buffers).any(|b| f.buf(b).state == BUF_CONSUMER) {
        f.dropped += 1;
        return true;
    }
    false
}

/// The checks every programmed frame must pass, against the module's own copies: the frame's end, in
/// device addresses, must lie inside the buffer. A frame is bytes here: what lines or planes fill them
/// is the producer's to check. Without a pool each buffer was held to `buffer_bytes` and the alignment
/// when it was given its memory, and is again each time it is handed out.
pub(crate) fn check_frame(g: &Geom, f: &Frame, bytes: u32) -> Result {
    if bytes == 0 || bytes > g.buffer_bytes || u64::from(bytes) > g.stride {
        return Err(EINVAL);
    }
    if f.own_mode() {
        return Ok(());
    }
    let pool = &f.region[POOL];
    if pool.buf.is_null() || pool.addr & (DMA_ALIGN - 1) != 0 {
        return Err(EINVAL);
    }
    let end = (u64::from(g.buffers) - 1)
        .checked_mul(g.stride)
        .and_then(|last| pool.addr.checked_add(last))
        .and_then(|last| last.checked_add(u64::from(bytes)));
    match end {
        Some(end) if end <= pool.end => Ok(()),
        _ => Err(EFAULT),
    }
}

// What lemnos_stream.c still calls, under its lock, until its callers are ported: the C names and
// signatures of the functions above (lemnos_stream_priv.h).

use core::ffi::c_int;

fn errno(r: Result) -> c_int {
    r.err().map_or(0, |e| e.to_errno())
}

/// The frame path's two halves of a stream.
///
/// # Safety
/// `s` is a live stream whose lock the caller holds (or that nothing else can reach yet).
unsafe fn parts<'a>(s: *mut lb::lem_stream) -> (&'a Geom, &'a mut Frame) {
    // SAFETY: the caller's contract: the geometry is never written after creation, and the frame
    // state is the lock holder's alone.
    unsafe { (&(*s).g, &mut (*s).f) }
}

/// # Safety
/// `s` is a live stream whose lock the caller holds.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_fault_locked(s: *mut lb::lem_stream, err: c_int) {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    fault_locked(g, f, err);
}

/// # Safety
/// `s` is a live stream whose lock the caller holds.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_drain(s: *mut lb::lem_stream) -> c_int {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    errno(drain(g, f))
}

/// # Safety
/// `s` is a live stream whose lock the caller holds; `job` is the caller's to write.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_take(s: *mut lb::lem_stream, job: *mut Job) -> bool {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    match take(g, f) {
        Some(taken) => {
            // SAFETY: the caller's contract: `job` is valid for a write.
            unsafe { job.write(taken) };
            true
        }
        None => false,
    }
}

/// # Safety
/// `s` is a live stream whose lock the caller holds; `job` is a job `lem_stream_take` gave.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_finish_locked(
    s: *mut lb::lem_stream,
    job: *const Job,
    bytes: u32,
    sequence: u64,
    timestamp_ns: u64,
    end_ns: u64,
) {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    // SAFETY: the caller's contract: a held job, valid for the call.
    finish_locked(g, f, unsafe { &*job }, bytes, sequence, timestamp_ns, end_ns);
}

/// # Safety
/// `s` is a live stream whose lock the caller holds.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_drop_frame_locked(s: *mut lb::lem_stream) -> bool {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    drop_frame_locked(g, f)
}

/// # Safety
/// `s` is a live stream whose lock the caller holds, or one nothing else can reach yet.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_check_frame(s: *mut lb::lem_stream, bytes: u32) -> c_int {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    errno(check_frame(g, f, bytes))
}

/// # Safety
/// `s` is a live stream whose lock the caller holds; `addr` is the caller's to write.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_buffer_addr(s: *mut lb::lem_stream, buffer: u32, bytes: u64, addr: *mut u64) -> c_int {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    match buffer_addr(g, f, buffer, bytes) {
        Ok(a) => {
            // SAFETY: the caller's contract: `addr` is valid for a write.
            unsafe { addr.write(a) };
            0
        }
        Err(e) => e.to_errno(),
    }
}

/// # Safety
/// `s` is a live stream whose lock the caller holds.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_enforce(s: *mut lb::lem_stream, now: u64) {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    enforce(g, f, now);
}

/// # Safety
/// `s` is a live stream whose lock the caller holds; `buffer` is below its buffers.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_reclaim(s: *mut lb::lem_stream, buffer: u32) -> bool {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    reclaim(g, f, buffer)
}

/// # Safety
/// `s` is a live stream whose lock the caller holds; `out` is the caller's to write.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_resolve(s: *mut lb::lem_stream, buffer: u32, addr: u64, len: u64, out: *mut JobRange) -> c_int {
    // SAFETY: the caller's contract.
    let (g, f) = unsafe { parts(s) };
    match resolve(g, f, buffer, addr, len) {
        Ok(r) => {
            // SAFETY: the caller's contract: `out` is valid for a write.
            unsafe { out.write(r) };
            0
        }
        Err(e) => e.to_errno(),
    }
}

/// # Safety
/// `s` is a stream being created: nothing else can reach it yet, and its ring is allocated.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_stream_init_ring(s: *mut lb::lem_stream) {
    // SAFETY: the caller's contract.
    let (g, _) = unsafe { parts(s) };
    g.init_ring();
}
