// SPDX-License-Identifier: GPL-2.0
//! The interrupt notifications of the Lemnos blocks (stage 1f): the lock, the ready queue in fire order
//! and each interrupt's mask state, all guarded by IRQ_LOCK. The core (rs/dev.rs) keeps the handler's
//! wake-up, the requests and frees of each block's interrupts, and the read and poll paths, which call
//! these.
//!
//! A `LemIrq` lives in its block's record (rs/dev.rs). The
//! fields are read and written only under IRQ_LOCK, except the ones lem_irq_init and the probe set while
//! the line is still masked and not requested.
use core::ptr::null_mut;
use kernel::{
    bindings,
    ffi::c_int,
    prelude::*, //
};

/// One interrupt of a block: the record that `struct lem_irq` (C) describes.
#[repr(C)]
pub(crate) struct LemIrq {
    /// The next record in fire order, while queued.
    next: *mut LemIrq,
    /// The Linux interrupt number.
    irq: c_int,
    /// Masked by the handler until a read or an enable clears it.
    masked: bool,
    /// On the ready queue: fired and not yet read.
    queued: bool,
    /// The block's number and this interrupt's index on it, as read events name them.
    block_id: u32,
    index: u32,
    /// Fires since the block was bound, and the time of the latest.
    count: u32,
    ns: u64,
}

const _: () = assert!(core::mem::size_of::<LemIrq>() == 40);

/// The event one read returns, `struct lem_irq_event` (C).
#[repr(C)]
pub(crate) struct LemIrqEvent {
    pub(crate) block_id: u32,
    pub(crate) index: u32,
    pub(crate) count: u32,
    pub(crate) ns: u64,
}

const _: () = assert!(core::mem::size_of::<LemIrqEvent>() == 24);

/// The ready queue: the first and last queued record (linked through `next`).
pub(crate) struct Queue {
    head: *mut LemIrq,
    tail: *mut LemIrq,
}

// SAFETY: the pointers are only followed under IRQ_LOCK, and the records they name outlive their queueing:
// lem_irq_forget unlinks a record before its block is freed.
unsafe impl Send for Queue {}

kernel::sync::global_lock! {
    unsafe(uninit) static IRQ_LOCK: SpinLockIrq<Queue> = Queue { head: null_mut(), tail: null_mut() };
}

/// Appends `q` unless it is queued already. Lock held.
///
/// # Safety
/// `q` is a live record of a bound block, and no other reference to it is in use.
unsafe fn enqueue(queue: &mut Queue, q: *mut LemIrq) {
    // SAFETY: the caller's guarantee: the record is live, and its fields are ours to read.
    if unsafe { (*q).queued } {
        return;
    }
    // SAFETY: the record is live and not queued, so `queued` is ours to write.
    unsafe { (*q).queued = true };
    // SAFETY: as above, for `next`.
    unsafe { (*q).next = null_mut() };
    if queue.tail.is_null() {
        queue.head = q;
    } else {
        // SAFETY: the tail is a queued record, live by this function's contract; the lock the
        // caller holds makes its `next` ours to write.
        unsafe { (*queue.tail).next = q };
    }
    queue.tail = q;
}

/// Unlinks `q` if it is queued. Lock held.
///
/// # Safety
/// Every queued record is live, as `enqueue` requires.
unsafe fn unlink(queue: &mut Queue, q: *mut LemIrq) {
    // SAFETY: the caller's record, live, and read under the lock the caller holds.
    if !unsafe { (*q).queued } {
        return;
    }
    let mut prev: *mut LemIrq = null_mut();
    let mut cur = queue.head;
    while !cur.is_null() && cur != q {
        prev = cur;
        // SAFETY: `cur` is not null and is on the queue, whose records are live under the lock, by
        // `enqueue`'s contract.
        cur = unsafe { (*cur).next };
    }
    if cur.is_null() {
        return;
    }
    // SAFETY: `q` was found on the queue, so it is live and its link is the queue's, under the lock.
    let next = unsafe { (*q).next };
    if prev.is_null() {
        queue.head = next;
    } else {
        // SAFETY: `prev` is the queued record before `q`, live under the lock.
        unsafe { (*prev).next = next };
    }
    if queue.tail == q {
        queue.tail = prev;
    }
    // SAFETY: `q` is live and off the queue now, so its link is ours to clear.
    unsafe { (*q).next = null_mut() };
    // SAFETY: as above, for `queued`.
    unsafe { (*q).queued = false };
}

/// Sets up the lock, once, at module load, before any interrupt is requested.
///
/// # Safety
/// Called once from the module's init, before any block is bound.
pub(crate) unsafe extern "C" fn lem_irq_setup() {
    // SAFETY: the one initialisation, before any use.
    unsafe { IRQ_LOCK.init() };
}

/// Sets one record up while its block is probed: the line is masked and not requested yet.
///
/// # Safety
/// `q` is the block's own record, and nothing else refers to it.
pub(crate) unsafe extern "C" fn lem_irq_init(q: *mut LemIrq, irq: c_int, index: u32) {
    // SAFETY: the caller passes its own record, before the line is requested.
    unsafe {
        q.write(LemIrq {
            next: null_mut(),
            irq,
            masked: true,
            queued: false,
            block_id: 0,
            index,
            count: 0,
            ns: 0,
        });
    }
}

/// Sets the block's number on its record, once the block is numbered and before its line is enabled.
///
/// # Safety
/// `q` is the block's own record and its line is still masked.
pub(crate) unsafe extern "C" fn lem_irq_set_block(q: *mut LemIrq, block_id: u32) {
    // SAFETY: the caller's guarantee: nothing else reads or writes the record yet.
    unsafe { (*q).block_id = block_id };
}

/// The handler's work: masks the line at its controller, counts the fire and queues the record. The
/// caller wakes the readers.
///
/// # Safety
/// `q` is a requested record of a bound block, live for the request's life.
pub(crate) unsafe extern "C" fn lem_irq_fired(q: *mut LemIrq, ns: u64) {
    // SAFETY: the record is live for the request's life, and its irq number never changes after init.
    let irq = unsafe { (*q).irq };
    // Level interrupts stay masked until the driver has cleared the cause.
    // SAFETY: the line this record was requested for, which is the one that fired.
    unsafe { bindings::disable_irq_nosync(irq as u32) };
    let mut queue = IRQ_LOCK.lock();
    // SAFETY: the record is live, and the lock makes this the only use of its fields.
    unsafe { (*q).masked = true };
    // SAFETY: as above.
    let count = unsafe { (*q).count };
    // SAFETY: as above.
    unsafe { (*q).count = count.wrapping_add(1) };
    // SAFETY: as above.
    unsafe { (*q).ns = ns };
    // SAFETY: the record is live and the lock is held, which is `enqueue`'s contract.
    unsafe { enqueue(&mut queue, q) };
}

/// Clears the mask state for an enable. True when the line was masked, so the caller enables it.
///
/// # Safety
/// `q` is a live record of a bound block.
pub(crate) unsafe extern "C" fn lem_irq_unmask(q: *mut LemIrq) -> bool {
    let _queue = IRQ_LOCK.lock();
    // SAFETY: the record is live, and the lock makes this the only use of its fields.
    if unsafe { (*q).masked } {
        // SAFETY: as above.
        unsafe { (*q).masked = false };
        true
    } else {
        false
    }
}

/// Masks the record and drops it from the queue. True when the line was unmasked, so the caller masks it.
///
/// # Safety
/// `q` is a live record of a bound block.
pub(crate) unsafe extern "C" fn lem_irq_mask(q: *mut LemIrq) -> bool {
    let mut queue = IRQ_LOCK.lock();
    // SAFETY: the record is live, and the lock covers its fields.
    let was_unmasked = !unsafe { (*q).masked };
    // SAFETY: as above.
    unsafe { (*q).masked = true };
    // SAFETY: the record is live, and the lock covers the queue.
    unsafe { unlink(&mut queue, q) };
    was_unmasked
}

/// Drops the record from the queue, for its block's removal.
///
/// # Safety
/// `q` is a live record of a block being removed.
pub(crate) unsafe extern "C" fn lem_irq_forget(q: *mut LemIrq) {
    let mut queue = IRQ_LOCK.lock();
    // SAFETY: the record is live, and the lock covers the queue.
    unsafe { unlink(&mut queue, q) };
}

/// Takes the first fired record off the queue into `e`. False when none is queued.
///
/// # Safety
/// `e` is a valid, writable event.
pub(crate) unsafe extern "C" fn lem_irq_pop(e: *mut LemIrqEvent) -> bool {
    let mut queue = IRQ_LOCK.lock();
    let q = queue.head;
    if q.is_null() {
        return false;
    }
    // SAFETY: the queue's records are live under the lock.
    unsafe { unlink(&mut queue, q) };
    // SAFETY: `q` was on the queue, so it is live, and the lock is still held over its fields.
    let block_id = unsafe { (*q).block_id };
    // SAFETY: as above.
    let index = unsafe { (*q).index };
    // SAFETY: as above.
    let count = unsafe { (*q).count };
    // SAFETY: as above.
    let ns = unsafe { (*q).ns };
    // SAFETY: the event is the caller's, valid and writable.
    unsafe {
        e.write(LemIrqEvent {
            block_id,
            index,
            count,
            ns,
        })
    };
    true
}

/// True when a record is queued.
pub(crate) extern "C" fn lem_irq_any() -> bool {
    !IRQ_LOCK.lock().head.is_null()
}
