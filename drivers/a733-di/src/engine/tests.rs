use std::cell::{Cell, RefCell};
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::{Field, Format, Frame, Layout, Mode};

const MS: u64 = 1_000_000;
const EINVAL: i32 = 22;
const EIO: i32 = 5;
const VERSION: uapi::Version = uapi::Version {
    major: 1,
    minor: 0,
    patchlevel: 0,
    ip_version: 0x301,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Reset,
    Configure(Settings),
    Check,
    Process,
}

#[derive(Default)]
struct Plan {
    open_failures: Cell<u32>,
    opens: Cell<u32>,
    ops: RefCell<Vec<Op>>,
    check_errno: Cell<Option<i32>>,
    process_errno: Cell<Option<i32>>,
    progressive: Cell<bool>,
}

struct Mock(Rc<Plan>);

impl Device for Mock {
    fn version(&self) -> io::Result<uapi::Version> {
        Ok(VERSION)
    }

    fn reset(&self) -> io::Result<()> {
        self.0.ops.borrow_mut().push(Op::Reset);
        Ok(())
    }

    fn configure(&self, settings: &Settings) -> io::Result<()> {
        self.0.ops.borrow_mut().push(Op::Configure(*settings));
        Ok(())
    }

    fn check(&self) -> io::Result<()> {
        self.0.ops.borrow_mut().push(Op::Check);
        match self.0.check_errno.get() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(()),
        }
    }

    fn process(&self, job: &Job<'_>) -> io::Result<i32> {
        assert!(job.request().in_fb0.dma_buf_fd >= 0);
        self.0.ops.borrow_mut().push(Op::Process);
        match self.0.process_errno.get() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(i32::from(self.0.progressive.get())),
        }
    }
}

fn engine(plan: &Rc<Plan>) -> Deinterlacer<Mock, impl FnMut() -> io::Result<Mock>> {
    let plan = Rc::clone(plan);
    Deinterlacer::new(move || {
        plan.opens.set(plan.opens.get() + 1);
        let left = plan.open_failures.get();
        if left > 0 {
            plan.open_failures.set(left - 1);
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Mock(Rc::clone(&plan)))
    })
}

const W: u32 = 64;
const H: u32 = 32;
const LAYOUT: Layout = Layout {
    luma_offset: 0,
    luma_stride: W,
    chroma_offset: (W * H) as u64,
    chroma2_offset: 0,
    chroma_stride: W,
};

fn buffers() -> [OwnedFd; 2] {
    [(); 2].map(|()| dma_buf::sealed_memfd(c"di-test", u64::from(W * H) * 3 / 2).unwrap())
}

/// Runs `check` with a bob job and a weave job over two real buffers.
fn with_jobs(check: impl FnOnce(&Job<'_>, &Job<'_>)) {
    let fds = buffers();
    let input = Frame::new(fds[0].as_fd(), Format::Nv12, W, H, LAYOUT).unwrap();
    let output = Frame::new(fds[1].as_fd(), Format::Nv12, W, H, LAYOUT).unwrap();
    let bob = Config::new(W, H, Mode::Bob).unwrap();
    let weave = Config::new(W, H, Mode::Weave).unwrap();
    let a = Job::new(bob, true, Field::Top, &[input], &[output]).unwrap();
    let b = Job::new(weave, true, Field::Top, &[input], &[output]).unwrap();
    check(&a, &b);
}

fn ops(plan: &Plan) -> Vec<Op> {
    plan.ops.borrow_mut().drain(..).collect()
}

#[test]
fn the_backoff_doubles_from_250_ms_to_8_s() {
    let delays: Vec<u64> = (0..9).map(backoff_ns).collect();
    assert_eq!(
        delays,
        [250, 500, 1000, 2000, 4000, 8000, 8000, 8000, 8000].map(|ms| ms * MS)
    );
}

#[test]
fn settings_are_applied_from_a_reset_only_when_they_change() {
    let plan = Rc::new(Plan::default());
    let mut di = engine(&plan);
    with_jobs(|bob, weave| {
        assert_eq!(di.state(), State::Closed);
        assert!(!di.run(0, bob).unwrap().progressive);
        assert_eq!(di.version(), Some(VERSION));
        let settings = bob.config().settings();
        assert_eq!(
            ops(&plan),
            [Op::Reset, Op::Configure(settings), Op::Check, Op::Process]
        );
        di.run(0, bob).unwrap();
        assert_eq!(ops(&plan), [Op::Process]);
        di.run(0, weave).unwrap();
        assert_eq!(
            ops(&plan),
            [
                Op::Reset,
                Op::Configure(weave.config().settings()),
                Op::Check,
                Op::Process
            ]
        );
        assert_eq!(plan.opens.get(), 1);
        plan.progressive.set(true);
        assert!(di.run(0, weave).unwrap().progressive);
    });
}

#[test]
fn a_refusal_keeps_the_engine_open_and_reapplies_the_settings() {
    let plan = Rc::new(Plan::default());
    let mut di = engine(&plan);
    with_jobs(|bob, _| {
        plan.process_errno.set(Some(EINVAL));
        assert!(matches!(di.run(0, bob), Err(RunError::Refused(_))));
        assert_eq!(di.state(), State::Open);
        plan.process_errno.set(None);
        ops(&plan);
        di.run(0, bob).unwrap();
        assert_eq!(ops(&plan)[0], Op::Reset, "applied afresh");
    });
    // A refused check: nothing is processed, and the next job starts from a reset.
    with_jobs(|_, weave| {
        plan.check_errno.set(Some(EINVAL));
        ops(&plan);
        assert!(matches!(di.run(0, weave), Err(RunError::Refused(_))));
        assert!(!ops(&plan).contains(&Op::Process));
        plan.check_errno.set(None);
        di.run(0, weave).unwrap();
        assert_eq!(ops(&plan)[0], Op::Reset);
        assert_eq!(plan.opens.get(), 1);
    });
}

#[test]
fn a_failure_closes_backs_off_and_reopens_when_due() {
    let plan = Rc::new(Plan::default());
    let mut di = engine(&plan);
    with_jobs(|bob, _| {
        di.run(0, bob).unwrap();
        plan.process_errno.set(Some(EIO));
        let Err(RunError::Device { retry_at_ns, .. }) = di.run(100, bob) else {
            panic!("a failed job closes the engine");
        };
        assert_eq!(retry_at_ns, 100 + 250 * MS);
        assert!(matches!(
            di.run(retry_at_ns - 1, bob),
            Err(RunError::BackingOff { .. })
        ));
        assert_eq!(plan.opens.get(), 1);
        plan.process_errno.set(None);
        plan.open_failures.set(6);
        let mut now = retry_at_ns;
        let mut delays = Vec::new();
        loop {
            match di.run(now, bob) {
                Ok(_) => break,
                Err(RunError::Device { retry_at_ns, .. }) => {
                    delays.push((retry_at_ns - now) / MS);
                    now = retry_at_ns;
                }
                Err(other) => panic!("{other}"),
            }
        }
        assert_eq!(delays, [500, 1000, 2000, 4000, 8000, 8000]);
        assert_eq!(di.state(), State::Open);
        di.stop();
        assert_eq!(di.state(), State::Closed);
        ops(&plan);
        di.run(now, bob).unwrap();
        assert_eq!(
            ops(&plan)[0],
            Op::Reset,
            "a reopened file starts from a reset"
        );
    });
}

#[test]
fn a_job_allocates_nothing() {
    let plan = Rc::new(Plan::default());
    let mut di = engine(&plan);
    with_jobs(|bob, weave| {
        di.run(0, bob).unwrap();
        plan.ops.borrow_mut().reserve(64);
        let ((), count) = allocations(|| {
            for _ in 0..4 {
                di.run(0, bob).unwrap();
                di.run(0, weave).unwrap();
            }
        });
        assert_eq!(count, 0);
    });
}
