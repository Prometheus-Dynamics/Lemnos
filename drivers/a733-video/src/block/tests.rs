use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use super::*;

const MS: u64 = 1_000_000;
const PAGE: u64 = 4096;
/// What the kernel answers a descriptor it can't attach with.
const EFAULT: i32 = 14;

/// What the played-back engine does, shared between a test and every file it opens.
#[derive(Default)]
struct Plan {
    open_failures: Cell<u32>,
    opens: Cell<u32>,
    /// Users counted by the kernel: requests less releases.
    users: Cell<i32>,
    locks: Cell<u32>,
    /// The bus address the next map answers; each map moves it on by a page-rounded megabyte.
    next_bus: Cell<u32>,
    /// The errno the next map fails with, once.
    map_errno: Cell<Option<i32>>,
    /// The errno every wait fails with.
    wait_errno: Cell<Option<i32>>,
    /// Whether waits answer that the interrupt came.
    interrupt: Cell<bool>,
    /// Descriptor numbers mapped and not unmapped.
    mapped: RefCell<Vec<RawFd>>,
    unmaps: Cell<u32>,
    waited_seconds: Cell<u32>,
    resets: Cell<u32>,
}

struct Mock(Rc<Plan>);

impl Device for Mock {
    fn request_engine(&self) -> io::Result<()> {
        self.0.users.set(self.0.users.get() + 1);
        Ok(())
    }

    fn release_engine(&self) -> io::Result<()> {
        self.0.users.set(self.0.users.get() - 1);
        Ok(())
    }

    fn lock(&self, _lock: Lock) -> io::Result<()> {
        self.0.locks.set(self.0.locks.get() + 1);
        Ok(())
    }

    fn unlock(&self, _lock: Lock) -> io::Result<()> {
        self.0.locks.set(self.0.locks.get() - 1);
        Ok(())
    }

    fn reset(&self) -> io::Result<()> {
        self.0.resets.set(self.0.resets.get() + 1);
        Ok(())
    }

    fn set_mode(&self, _mode: Mode) -> io::Result<()> {
        Ok(())
    }

    fn map(&self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        if let Some(errno) = self.0.map_errno.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        self.0.mapped.borrow_mut().push(buffer.as_raw_fd());
        let bus = self.0.next_bus.get();
        self.0.next_bus.set(bus.wrapping_add(1 << 20));
        Ok(bus)
    }

    fn unmap(&self, buffer: BorrowedFd<'_>) -> io::Result<()> {
        self.0.unmaps.set(self.0.unmaps.get() + 1);
        let mut mapped = self.0.mapped.borrow_mut();
        let at = mapped
            .iter()
            .position(|&fd| fd == buffer.as_raw_fd())
            .expect("unmapped a descriptor that was never mapped");
        mapped.swap_remove(at);
        Ok(())
    }

    fn clear_completion(&self) -> io::Result<()> {
        Ok(())
    }

    fn wait(&self, _completion: Completion, seconds: u32) -> io::Result<bool> {
        self.0.waited_seconds.set(seconds);
        match self.0.wait_errno.get() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(self.0.interrupt.get()),
        }
    }

    fn read(&self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= uapi::WINDOW_BYTES);
        0x5a00_0000 | offset as u32
    }
}

fn plan() -> Rc<Plan> {
    let plan = Plan::default();
    plan.next_bus.set(0x4000_0000);
    plan.interrupt.set(true);
    Rc::new(plan)
}

fn block(plan: &Rc<Plan>) -> Block<Mock, impl FnMut() -> io::Result<Mock>> {
    let plan = Rc::clone(plan);
    Block::new(move || {
        plan.opens.set(plan.opens.get() + 1);
        let left = plan.open_failures.get();
        if left > 0 {
            plan.open_failures.set(left - 1);
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Mock(Rc::clone(&plan)))
    })
}

/// A descriptor with a real, fixed size and no device behind it.
fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"ve-test", len).expect("memfd")
}

#[test]
fn the_backoff_doubles_from_250_ms_to_8_s() {
    let delays: Vec<u64> = (0..9).map(backoff_ns).collect();
    assert_eq!(
        delays,
        [250, 500, 1000, 2000, 4000, 8000, 8000, 8000, 8000].map(|ms| ms * MS)
    );
    assert_eq!(backoff_ns(u32::MAX), 8000 * MS);
}

#[test]
fn a_claim_counts_a_user_takes_the_lock_and_release_gives_both_back() {
    let plan = plan();
    let mut ve = block(&plan);
    assert_eq!(ve.state(), State::Closed);
    ve.claim(0, Lock::Encoder).unwrap();
    assert_eq!(ve.state(), State::Claimed(Lock::Encoder));
    assert_eq!(
        (plan.opens.get(), plan.users.get(), plan.locks.get()),
        (1, 1, 1)
    );
    assert!(matches!(
        ve.claim(0, Lock::Decoder),
        Err(Error::AlreadyClaimed(Lock::Encoder))
    ));
    let b = buffer(PAGE);
    ve.attach(0, b.as_fd()).unwrap();
    ve.release(0).unwrap();
    assert_eq!(ve.state(), State::Open);
    assert_eq!((plan.users.get(), plan.locks.get()), (0, 0));
    assert!(plan.mapped.borrow().is_empty(), "release detaches");
    // The file stays open for the next claim.
    ve.claim(0, Lock::Encoder).unwrap();
    assert_eq!(plan.opens.get(), 1);
    drop(ve);
    assert_eq!(
        (plan.users.get(), plan.locks.get()),
        (0, 0),
        "drop gives both back"
    );
}

#[test]
fn nothing_reaches_the_engine_unclaimed() {
    let plan = plan();
    let mut ve = block(&plan);
    let b = buffer(PAGE);
    assert!(matches!(ve.attach(0, b.as_fd()), Err(Error::NotClaimed)));
    assert!(matches!(ve.read(0), Err(Error::NotClaimed)));
    assert!(matches!(
        ve.wait(0, Completion::Encoder, 1),
        Err(Error::NotClaimed)
    ));
    assert!(matches!(ve.reset(0), Err(Error::NotClaimed)));
    assert!(matches!(
        ve.set_mode(0, Mode::Encoder),
        Err(Error::NotClaimed)
    ));
    assert!(matches!(ve.clear_completion(0), Err(Error::NotClaimed)));
    assert_eq!(plan.opens.get(), 0);
    // An id from a claim that ended is unknown to the next one.
    ve.claim(0, Lock::Encoder).unwrap();
    let id = ve.attach(0, b.as_fd()).unwrap();
    ve.release(0).unwrap();
    assert!(matches!(ve.address(id, 0, 1), Err(Error::NotClaimed)));
    ve.claim(0, Lock::Encoder).unwrap();
    assert!(matches!(ve.address(id, 0, 1), Err(Error::UnknownBuffer)));
}

#[test]
fn an_address_is_handed_out_only_for_a_range_inside_a_held_buffer() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    let b = buffer(3 * PAGE);
    let id = ve.attach(0, b.as_fd()).unwrap();
    assert_eq!(ve.buffer_len(id).unwrap(), 3 * PAGE);
    assert_eq!(ve.address(id, 0, 3 * PAGE).unwrap(), 0x4000_0000);
    assert_eq!(
        ve.address(id, 3 * PAGE - 1, 1).unwrap(),
        0x4000_0000 + 3 * 4096 - 1
    );
    // One byte past the end, an empty range, and offsets that overflow.
    assert!(matches!(
        ve.address(id, 3 * PAGE, 1),
        Err(Error::OutOfBuffer)
    ));
    assert!(matches!(
        ve.address(id, 1, 3 * PAGE),
        Err(Error::OutOfBuffer)
    ));
    assert!(matches!(ve.address(id, 0, 0), Err(Error::OutOfBuffer)));
    assert!(matches!(
        ve.address(id, u64::MAX, 2),
        Err(Error::OutOfBuffer)
    ));
    assert!(matches!(
        ve.address(id, 1, u64::MAX),
        Err(Error::OutOfBuffer)
    ));
    // A second buffer's id names only that buffer.
    let small = buffer(PAGE);
    let other = ve.attach(0, small.as_fd()).unwrap();
    assert_eq!(ve.address(other, 0, PAGE).unwrap(), 0x4010_0000);
    assert!(matches!(
        ve.address(other, 0, PAGE + 1),
        Err(Error::OutOfBuffer)
    ));
    // Detached: stale, even once its slot holds another buffer.
    ve.detach(0, other).unwrap();
    assert!(matches!(ve.address(other, 0, 1), Err(Error::UnknownBuffer)));
    let again = ve.attach(0, small.as_fd()).unwrap();
    assert!(matches!(ve.address(other, 0, 1), Err(Error::UnknownBuffer)));
    assert!(ve.address(again, 0, 1).is_ok());
    assert!(matches!(ve.detach(0, other), Err(Error::UnknownBuffer)));
}

#[test]
fn a_buffer_whose_bus_range_would_pass_32_bits_is_refused_and_detached() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    plan.next_bus.set(u32::MAX - 4095);
    let fits = buffer(PAGE);
    let id = ve.attach(0, fits.as_fd()).unwrap();
    assert_eq!(ve.address(id, PAGE - 1, 1).unwrap(), u32::MAX);
    plan.next_bus.set(u32::MAX - 4095);
    let past = buffer(PAGE + 1);
    assert!(matches!(ve.attach(0, past.as_fd()), Err(Error::Refused(_))));
    assert_eq!(
        plan.mapped.borrow().len(),
        1,
        "the refused one was detached"
    );
    assert_eq!(ve.state(), State::Claimed(Lock::Encoder));
}

#[test]
fn an_empty_or_unattachable_buffer_is_refused_and_the_engine_stays_claimed() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    let empty = buffer(0);
    assert!(matches!(
        ve.attach(0, empty.as_fd()),
        Err(Error::Refused(_))
    ));
    assert!(
        plan.mapped.borrow().is_empty(),
        "never offered to the kernel"
    );
    plan.map_errno.set(Some(EFAULT));
    let b = buffer(PAGE);
    assert!(matches!(ve.attach(0, b.as_fd()), Err(Error::Refused(_))));
    assert_eq!(ve.state(), State::Claimed(Lock::Encoder));
    assert!(ve.attach(0, b.as_fd()).is_ok());
}

#[test]
fn the_driver_maps_its_own_descriptor_and_detaches_that_one() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    let b = buffer(PAGE);
    let caller = b.as_raw_fd();
    let id = ve.attach(0, b.as_fd()).unwrap();
    let mapped = plan.mapped.borrow()[0];
    assert_ne!(mapped, caller, "a duplicate the driver holds");
    // The caller's descriptor closes; the number the kernel knows stays the buffer's.
    drop(b);
    assert!(ve.address(id, 0, PAGE).is_ok());
    ve.detach(0, id).unwrap();
    assert!(plan.mapped.borrow().is_empty());
}

#[test]
fn the_table_holds_a_fixed_number_of_buffers() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    let b = buffer(PAGE);
    plan.next_bus.set(0);
    let ids: Vec<BufferId> = (0..MAX_BUFFERS)
        .map(|_| ve.attach(0, b.as_fd()).unwrap())
        .collect();
    assert!(matches!(ve.attach(0, b.as_fd()), Err(Error::Full)));
    ve.detach(0, ids[7]).unwrap();
    assert!(ve.attach(0, b.as_fd()).is_ok());
}

#[test]
fn registers_are_read_only_inside_the_window_on_word_boundaries() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    assert_eq!(ve.read(0).unwrap(), 0x5a00_0000);
    assert_eq!(ve.read(uapi::ENC_INT_STATUS).unwrap(), 0x5a00_0b1c);
    assert_eq!(ve.read(uapi::WINDOW_BYTES - 4).unwrap(), 0x5a00_0ffc);
    for offset in [
        uapi::WINDOW_BYTES,
        uapi::WINDOW_BYTES - 3,
        2,
        usize::MAX - 1,
        usize::MAX - 3,
    ] {
        assert!(
            matches!(ve.read(offset), Err(Error::OutOfWindow)),
            "{offset:#x}"
        );
    }
}

#[test]
fn a_timeout_keeps_the_claim_and_the_wait_is_clamped() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    assert!(ve.wait(0, Completion::Encoder, 2).unwrap());
    assert_eq!(plan.waited_seconds.get(), 2);
    plan.interrupt.set(false);
    assert!(!ve.wait(0, Completion::Encoder, 0).unwrap());
    assert_eq!(plan.waited_seconds.get(), 1);
    ve.wait(0, Completion::VcuEncoder, u32::MAX).unwrap();
    assert_eq!(plan.waited_seconds.get(), MAX_WAIT_S);
    assert_eq!(ve.state(), State::Claimed(Lock::Encoder));
    ve.reset(0).unwrap();
    assert_eq!(plan.resets.get(), 1);
}

#[test]
fn a_failure_tears_down_backs_off_and_reopens_when_due() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    let b = buffer(PAGE);
    let id = ve.attach(0, b.as_fd()).unwrap();
    plan.wait_errno.set(Some(5));
    let Err(Error::Device { retry_at_ns, .. }) = ve.wait(100, Completion::Encoder, 1) else {
        panic!("a failed wait closes the engine");
    };
    assert_eq!(retry_at_ns, 100 + 250 * MS);
    assert!(plan.mapped.borrow().is_empty(), "buffers detached");
    assert_eq!((plan.users.get(), plan.locks.get()), (0, 0));
    assert_eq!(
        ve.state(),
        State::BackingOff {
            retry_at_ns,
            failures: 1
        }
    );
    assert!(matches!(ve.address(id, 0, 1), Err(Error::NotClaimed)));
    // Not yet due: the device isn't touched.
    assert!(matches!(
        ve.claim(retry_at_ns - 1, Lock::Encoder),
        Err(Error::BackingOff { .. })
    ));
    assert_eq!(plan.opens.get(), 1);
    // Due, and the node is gone for a while: the delay doubles to the cap and holds.
    plan.wait_errno.set(None);
    plan.open_failures.set(7);
    let mut now = retry_at_ns;
    let mut delays = Vec::new();
    loop {
        match ve.claim(now, Lock::Encoder) {
            Ok(()) => break,
            Err(Error::Device { retry_at_ns, .. }) => {
                delays.push((retry_at_ns - now) / MS);
                now = retry_at_ns;
            }
            Err(other) => panic!("{other}"),
        }
    }
    assert_eq!(delays, [500, 1000, 2000, 4000, 8000, 8000, 8000]);
    assert_eq!(ve.state(), State::Claimed(Lock::Encoder));
    assert!(matches!(ve.address(id, 0, 1), Err(Error::UnknownBuffer)));
    let fresh = ve.attach(now, b.as_fd()).unwrap();
    assert!(ve.address(fresh, 0, 1).is_ok());
}

#[test]
fn stop_gives_everything_back_and_the_next_claim_opens_again() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Jpeg).unwrap();
    let b = buffer(PAGE);
    ve.attach(0, b.as_fd()).unwrap();
    ve.stop();
    assert_eq!(ve.state(), State::Closed);
    assert!(plan.mapped.borrow().is_empty());
    assert_eq!((plan.users.get(), plan.locks.get()), (0, 0));
    ve.claim(0, Lock::Jpeg).unwrap();
    assert_eq!(plan.opens.get(), 2);
}

#[test]
fn the_steady_state_allocates_nothing() {
    let plan = plan();
    let mut ve = block(&plan);
    ve.claim(0, Lock::Encoder).unwrap();
    let b = buffer(PAGE);
    // The mock's own list of mapped descriptors is grown before counting.
    plan.mapped.borrow_mut().reserve(8);
    let ((), count) = allocations(|| {
        for _ in 0..4 {
            let id = ve.attach(0, b.as_fd()).unwrap();
            let address = ve.address(id, 16, 64).unwrap();
            assert_eq!(address & 0xfff, 16);
            assert!(ve.read(uapi::ENC_INT_STATUS).is_ok());
            assert!(ve.wait(0, Completion::Encoder, 1).unwrap());
            assert!(ve.address(id, PAGE, 1).is_err());
            assert!(ve.read(uapi::WINDOW_BYTES).is_err());
            ve.clear_completion(0).unwrap();
            ve.detach(0, id).unwrap();
        }
    });
    assert_eq!(count, 0);
}

#[test]
fn the_lock_and_mode_values_are_the_kernels() {
    assert_eq!(
        [Lock::Decoder, Lock::Encoder, Lock::Jpeg].map(Lock::bit),
        [1, 2, 4]
    );
    assert_eq!(
        [
            Mode::EncoderPreprocess,
            Mode::Encoder,
            Mode::Decoder,
            Mode::VcuEncoder,
            Mode::VcuDecoder
        ]
        .map(Mode::value),
        [0, 1, 2, 3, 4]
    );
    assert_eq!(
        [
            Completion::Decoder,
            Completion::Encoder,
            Completion::VcuEncoder,
            Completion::VcuDecoder
        ]
        .map(Completion::request),
        [0x102, 0x103, 0x801, 0x805]
    );
}
