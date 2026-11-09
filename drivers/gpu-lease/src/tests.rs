use super::queue::{MAX_LEASES, Queue};
use super::*;
use lemnos_contract_gpu_lease::GpuLease as _;
use lemnos_core::{Bound, Caller, Driver, Reply};
use std::time::Duration;

/// Requests an exclusive lease with no hold limit, at `priority`.
fn excl(q: &mut Queue, priority: u8, wait: bool, now: u64) -> Result<Ticket> {
    q.acquire(false, priority, wait, 0, now)
}

#[test]
fn exclusive_lease_queues_the_next_and_is_granted_on_release() {
    let mut q = Queue::new();
    let a = excl(&mut q, 50, false, 0).unwrap();
    assert_eq!((a.lease, a.held, a.ahead), (1, true, 0));
    // Refused outright when it cannot wait; queued behind the holder when it can.
    assert_eq!(
        q.acquire(true, 50, false, 0, 0),
        Err(Status::BUSY),
        "a shared lease beside an exclusive holder is refused"
    );
    let c = excl(&mut q, 50, true, 0).unwrap();
    assert_eq!((c.held, c.ahead), (false, 1));
    assert!(!q.poll(c.lease, 0).unwrap().held);
    // The holder's release grants the waiter; the waiter sees it on its next poll.
    q.release(a.lease, 5).unwrap();
    let s = q.poll(c.lease, 5).unwrap();
    assert!(s.held);
    assert_eq!(s.for_ms, 0);
    assert_eq!(q.release(a.lease, 6), Err(Status::NOT_FOUND));
    q.release(c.lease, 7).unwrap();
    assert_eq!(q.counts(7), (0, 0));
}

#[test]
fn shared_holders_coexist_and_a_waiter_that_gives_up_leaves_the_queue() {
    let mut q = Queue::new();
    let a = q.acquire(true, 50, false, 0, 0).unwrap();
    let b = q.acquire(true, 50, false, 0, 0).unwrap();
    assert!(a.held && b.held);
    let c = excl(&mut q, 50, true, 0).unwrap();
    assert_eq!(c.ahead, 2);
    // A shared lease behind the exclusive waiter waits too: no overtaking.
    assert_eq!(q.acquire(true, 50, false, 0, 0), Err(Status::BUSY));
    assert_eq!(q.counts(0), (2, 1));
    // Giving up the place is a release.
    q.release(c.lease, 0).unwrap();
    assert_eq!(q.counts(0), (2, 0));
    assert_eq!(q.poll(c.lease, 0), Err(Status::NOT_FOUND));
}

#[test]
fn a_higher_priority_waiter_goes_first_and_asks_the_holder_to_yield() {
    let mut q = Queue::new();
    let low = excl(&mut q, 10, false, 0).unwrap();
    let mid = excl(&mut q, 50, true, 0).unwrap();
    let high = excl(&mut q, 90, true, 0).unwrap();
    assert_eq!((mid.ahead, high.ahead), (1, 1));
    assert!(q.poll(low.lease, 0).unwrap().yield_asked);
    assert!(!q.poll(mid.lease, 0).unwrap().yield_asked);
    // Holders are not asked to yield for an equal priority.
    let equal = excl(&mut q, 90, true, 0).unwrap();
    assert_eq!(
        equal.ahead, 2,
        "behind the holder and the higher waiter, ahead of the lower one"
    );
    q.release(low.lease, 1).unwrap();
    assert!(q.poll(high.lease, 1).unwrap().held);
    let mid_state = q.poll(mid.lease, 1).unwrap();
    assert!(!mid_state.held);
    assert_eq!(mid_state.ahead, 2, "behind the holder and the equal waiter");
}

#[test]
fn a_holder_past_its_time_is_marked_once_and_keeps_the_lease() {
    let mut q = Queue::new();
    let a = q.acquire(false, 50, false, 100, 1_000).unwrap();
    assert!(a.held);
    assert!(!q.poll(a.lease, 1_099).unwrap().overdue);
    let at = q.poll(a.lease, 1_100).unwrap();
    assert!(at.overdue && at.held);
    let later = q.poll(a.lease, 1_250).unwrap();
    assert!(later.overdue && later.held);
    assert_eq!((later.for_ms, later.max_hold_ms), (250, 100));
}

#[test]
fn a_full_queue_refuses_until_one_lease_ends() {
    let mut q = Queue::new();
    let first = excl(&mut q, 50, false, 0).unwrap();
    for _ in 1..MAX_LEASES {
        excl(&mut q, 50, true, 0).unwrap();
    }
    assert_eq!(excl(&mut q, 50, true, 0), Err(Status::BUSY));
    q.release(first.lease, 0).unwrap();
    assert!(excl(&mut q, 50, true, 0).is_ok());
}

#[test]
fn unknown_leases_are_not_found() {
    let mut q = Queue::new();
    assert_eq!(q.poll(99, 0), Err(Status::NOT_FOUND));
    assert_eq!(q.release(99, 0), Err(Status::NOT_FOUND));
    assert!(!q.contains(99));
}

/// A GPU device started with no config.
fn device() -> GpuLeaseDevice {
    GpuLeaseDevice::start(GpuLeaseDeviceNeeds {}, &Config::default()).unwrap()
}

#[test]
fn hangs_are_counted_and_the_last_detail_is_read_back() {
    let mut dev = device();
    let lease = dev.acquire(true, 50, false, 0).unwrap().lease;
    dev.hang(lease, "fence never signalled").unwrap();
    assert_eq!(
        dev.hang(lease + 100, "x"),
        Err(Status::NOT_FOUND),
        "a hang needs a lease the device knows"
    );
    let state = dev.state().unwrap();
    assert_eq!((state.holders, state.waiters, state.hangs), (1, 0, 1));
    assert!(state.last_hang_ms.is_some());
    let mut buf = [0u8; 64];
    let n = dev.last_hang(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"fence never signalled");
    let mut small = [0u8; 4];
    assert_eq!(dev.last_hang(&mut small), Err(Status::BUFFER_TOO_SMALL));
}

#[test]
fn a_hang_detail_is_clipped_on_a_character_boundary() {
    let mut dev = device();
    let lease = dev.acquire(true, 50, false, 0).unwrap().lease;
    // Two-byte characters: 128 bytes is 64 of them, so 127 bytes would split one.
    let long = "é".repeat(100);
    dev.hang(lease, &long).unwrap();
    let mut buf = [0u8; DETAIL_MAX];
    let n = dev.last_hang(&mut buf).unwrap();
    assert_eq!(n, DETAIL_MAX);
    assert!(std::str::from_utf8(&buf[..n]).is_ok());
}

#[test]
fn overdue_is_seen_on_the_next_poll_in_real_time() {
    let mut dev = device();
    let lease = dev.acquire(false, 50, false, 1).unwrap().lease;
    assert!(!dev.poll(lease).unwrap().overdue);
    std::thread::sleep(Duration::from_millis(20));
    let s = dev.poll(lease).unwrap();
    assert!(s.overdue && s.held);
}

/// Reaches the device the way a host does: bytes in, bytes out.
struct Direct<'a> {
    dev: &'a mut GpuLeaseDevice,
    args: [u8; 128],
}

impl Caller for Direct<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let mut writer = Reply::new(reply);
        let id = MethodId {
            contract: 0,
            method,
        };
        let status = self.dev.dispatch(id, &self.args[..args_len], &mut writer);
        if status.is_ok() {
            Ok(writer.len())
        } else {
            Err(status)
        }
    }
}

#[test]
fn a_client_reaches_the_device_through_the_contract() {
    let mut dev = device();
    let mut gpu: Bound<dyn GpuLease, Direct<'_>> = Bound::new(Direct {
        dev: &mut dev,
        args: [0; 128],
    });
    let low = gpu.acquire(false, 10, false, 0).unwrap();
    assert!(low.held);
    assert_eq!(gpu.acquire(false, 90, false, 0), Err(Status::BUSY));
    let high = gpu.acquire(false, 90, true, 0).unwrap();
    assert_eq!((high.held, high.ahead), (false, 1));
    assert!(gpu.poll(low.lease).unwrap().yield_asked);
    gpu.release(low.lease).unwrap();
    assert!(gpu.poll(high.lease).unwrap().held);
    gpu.hang(high.lease, "stalled").unwrap();
    let state = gpu.state().unwrap();
    assert_eq!((state.holders, state.waiters, state.hangs), (1, 0, 1));
    let mut detail = [0u8; 16];
    let n = gpu.last_hang(&mut detail).unwrap();
    assert_eq!(&detail[..n], b"stalled");
    assert_eq!(gpu.release(low.lease), Err(Status::NOT_FOUND));
}

#[test]
fn the_description_lists_the_state() {
    let dev = device();
    let mut out = Description::new();
    Driver::describe(&dev, &mut out);
    assert_eq!(out.find("state").unwrap().method, STATE);
}

#[test]
fn a_gone_caller_loses_its_leases_and_the_next_waiter_is_granted() {
    let mut q = Queue::new();
    q.set_caller(CallerId(1));
    let holder = excl(&mut q, 50, false, 0).unwrap();
    let also_waiting = excl(&mut q, 50, true, 0).unwrap();
    q.set_caller(CallerId(2));
    let other = excl(&mut q, 50, true, 0).unwrap();
    assert!(!q.poll(other.lease, 0).unwrap().held);

    q.caller_gone(CallerId(1), 5);

    assert!(q.poll(other.lease, 5).unwrap().held);
    assert_eq!(q.counts(5), (1, 0));
    q.set_caller(CallerId(1));
    assert_eq!(q.poll(holder.lease, 5), Err(Status::NOT_FOUND));
    assert_eq!(q.poll(also_waiting.lease, 5), Err(Status::NOT_FOUND));
}

#[test]
fn a_lease_belongs_to_the_caller_that_took_it() {
    let mut q = Queue::new();
    q.set_caller(CallerId(1));
    let lease = excl(&mut q, 50, false, 0).unwrap().lease;
    q.set_caller(CallerId(2));
    assert_eq!(q.poll(lease, 0), Err(Status::NOT_FOUND));
    assert_eq!(q.release(lease, 0), Err(Status::NOT_FOUND));
    assert!(!q.contains(lease));
    q.set_caller(CallerId(1));
    assert!(q.poll(lease, 0).unwrap().held);
}
