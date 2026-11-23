// SPDX-License-Identifier: GPL-2.0
//! A frame stream and its file (the stream port): creation, the ioctls, the mapping, the end.
//!
//! The module is the producer of a stream, on the ring the socket protocol's streams already use
//! (lemnos.h; docs/wire.md). It publishes frames, wakes the consumer, takes back buffers no reader
//! holds and collects jobs, and checks every address a job names against dma-bufs it has mapped and
//! pinned. Any broken rule ends the stream with an errno and state 2: its own stream, nothing else,
//! and nothing is reachable that warns or loops on what the consumer wrote.
//!
//! Locking: the stream's lock (`f`) is the frame path's, taken by the source (a block's interrupt
//! handler, or the test source) and by KICK. The control mutex (`ctl`) orders registration and
//! teardown and is never taken on the frame path. The source keeps no lock while it writes memory;
//! instead teardown stops it (the block's stop callback, cancel_delayed_work_sync) before releasing
//! anything, and the regions a job names stay held (`users`) until the job finishes. Lock order: the
//! core's lem_blocks_sem, STREAMS, a stream's control mutex, its lock.
//!
//! Derived in part from the capture module's buffer queue on the dev branch (a queued buffer names a
//! dma-buf offset; the module keeps the mapping while the buffer is in use). The ring protocol and
//! everything checking it are new.
//!
//! Copyright (c) 2026 the Lemnos authors.

use crate::bindings as lb;
use crate::param::param;
use crate::stream::{DelayedWork, JOB_HOLDS, Job, Producer, Work, Wq};
use crate::stream_frame::{POOL, REGIONS, check_frame, drain, drop_frame_locked, finish_locked, now_ns, take};
use crate::stream_producer::{lem_stream_fault, over};
use crate::stream_reader::{self, Bell, EventFd, Fops};
use crate::stream_region::{Ctl, Region, cpu_read_enable, register, register_lent, set_buffer, unregister};
use crate::stream_ring::{
    BUF_ABSENT, BUF_FREE, Buf, Frame, Geom, JOB_HEAD, JOB_SLOTS, JOB_TAIL, POOL_READERS, POOL_TICKETS,
    Reader, SHM_INDEX, SHM_SLOT_WORDS, SHM_W_END,
};
use core::{
    ffi::c_void,
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};
use kernel::{
    ffi::{c_int, c_long, c_uint, c_ulong},
    alloc::{KVVec, KVec, flags::GFP_KERNEL},
    bindings,
    error::{code::*, from_err_ptr},
    ioctl::{_IO, _IOC_SIZE, _IOR, _IOW, _IOWR},
    mm::virt::{VmaNew, flags::SHARED},
    new_mutex, new_spinlock_irq,
    prelude::*,
    sync::{Arc, ArcBorrow, Mutex, SpinLockIrq, lock::{Guard, mutex::MutexBackend, spinlock::SpinLockIrqBackend}},
    transmute::{AsBytes, FromBytes},
    types::Opaque,
    uaccess::{UserPtr, UserSlice},
};

/// Each stream is kernel memory, so their number has an end.
const STREAM_MAX: usize = 64;

param!(TEST_STREAM_SOURCE, test_stream_source: bool = false, param_ops_bool, "bool",
    "Test only: LEMNOS_STREAM_IOC_TEST_SOURCE produces frames with nothing behind them");

/// Whether the test source is allowed, and with it the kernel mapping of every region it fills.
pub(crate) fn test_stream_source() -> bool {
    TEST_STREAM_SOURCE.get()
}

const IOC_KICK: c_uint = _IO(b'L' as u32, 0x81);
const IOC_REGISTER: c_uint = _IOWR::<lb::lemnos_stream_register>(b'L' as u32, 0x82);
const IOC_UNREGISTER: c_uint = _IOW::<lb::lemnos_stream_unregister>(b'L' as u32, 0x83);
const IOC_STATUS: c_uint = _IOR::<lb::lemnos_stream_status>(b'L' as u32, 0x84);
const IOC_TEST_SOURCE: c_uint = _IOW::<lb::lemnos_stream_test_source>(b'L' as u32, 0x85);
const IOC_BUFFER: c_uint = _IOWR::<lb::lemnos_stream_buffer>(b'L' as u32, 0x86);
const IOC_READER: c_uint = _IOWR::<lb::lemnos_stream_reader>(b'L' as u32, 0x87);
const IOC_READER_V0: c_uint = _IOWR::<lb::lemnos_stream_reader_v0>(b'L' as u32, 0x87);
const IOC_BELL: c_uint = _IOW::<lb::lemnos_stream_bell>(b'L' as u32, 0x89);
const IOC_REGISTER_LENT: c_uint = _IOWR::<lb::lemnos_stream_register_lent>(b'L' as u32, 0x8a);

const _: () = assert!(size_of::<lb::lemnos_stream_reader_v0>() == 24);
const _: () = assert!(size_of::<lb::lemnos_stream_reader>() == 48);
const _: () = assert!(offset_of!(lb::lemnos_stream_reader, refused) == size_of::<lb::lemnos_stream_reader_v0>());
const _: () = assert!(offset_of!(lb::lemnos_stream_reader, max_hold_ns) == offset_of!(lb::lemnos_stream_reader_v0, max_hold_ns));
const _: () = assert!(size_of::<lb::lemnos_stream_bell>() == 8);

// SAFETY: each is plain integers with no padding (lemnos.h), so any bytes are a value and every byte
// of a value is initialised.
unsafe impl FromBytes for lb::lemnos_stream_register {}
unsafe impl AsBytes for lb::lemnos_stream_register {}
unsafe impl FromBytes for lb::lemnos_stream_register_lent {}
unsafe impl AsBytes for lb::lemnos_stream_register_lent {}
unsafe impl FromBytes for lb::lemnos_stream_unregister {}
unsafe impl FromBytes for lb::lemnos_stream_buffer {}
unsafe impl AsBytes for lb::lemnos_stream_buffer {}
unsafe impl FromBytes for lb::lemnos_stream_reader {}
unsafe impl AsBytes for lb::lemnos_stream_reader {}
unsafe impl AsBytes for lb::lemnos_stream_status {}
unsafe impl FromBytes for lb::lemnos_stream_test_source {}
unsafe impl FromBytes for lb::lemnos_stream_bell {}

/// THIS_MODULE: the kernel's own `__this_module` for a module, null built in.
pub(crate) const fn this_module() -> *mut bindings::module {
    #[cfg(MODULE)]
    {
        (&raw const bindings::__this_module).cast_mut()
    }
    #[cfg(not(MODULE))]
    {
        ptr::null_mut()
    }
}

/// One frame stream: what `struct lem_stream *` points at for the blocks.
#[pin_data(PinnedDrop)]
pub(crate) struct Stream {
    /// The block, held until the stream is freed.
    dev: *mut bindings::device,
    /// The eventfd given at creation, or null.
    doorbell: *mut bindings::eventfd_ctx,
    g: Geom,
    /// The frame path's state.
    #[pin]
    f: SpinLockIrq<Frame>,
    /// Regions, the source, teardown.
    #[pin]
    ctl: Mutex<Ctl>,
    #[pin]
    wait: Opaque<bindings::wait_queue_head>,
    /// The test source.
    #[pin]
    source: DelayedWork,
    period_us: AtomicU32,
    /// The block going away, see `lem_streams_block_gone`.
    #[pin]
    gone: Work,
    /// The consumer's doorbell, LEMNOS_STREAM_IOC_BELL.
    bell: Bell,
}

// SAFETY: the raw pointers are to kernel objects the stream holds references on (the device, the
// eventfd) and to its own ring; everything that changes is behind the lock, the mutex or an atomic.
unsafe impl Send for Stream {}
// SAFETY: as Send.
unsafe impl Sync for Stream {}

/// The streams that exist, for the block that goes away, and their count, which has an end.
struct Registry {
    streams: [*const Stream; STREAM_MAX],
    count: usize,
}

// SAFETY: the pointers are streams whose files exist: a stream leaves the table, under its lock,
// before its file's reference is dropped.
unsafe impl Send for Registry {}

kernel::sync::global_lock! {
    // SAFETY: initialised by `init`, at module init, before any stream can be made.
    unsafe(uninit) static STREAMS: Mutex<Registry> = Registry { streams: [ptr::null(); STREAM_MAX], count: 0 };
}

/// Called once, at module init, before any block can be bound.
pub(crate) fn init() {
    // SAFETY: called once, from the module's init, before anything takes the lock.
    unsafe { STREAMS.init() };
}

/// The kernel's shared workqueue (system_wq), which schedule_work() and schedule_delayed_work() use.
fn system_wq() -> Wq {
    // SAFETY: set once at boot, before any module code runs, and never changed.
    Wq(unsafe { bindings::system_wq })
}

impl Stream {
    /// What creation fixed.
    pub(crate) fn geom(&self) -> &Geom {
        &self.g
    }

    /// The frame path's state, with the stream's lock held and interrupts off.
    pub(crate) fn lock(&self) -> Guard<'_, Frame, SpinLockIrqBackend> {
        self.f.lock()
    }

    /// What the control mutex guards, with it held.
    pub(crate) fn ctl(&self) -> Guard<'_, Ctl, MutexBackend> {
        self.ctl.lock()
    }

    /// The stream's block.
    pub(crate) fn dev(&self) -> *mut bindings::device {
        self.dev
    }

    fn as_ptr(&self) -> *mut Stream {
        ptr::from_ref(self).cast_mut()
    }

    /// Wakes whoever is waiting. Never under the stream's lock: the eventfd has its own.
    pub(crate) fn wake(&self) {
        // SAFETY: wake_up_interruptible on the stream's own queue, initialised at creation.
        unsafe { bindings::__wake_up(self.wait.get(), bindings::TASK_INTERRUPTIBLE, 1, ptr::null_mut()) };
        if !self.doorbell.is_null() {
            // SAFETY: the eventfd is held from creation until the stream is freed.
            unsafe { bindings::eventfd_signal_mask(self.doorbell, 0) };
        }
        self.bell.ring();
    }

    /// What a block would do: writes the frame, and each range a job named. Only kernel values reach
    /// this. The lock is held only to read where to write: it must not be held across a large copy,
    /// and the job's regions are held (`users`) and live until it completes; teardown cancels this
    /// work and waits for it before releasing anything.
    fn fill(&self, job: &Job, sequence: u64) {
        let count = (job.count as usize).min(JOB_HOLDS);
        let mut ranges = [(ptr::null_mut::<u8>(), 0usize); JOB_HOLDS];
        let frame = {
            let mut f = self.lock();
            for (to, r) in ranges.iter_mut().zip(&job.range[..count]) {
                let base = f.region_at(r.region).vaddr();
                if !base.is_null() {
                    *to = (base.wrapping_add(r.offset as usize), r.len as usize);
                }
            }
            if f.own_mode() {
                f.own_at(job.buffer).vaddr()
            } else {
                let pool = f.region[POOL].vaddr();
                if pool.is_null() { pool } else { pool.wrapping_add((u64::from(job.buffer) * self.g.stride) as usize) }
            }
        };
        let bytes = self.g.buffer_bytes as usize;
        if !frame.is_null() {
            for i in 0..bytes / 8 {
                // SAFETY: word `i` of the buffer's `buffer_bytes`, inside its region's kernel mapping.
                unsafe { frame.wrapping_add(i * 8).cast::<u64>().write_unaligned(sequence) };
            }
            // SAFETY: the buffer's last `bytes % 8` bytes, inside the same mapping.
            unsafe { ptr::write_bytes(frame.wrapping_add(bytes & !7), 0, bytes % 8) };
        }
        for (i, (to, len)) in ranges.iter().take(count).enumerate() {
            if !to.is_null() {
                // SAFETY: a range the stream resolved inside a region it holds for this job, written
                // through that region's kernel mapping.
                unsafe { ptr::write_bytes(*to, 0xa5 ^ i as u8, *len) };
            }
        }
    }

    /// LEMNOS_STREAM_IOC_TEST_SOURCE. The control mutex is held.
    fn test_source(&self, a: &lb::lemnos_stream_test_source) -> Result {
        if !test_stream_source() {
            return Err(EPERM);
        }
        if self.lock().producer.stop.is_some() {
            return Err(EBUSY);
        }
        self.source.cancel_sync();
        {
            let mut f = self.lock();
            self.period_us.store(a.period_us, Ordering::Relaxed);
            f.left = a.frames;
        }
        if a.frames != 0 {
            self.source.queue(system_wq(), 0);
        }
        Ok(())
    }

    /// Ends the stream: nothing runs on the frame path after this, every region is unpinned, and the
    /// consumer learns. Once only; waits for work in flight.
    fn shutdown(&self, state: u64) {
        let _ctl = self.ctl();
        let producer = {
            let mut f = self.lock();
            if f.dead {
                return;
            }
            f.dead = true;
            let producer = f.producer;
            f.producer.stop = None;
            if f.fault == 0 {
                self.g.write_release(self.g.w_state, state);
            }
            producer
        };
        self.source.cancel_sync();

        // The block's DMA stops here, before anything it could write to is released.
        if let Some(stop) = producer.stop {
            // SAFETY: the producer's own callback and private data, called once, in process context,
            // with the stream's lock not held, as the interface says.
            unsafe { stop(producer.private, self.as_ptr()) };
        }
        if self.lock().wedged {
            // DMA may still run: keep every pin for good. Nothing else is safe.
            self.wake();
            return;
        }
        let regions = REGIONS as u32 + if self.lock().own_mode() { self.g.buffers } else { 0 };
        for i in 0..regions {
            // Taken out under the lock, unmapped (which sleeps) with it dropped.
            let mut r = core::mem::take(self.lock().region_at(i));
            r.unmap();
        }
        self.wake();
    }
}

#[pinned_drop]
impl PinnedDrop for Stream {
    /// The stream's file is gone, and every reader file that held the stream.
    fn drop(self: Pin<&mut Self>) {
        let (release, private, wedged) = {
            let f = self.lock();
            (f.producer.release, f.producer.private, f.wedged)
        };
        if let Some(release) = release {
            // SAFETY: the producer's own callback, called once: nothing can reach its private data any more.
            unsafe { release(private) };
        }
        if !self.doorbell.is_null() {
            // SAFETY: the reference creation took, dropped once.
            unsafe { bindings::eventfd_ctx_put(self.doorbell) };
        }
        if !wedged {
            // A wedged block's DMA may still run: its pins are kept for good.
            let regions = REGIONS as u32 + if self.lock().own_mode() { self.g.buffers } else { 0 };
            for i in 0..regions {
                let mut r = core::mem::take(self.lock().region_at(i));
                r.unmap();
            }
            for pin in self.ctl().pins.iter_mut() {
                pin.unmap();
            }
            // SAFETY: the reference creation took, dropped once.
            unsafe { bindings::put_device(self.dev) };
        }
        // SAFETY: the ring vmalloc_user() gave, which no mapping of the file outlives its pages' own
        // references for; freed once.
        unsafe { lb::vfree(self.g.ring.cast()) };
        // The readers' pages, the bell, the buffers' states and the queue go with their fields.
    }
}

/// The test source's work: one frame, or one drop.
unsafe extern "C" fn source_work(work: *mut bindings::work_struct) {
    // SAFETY: `work` is the work of the delayed work at `source` of a stream.
    let st = unsafe { work.cast::<u8>().sub(offset_of!(Stream, source)) }.cast::<Stream>();
    // SAFETY: teardown cancels the work and waits for it before the stream can be freed.
    let st = unsafe { &*st };
    let g = st.geom();
    let (job, sequence) = {
        let mut f = st.lock();
        if over(&f).is_some() || drain(g, &mut f).is_err() {
            drop(f);
            st.wake();
            return;
        }
        let job = take(g, &mut f);
        let sequence = f.sequence;
        // A frame with no free buffer is dropped and counted; the source keeps running (docs/streams.md).
        if job.is_some() || drop_frame_locked(g, &mut f) {
            f.sequence += 1;
        }
        (job, sequence)
    };
    if let Some(job) = &job {
        st.fill(job, sequence);
    }
    let again = {
        let mut f = st.lock();
        if let Some(job) = &job {
            finish_locked(g, &mut f, job, g.buffer_bytes, sequence, now_ns(), now_ns());
            if f.left != u32::MAX && f.left != 0 {
                f.left -= 1;
            }
        }
        !f.dead && f.fault == 0 && f.left != 0
    };
    st.wake();
    if again {
        // SAFETY: the exported conversion, valid for any value.
        let delay = unsafe { bindings::__usecs_to_jiffies(st.period_us.load(Ordering::Relaxed)) };
        // Nothing to fill: don't spin until the consumer gives one back.
        st.source.queue(system_wq(), if job.is_some() { delay } else { delay.max(1) });
    }
}

/// The block going away: the stream ends from a worker, see `lem_streams_block_gone`.
unsafe extern "C" fn gone_work(work: *mut bindings::work_struct) {
    // SAFETY: `work` is the work at `gone` of a stream in the table.
    let st = unsafe { work.cast::<u8>().sub(offset_of!(Stream, gone)) }.cast::<Stream>();
    // SAFETY: `lem_streams_block_gone` waits for the work with the table's lock held, so the stream
    // is live.
    let st = unsafe { &*st };
    st.shutdown(u64::from(lb::LEMNOS_RING_STATE_CLOSED));
}

fn copy_in<T: FromBytes>(arg: usize) -> Result<T> {
    UserSlice::new(UserPtr::from_addr(arg), size_of::<T>()).reader().read::<T>()
}

fn copy_out<T: AsBytes>(arg: usize, v: &T) -> Result {
    UserSlice::new(UserPtr::from_addr(arg), size_of::<T>()).writer().write(v)
}

fn errno(r: Result) -> c_long {
    r.err().map_or(0, |e| e.to_errno() as c_long)
}

/// The stream of one of our files.
///
/// # Safety
/// `file` is a live file whose operations are STREAM_FOPS.
unsafe fn stream_of<'a>(file: *mut bindings::file) -> ArcBorrow<'a, Stream> {
    // SAFETY: the caller's contract: the file is live.
    let st = unsafe { (*file).private_data }.cast::<Stream>();
    // SAFETY: the caller's contract: the file's private data is the reference creation gave it,
    // which it keeps until its release.
    unsafe { ArcBorrow::from_raw(st) }
}

unsafe extern "C" fn stream_ioctl(file: *mut bindings::file, cmd: c_uint, arg: c_ulong) -> c_long {
    // SAFETY: called by the VFS on one of our files.
    let st = unsafe { stream_of(file) };
    let arg = arg as usize;
    let g = st.geom();
    // What a stream that is over answers, under the control mutex, as each request below is.
    let ended = || over(&st.lock()).map(|e| e as c_long);

    match cmd {
        IOC_KICK => {
            let (kick, ret) = {
                let mut f = st.lock();
                match over(&f) {
                    Some(e) => (None, e as c_long),
                    None => match f.producer.kick {
                        Some(kick) => (Some((kick, f.producer.private)), 0),
                        None => (None, errno(drain(g, &mut f))),
                    },
                }
            };
            if let Some((kick, private)) = kick {
                // SAFETY: the producer's own callback and private data, no lock held, the stream not over.
                let ret = unsafe { kick(private) } as c_long;
                if ret != 0 && ret != ENODEV.to_errno() as c_long {
                    // SAFETY: the stream is live: its file is open.
                    unsafe { lem_stream_fault(st.as_ptr(), ret as c_int) };
                }
                return ret;
            }
            if ret != 0 && ret != ENODEV.to_errno() as c_long {
                st.wake();
            }
            ret
        }
        IOC_STATUS => {
            let status = {
                let f = st.lock();
                lb::lemnos_stream_status {
                    state: if f.fault != 0 {
                        lb::LEMNOS_RING_STATE_FAULT
                    } else if f.dead {
                        lb::LEMNOS_RING_STATE_CLOSED
                    } else {
                        lb::LEMNOS_RING_STATE_OPEN
                    } as u32,
                    fault: -f.fault,
                    frames: f.published,
                    dropped: f.dropped,
                    returned: f.returned,
                    jobs: f.jobs,
                }
            };
            errno(copy_out(arg, &status))
        }
        IOC_REGISTER => {
            let Ok(mut a) = copy_in::<lb::lemnos_stream_register>(arg) else { return errno(Err(EFAULT)) };
            let ret = {
                let mut ctl = st.ctl();
                ended().unwrap_or_else(|| errno(register(&st, &mut ctl, &mut a)))
            };
            if ret == 0 { errno(copy_out(arg, &a)) } else { ret }
        }
        IOC_REGISTER_LENT => {
            let Ok(mut a) = copy_in::<lb::lemnos_stream_register_lent>(arg) else { return errno(Err(EFAULT)) };
            if a.reserved != 0 {
                return errno(Err(EINVAL));
            }
            let ret = {
                let mut ctl = st.ctl();
                ended().unwrap_or_else(|| errno(register_lent(&st, &mut ctl, &mut a)))
            };
            if ret == 0 { errno(copy_out(arg, &a)) } else { ret }
        }
        IOC_UNREGISTER => {
            let Ok(a) = copy_in::<lb::lemnos_stream_unregister>(arg) else { return errno(Err(EFAULT)) };
            if a.reserved != 0 {
                return errno(Err(EINVAL));
            }
            let mut ctl = st.ctl();
            if st.lock().dead {
                return errno(Err(ENODEV));
            }
            errno(unregister(&st, &mut ctl, a.region))
        }
        IOC_BUFFER => {
            let Ok(mut a) = copy_in::<lb::lemnos_stream_buffer>(arg) else { return errno(Err(EFAULT)) };
            let ret = {
                let _ctl = st.ctl();
                ended().unwrap_or_else(|| errno(set_buffer(&st, &mut a)))
            };
            if ret == 0 { errno(copy_out(arg, &a)) } else { ret }
        }
        // Both sizes of the request: the argument is the caller's, so nothing is read or written past
        // the size its number says, and the refusal fields stay zero for a caller that has none.
        IOC_READER | IOC_READER_V0 => {
            let size = _IOC_SIZE(cmd);
            let mut bytes = [0u8; size_of::<lb::lemnos_stream_reader>()];
            if UserSlice::new(UserPtr::from_addr(arg), size).reader().read_slice(&mut bytes[..size]).is_err() {
                return errno(Err(EFAULT));
            }
            // SAFETY: the struct is plain integers with no padding, so any bytes are a value of it.
            let mut a: lb::lemnos_stream_reader = unsafe { core::mem::transmute(bytes) };
            a.refused = lb::LEMNOS_REFUSED_NONE;
            a.layout = 0;
            a.found = 0;
            a.expected = 0;
            let ret = {
                let _ctl = st.ctl();
                ended().unwrap_or_else(|| errno(stream_reader::add(st, &mut a)))
            };
            // A refusal is the one failure that carries an answer back.
            if (ret == 0 || ret == EPROTO.to_errno() as c_long)
                && UserSlice::new(UserPtr::from_addr(arg), size).writer().write_slice(&a.as_bytes()[..size]).is_err()
            {
                return errno(Err(EFAULT));
            }
            ret
        }
        IOC_BELL => {
            let Ok(a) = copy_in::<lb::lemnos_stream_bell>(arg) else { return errno(Err(EFAULT)) };
            let _ctl = st.ctl();
            ended().unwrap_or_else(|| errno(st.bell.set(&a)))
        }
        IOC_TEST_SOURCE => {
            let Ok(a) = copy_in::<lb::lemnos_stream_test_source>(arg) else { return errno(Err(EFAULT)) };
            let _ctl = st.ctl();
            ended().unwrap_or_else(|| errno(st.test_source(&a)))
        }
        _ => {
            // Under the control mutex, because a stream that's over may have lost its block.
            let _ctl = st.ctl();
            let (producer, dead) = {
                let f = st.lock();
                (f.producer, f.dead)
            };
            match producer.ioctl {
                None => errno(Err(ENOTTY)),
                Some(_) if dead => errno(Err(ENODEV)),
                // SAFETY: the producer's own callback and private data, the control mutex held, the
                // stream not over, as the interface says.
                Some(ioctl) => unsafe { ioctl(producer.private, cmd, arg as *mut c_void) as c_long },
            }
        }
    }
}

unsafe extern "C" fn stream_poll(file: *mut bindings::file, wait: *mut bindings::poll_table_struct) -> bindings::__poll_t {
    // SAFETY: called by the VFS on one of our files.
    let st = unsafe { stream_of(file) };
    // SAFETY: the file, the stream's own queue and the table the VFS passed.
    unsafe { bindings::poll_wait(file, st.wait.get(), wait) };
    // Frames come on the doorbell (wake), not on this file: only the end is readable here.
    if over(&st.lock()).is_some() {
        bindings::POLLIN | bindings::POLLRDNORM | bindings::POLLHUP
    } else {
        0
    }
}

unsafe extern "C" fn stream_mmap(file: *mut bindings::file, vma: *mut bindings::vm_area_struct) -> c_int {
    // SAFETY: called by the VFS on one of our files.
    let st = unsafe { stream_of(file) };
    // SAFETY: the VFS passes a new mapping being set up, which is ours to read and adjust here.
    let area = unsafe { VmaNew::from_raw(vma) };
    // SAFETY: as above; reads one field.
    let pgoff = unsafe { (*vma).vm_pgoff };
    let producer = st.lock().producer;
    if let (true, Some(mmap)) = (pgoff != 0, producer.mmap) {
        // SAFETY: the producer's own callback and private data, no lock held, as the interface says.
        return unsafe { mmap(producer.private, vma) };
    }
    let ring_bytes = st.g.ring_bytes.next_multiple_of(bindings::PAGE_SIZE);
    if area.flags() & SHARED == 0 || pgoff != 0 || area.end() - area.start() != ring_bytes {
        return EINVAL.to_errno();
    }
    // An executable mapping is refused, and one that is not can't be made so later.
    if area.executable() || area.try_clear_mayexec().is_err() {
        return EPERM.to_errno();
    }
    // SAFETY: the ring is the stream's own vmalloc_user() memory of `ring_bytes` rounded up to pages,
    // and the mapping's length was checked against exactly that.
    unsafe { lb::remap_vmalloc_range(vma, st.g.ring.cast(), 0) }
}

unsafe extern "C" fn stream_release(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    // SAFETY: the VFS calls this with the file being released, still live.
    let st = unsafe { (*file).private_data }
        .cast::<Stream>()
        .cast_const();
    // SAFETY: the reference creation gave this file, taken back once, when its last reference closes.
    let st = unsafe { Arc::from_raw(st) };
    {
        let mut all = STREAMS.lock();
        if let Some(at) = all.streams.iter_mut().find(|p| ptr::eq(**p, Arc::as_ptr(&st))) {
            *at = ptr::null();
        }
        all.count -= 1;
    }
    st.shutdown(u64::from(lb::LEMNOS_RING_STATE_CLOSED));
    0
}

static STREAM_FOPS: Fops = Fops(bindings::file_operations {
    owner: this_module(),
    release: Some(stream_release),
    poll: Some(stream_poll),
    mmap: Some(stream_mmap),
    unlocked_ioctl: Some(stream_ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: a file_operations of null pointers is the table with nothing else set.
    ..unsafe { core::mem::zeroed() }
});

/// The limits both sides of the protocol enforce (Layout::checked).
fn check_layout(c: &lb::lemnos_stream_create) -> Result {
    let own = c.flags & lb::LEMNOS_STREAM_CREATE_BUFFERS != 0;
    if c.flags & !lb::LEMNOS_STREAM_CREATE_BUFFERS != 0 {
        return Err(EINVAL);
    }
    // The frame ring is the shared-memory ring, which needs a slot per frame in flight and a second for the reader.
    if c.frame_slots < 2 {
        return Err(EINVAL);
    }
    if c.buffers == 0 || c.buffers > lb::LEMNOS_STREAM_MAX_BUFFERS {
        return Err(EINVAL);
    }
    // No pool: no pool descriptor either, and a table of dma-bufs with an end.
    if own && (c.pool_fd != -1 || c.buffers > lb::LEMNOS_STREAM_MAX_OWN_BUFFERS) {
        return Err(EINVAL);
    }
    if c.buffer_bytes == 0 || c.buffer_bytes > lb::LEMNOS_STREAM_MAX_BUFFER_BYTES {
        return Err(EINVAL);
    }
    if !c.frame_slots.is_power_of_two() || c.frame_slots < c.buffers || c.frame_slots > 2 * lb::LEMNOS_STREAM_MAX_BUFFERS {
        return Err(EINVAL);
    }
    if !c.job_slots.is_power_of_two() || c.job_slots > lb::LEMNOS_STREAM_MAX_JOB_SLOTS {
        return Err(EINVAL);
    }
    if (c.job_bytes as usize) < size_of::<lb::lemnos_stream_job>() || c.job_bytes > lb::LEMNOS_STREAM_MAX_JOB_BYTES {
        return Err(EINVAL);
    }
    if u64::from(c.buffers) * stride_of(c.buffer_bytes) > lb::LEMNOS_STREAM_MAX_POOL_BYTES as u64 {
        return Err(EINVAL);
    }
    Ok(())
}

/// A buffer's slice of the pool: its bytes, rounded up to LEMNOS_STREAM_BUFFER_ALIGN.
fn stride_of(buffer_bytes: u32) -> u64 {
    u64::from(buffer_bytes).next_multiple_of(u64::from(lb::LEMNOS_STREAM_BUFFER_ALIGN))
}

/// The ring's memory until a stream owns it: freed when dropped.
struct Ring(*mut u64);

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: memory vmalloc_user() gave (or null), which nothing has mapped yet; freed once.
        unsafe { lb::vfree(self.0.cast()) };
    }
}

/// The pool's pin until a stream owns it: unmapped when dropped.
struct Pinned(Region);

impl Drop for Pinned {
    fn drop(&mut self) {
        self.0.unmap();
    }
}

/// Makes the stream and its file. `c` is the core's copy of the request. Called with the core's
/// lem_blocks_sem held for reading: the block can't go while this runs.
fn create(dev: *mut bindings::device, c: &mut lb::lemnos_stream_create, producer: Option<&Producer>, out: *mut *mut Stream) -> Result {
    check_layout(c)?;
    {
        let mut all = STREAMS.lock();
        if all.count >= STREAM_MAX {
            return Err(ENOSPC);
        }
        all.count += 1;
    }
    let made = create_counted(dev, c, producer, out);
    if made.is_err() {
        STREAMS.lock().count -= 1;
    }
    made
}

fn create_counted(dev: *mut bindings::device, c: &mut lb::lemnos_stream_create, producer: Option<&Producer>, out: *mut *mut Stream) -> Result {
    let own = c.flags & lb::LEMNOS_STREAM_CREATE_BUFFERS != 0;
    let (buffers, frame_slots, job_slots) = (c.buffers as usize, c.frame_slots as usize, c.job_slots as usize);
    let job_words = 1 + (c.job_bytes as usize).div_ceil(8);
    // The words, in order: the frame ring (header, lane head, index, slots), the pool page (magic and
    // shape, tickets, cut-off words), then the job block: its head and tail, and the job slots. A
    // buffer goes back by its marks, so nothing returns through the ring.
    let shm_slots_at = SHM_INDEX + frame_slots;
    let shm_total = shm_slots_at + frame_slots * SHM_SLOT_WORDS;
    let control = shm_total + POOL_TICKETS + buffers + POOL_READERS;
    let jobs_at = control + JOB_SLOTS;
    let ring_bytes = (jobs_at + job_slots * job_words) * 8;
    let stride = stride_of(c.buffer_bytes);
    let pool_bytes = u64::from(c.buffers) * stride;

    let mut readers = KVec::with_capacity(POOL_READERS, GFP_KERNEL)?;
    for _ in 0..POOL_READERS {
        // The capacity was reserved above, so this neither allocates nor fails.
        let _ = readers.push_within_capacity(Reader::empty());
    }
    let state = Buf { fence: 0, state: if own { BUF_ABSENT } else { BUF_FREE } };
    let mut f = Frame::new(
        KVVec::from_elem(state, buffers, GFP_KERNEL)?,
        KVVec::from_elem(Job::default(), job_slots, GFP_KERNEL)?,
        if own { KVVec::from_elem(Region::default(), buffers, GFP_KERNEL)? } else { KVVec::new() },
        readers,
    );
    // SAFETY: allocates zeroed memory that may be mapped to user space; freed by Ring or the stream.
    let ring = Ring(unsafe { lb::vmalloc_user_noprof(ring_bytes.next_multiple_of(bindings::PAGE_SIZE) as c_ulong) }.cast());
    if ring.0.is_null() {
        return Err(ENOMEM);
    }
    let doorbell = if c.doorbell_fd != -1 { Some(EventFd::get(c.doorbell_fd)?) } else { None };
    let mut pool = Pinned(Region::default());
    if !own {
        pool.0 = Region::map(dev, c.pool_fd)?;
        // Only what the buffers use is reachable, however big the dma-buf.
        if pool.0.size < pool_bytes {
            return Err(EINVAL);
        }
        pool.0.size = pool_bytes;
        pool.0.end = pool.0.addr + pool_bytes;
    }
    f.region[POOL] = pool.0;

    let g = Geom {
        ring: ring.0,
        ring_bytes,
        buffers: c.buffers,
        buffer_bytes: c.buffer_bytes,
        frame_slots: c.frame_slots,
        job_slots: c.job_slots,
        job_bytes: c.job_bytes,
        jobs_at,
        job_words,
        stride,
        shm_total,
        shm_slots_at,
        page_at: shm_total,
        w_state: SHM_W_END,
        w_jobs_head: control + JOB_HEAD,
        w_jobs_tail: control + JOB_TAIL,
    };
    g.init_ring();

    // The producer is attached, with a frame of the whole buffer, before the fd exists, so the
    // consumer never holds a stream without it.
    if let Some(producer) = producer {
        if producer.job_bytes != 0 && producer.job_bytes != g.job_bytes {
            return Err(EINVAL);
        }
        check_frame(&g, &f, c.buffer_bytes)?;
        f.producer = *producer;
        f.frame_bytes = c.buffer_bytes;
    }

    let pool_addr = pool.0.addr;
    let doorbell_ctx = doorbell.as_ref().map_or(ptr::null_mut(), EventFd::as_raw);
    let st = Arc::pin_init(
        pin_init!(Stream {
            dev,
            doorbell: doorbell_ctx,
            g,
            f <- new_spinlock_irq!(f, "lem_stream.lock"),
            ctl <- new_mutex!(Ctl::new(), "lem_stream.ctl"),
            wait <- Opaque::ffi_init(|p| {
                // SAFETY: initialises the queue in place, before any use; the name and class are static.
                unsafe { bindings::__init_waitqueue_head(p, c"lem_stream.wait".as_ptr().cast(), kernel::static_lock_class!().as_ptr()) }
            }),
            source <- DelayedWork::new(c"lem_stream.source", source_work),
            period_us: AtomicU32::new(0),
            gone <- Work::new(c"lem_stream.gone", gone_work),
            bell: Bell::new(),
        }),
        GFP_KERNEL,
    )?;
    // The stream owns them from here: its drop frees the ring, unmaps the pool and puts the eventfd.
    core::mem::forget(ring);
    core::mem::forget(pool);
    core::mem::forget(doorbell);
    // SAFETY: the block is live (the caller's contract); the reference is dropped with the stream.
    unsafe { bindings::get_device(dev) };
    let raw = Arc::into_raw(st).cast_mut();
    // It never existed: on a failure from here the producer's private data is still the caller's.
    let unmake = |e: Error| {
        // SAFETY: the reference made above, which no file took; nothing else can reach the stream.
        let st = unsafe { Arc::from_raw(raw.cast_const()) };
        st.lock().producer.release = None;
        e
    };

    // SAFETY: reserves a descriptor in this process; given back below if the file can't be made.
    let fd = unsafe { bindings::get_unused_fd_flags(bindings::O_CLOEXEC) };
    if fd < 0 {
        return Err(unmake(Error::from_errno(fd)));
    }
    // SAFETY: the name and the table are static; the reference is the file's from here, dropped by its release.
    let file = unsafe {
        lb::anon_inode_getfile(c"[lemnos-stream]".as_ptr().cast(), &raw const STREAM_FOPS.0, raw.cast(), bindings::O_RDWR as c_int)
    };
    let file = match from_err_ptr(file) {
        Ok(file) => file,
        Err(e) => {
            // SAFETY: the descriptor reserved above, never installed.
            unsafe { bindings::put_unused_fd(fd as c_uint) };
            return Err(unmake(e));
        }
    };
    {
        let mut all = STREAMS.lock();
        // The count was taken before the stream was made, so the table has a free slot.
        if let Some(at) = all.streams.iter_mut().find(|p| p.is_null()) {
            *at = raw;
        }
    }
    c.ring_bytes = ring_bytes as u64;
    c.pool_addr = pool_addr;
    c.fd = fd;
    if !out.is_null() {
        // SAFETY: the caller's contract: `out` is valid for a write. Set before the fd is installed.
        unsafe { out.write(raw) };
    }
    // SAFETY: the descriptor and the file made above; the file is the descriptor's from here.
    unsafe { bindings::fd_install(fd as c_uint, file) };
    Ok(())
}

/// LEMNOS_IOC_STREAM_CREATE, from the core: a stream on `dev`, or its errno.
///
/// # Safety
/// `dev` is a live block the caller keeps from going for the call; `c` is the caller's copy of the request.
pub(crate) unsafe extern "C" fn lem_stream_create(dev: *mut bindings::device, c: *mut lb::lemnos_stream_create) -> core::ffi::c_long {
    // SAFETY: the caller's contract.
    errno(create(dev, unsafe { &mut *c }, None, ptr::null_mut())) as core::ffi::c_long
}

/// `lem_stream_create()` for a block whose jobs are its own: the producer is attached, with a frame of
/// the whole buffer, before the fd exists. `*out` is set before the fd is installed. On failure
/// there's no stream and `producer->priv` is still the caller's; on success the stream owns it,
/// through `producer->release`.
///
/// # Safety
/// As `lem_stream_create`; `producer` is valid for the call; `out` is null or the caller's to write.
pub(crate) unsafe extern "C" fn lem_stream_create_producer(
    dev: *mut bindings::device,
    c: *mut lb::lemnos_stream_create,
    producer: *const Producer,
    out: *mut *mut Stream,
) -> core::ffi::c_long {
    // SAFETY: the caller's contract: the caller's copy of the request.
    let c = unsafe { &mut *c };
    // SAFETY: the caller's contract: valid for the call.
    let producer = unsafe { producer.as_ref() };
    errno(create(dev, c, producer, out)) as core::ffi::c_long
}

/// The stream open as `file`, if it was created on `dev`; an ERR_PTR otherwise.
///
/// # Safety
/// `file` is a live file; the stream answered is live while the caller holds the file.
pub(crate) unsafe extern "C" fn lem_stream_from_file(file: *mut bindings::file, dev: *mut bindings::device) -> *mut Stream {
    // SAFETY: the caller's contract; reads one field.
    if !ptr::eq(unsafe { (*file).f_op }, &raw const STREAM_FOPS.0) {
        return EINVAL.to_ptr();
    }
    // SAFETY: one of our files: its stream is live while the file is.
    let st = unsafe { stream_of(file) };
    if st.dev != dev {
        return EXDEV.to_ptr();
    }
    st.as_ptr()
}

/// The block's DMA can't be shown to have stopped: the stream keeps every pin, and the block keeps the
/// stream, which it may yet see stop (`lem_stream_unquarantine`). Answers the block's own reference to
/// the stream: given back by `lem_stream_unquarantine` only. A stream never let out stays pinned for good,
/// file closed or not, which is the wedge as it always was.
///
/// # Safety
/// `s` is a live stream.
pub(crate) unsafe extern "C" fn lem_stream_quarantine(s: *mut Stream) -> *mut Stream {
    // SAFETY: the caller's contract: a live stream, made by `create` as an Arc whose pointer this is.
    let borrowed = unsafe { ArcBorrow::from_raw(s.cast_const()) };
    borrowed.lock().wedged = true;
    let held: Arc<Stream> = borrowed.into();
    Arc::into_raw(held).cast_mut()
}

/// The quarantined stream's DMA was seen to stop: the wedge is over. A stream whose end already came
/// gives up now what its end kept (every region); a stream still open goes on as one whose producer
/// stopped. Then the block's reference goes, and with the last one the stream is freed as any other.
/// Takes the stream's control mutex: never under the block's `ctl` or locks.
///
/// # Safety
/// `s` is what `lem_stream_quarantine` answered, given back once.
pub(crate) unsafe extern "C" fn lem_stream_unquarantine(s: *mut Stream) {
    // SAFETY: the caller's contract: the reference lem_stream_quarantine took, given back once.
    let st = unsafe { Arc::from_raw(s.cast_const()) };
    {
        // Ordered with the end (`shutdown`), which holds this while it looks at `wedged`.
        let _ctl = st.ctl();
        let dead = {
            let mut f = st.lock();
            f.wedged = false;
            f.dead
        };
        if dead {
            let regions = REGIONS as u32 + if st.lock().own_mode() { st.g.buffers } else { 0 };
            for i in 0..regions {
                // Taken out under the lock, unmapped (which sleeps) with it dropped.
                let mut r = core::mem::take(st.lock().region_at(i));
                r.unmap();
            }
        }
    }
    st.wake();
}

/// The memory the block can write through the stream: how many dma-bufs it has pinned and their bytes,
/// as the dma-bufs are (a buffer's own dma-buf may be bigger than the buffer). Any context: the
/// stream's lock only.
///
/// # Safety
/// `s` is a live stream.
pub(crate) unsafe fn lem_stream_pinned(s: *mut Stream) -> (u32, u64) {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    let mut f = st.lock();
    let regions = REGIONS as u32 + if f.own_mode() { st.g.buffers } else { 0 };
    let (mut count, mut bytes) = (0u32, 0u64);
    for i in 0..regions {
        let buf = f.region_at(i).buf;
        if !buf.is_null() {
            count += 1;
            // SAFETY: a dma-buf the region holds a reference on; its size is fixed at its making.
            bytes += unsafe { (*buf).size } as u64;
        }
    }
    (count, bytes)
}

/// Registered regions keep a kernel mapping from here on, so a block can read through
/// `lem_stream_range_read()`. Takes the control mutex, so never under the block's locks.
///
/// # Safety
/// `s` is a live stream.
pub(crate) unsafe extern "C" fn lem_stream_cpu_read_enable(s: *mut Stream) -> c_int {
    // SAFETY: the caller's contract.
    let st = unsafe { &*s };
    let mut ctl = st.ctl();
    cpu_read_enable(st, &mut ctl).err().map_or(0, |e| e.to_errno())
}

/// The block is going: every stream on it ends here, and its mappings with it. From a worker, because
/// here a dma_buf_put would land on the unbind's task work, which runs only when the unbind returns,
/// and the unbind is waiting for that very release.
///
/// # Safety
/// `dev` is the block being unbound.
pub(crate) unsafe extern "C" fn lem_streams_block_gone(dev: *mut bindings::device) {
    let all = STREAMS.lock();
    for s in all.streams.iter().filter(|s| !s.is_null()) {
        // SAFETY: a stream in the table is live while the table's lock is held: its file's release
        // takes it out, under this lock, before dropping its reference.
        let st = unsafe { &**s };
        if st.dev != dev {
            continue;
        }
        st.gone.queue(system_wq());
        st.gone.flush();
    }
}
