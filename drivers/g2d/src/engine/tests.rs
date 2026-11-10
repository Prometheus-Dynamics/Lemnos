use std::cell::Cell;
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::{Blend, ColorRange, Gamut, PixelFormat, Surface, Turn};

const MS: u64 = 1_000_000;
const VERSION: HardwareVersion = HardwareVersion {
    g2d_version: 0x0105_0000,
    chip_version: 7,
};

/// What the played-back engine does, shared between the test and every engine it opens.
#[derive(Default)]
struct Plan {
    /// Opens that fail before one succeeds.
    open_failures: Cell<u32>,
    /// The errno the next runs fail with, or none.
    run_errno: Cell<Option<i32>>,
    opens: Cell<u32>,
    runs: Cell<u32>,
}

struct Mock(Rc<Plan>);

impl Engine for Mock {
    fn version(&self) -> io::Result<HardwareVersion> {
        Ok(VERSION)
    }

    fn run(&self, _job: &Job<'_>) -> io::Result<()> {
        self.0.runs.set(self.0.runs.get() + 1);
        match self.0.run_errno.get() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(()),
        }
    }
}

fn engine(plan: &Rc<Plan>) -> G2d<Mock, impl FnMut() -> io::Result<Mock>> {
    let plan = Rc::clone(plan);
    G2d::new(move || {
        plan.opens.set(plan.opens.get() + 1);
        let left = plan.open_failures.get();
        if left > 0 {
            plan.open_failures.set(left - 1);
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Mock(Rc::clone(&plan)))
    })
}

fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"g2d-test", len).expect("memfd")
}

/// Runs `check` with a fill job over a small real buffer.
fn with_job(check: impl FnOnce(&Job<'_>)) {
    let b = buffer(4 * 8 * 8);
    let s = Surface::new(b.as_fd(), PixelFormat::Rgba8888, 8, 8).expect("surface");
    let job = Job::fill(&s, s.full(), 0, Gamut::Bt601, ColorRange::Full).expect("job");
    check(&job);
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
fn the_engine_opens_on_the_first_job_and_stays_open() {
    let plan = Rc::new(Plan::default());
    let mut g = engine(&plan);
    assert_eq!(g.state(), State::Closed);
    assert_eq!(plan.opens.get(), 0, "nothing opens before a job");
    with_job(|job| {
        g.run(0, job).expect("run");
        g.run(MS, job).expect("run");
    });
    assert_eq!(g.state(), State::Open);
    assert_eq!(g.version(), Some(VERSION));
    assert_eq!((plan.opens.get(), plan.runs.get()), (1, 2));
}

#[test]
fn a_refused_job_leaves_the_engine_open() {
    let plan = Rc::new(Plan::default());
    let mut g = engine(&plan);
    plan.run_errno.set(Some(libc_einval()));
    with_job(|job| {
        assert!(matches!(g.run(0, job), Err(RunError::Refused(_))));
        assert!(matches!(g.run(0, job), Err(RunError::Refused(_))));
    });
    assert_eq!(g.state(), State::Open);
    assert_eq!(plan.opens.get(), 1);
}

#[test]
fn a_failing_engine_backs_off_and_never_gives_up() {
    let plan = Rc::new(Plan::default());
    let mut g = engine(&plan);
    // A job fails with something other than a refusal: the engine closes.
    plan.run_errno.set(Some(110)); // ETIMEDOUT
    let mut now = 0;
    with_job(|job| {
        let Err(RunError::Device { retry_at_ns, .. }) = g.run(now, job) else {
            panic!("a device failure");
        };
        assert_eq!(retry_at_ns, 250 * MS);
        plan.run_errno.set(None);
        // Then the node is gone for a while; every due call tries again, later each time.
        plan.open_failures.set(u32::MAX);
        let mut due = retry_at_ns;
        for want_ms in [500, 1000, 2000, 4000, 8000, 8000, 8000, 8000] {
            // Before it is due, nothing is touched.
            let opens = plan.opens.get();
            assert!(matches!(
                g.run(due - 1, job),
                Err(RunError::BackingOff { retry_at_ns }) if retry_at_ns == due
            ));
            assert_eq!(plan.opens.get(), opens);
            now = due;
            let Err(RunError::Device { retry_at_ns, .. }) = g.run(now, job) else {
                panic!("a failed open");
            };
            assert_eq!(plan.opens.get(), opens + 1);
            assert_eq!(retry_at_ns - now, want_ms * MS);
            due = retry_at_ns;
        }
        assert!(matches!(g.state(), State::BackingOff { failures: 9, .. }));
        // The node comes back: the next due call reopens, and the backoff clears.
        plan.open_failures.set(0);
        g.run(due, job).expect("reopened");
        assert_eq!(g.state(), State::Open);
        plan.run_errno.set(Some(5)); // EIO
        let Err(RunError::Device { retry_at_ns, .. }) = g.run(due + 1, job) else {
            panic!("a device failure");
        };
        assert_eq!(retry_at_ns, due + 1 + 250 * MS, "the backoff starts over");
    });
}

#[test]
fn stop_closes_and_the_next_job_reopens_at_once() {
    let plan = Rc::new(Plan::default());
    let mut g = engine(&plan);
    with_job(|job| {
        plan.open_failures.set(1);
        assert!(matches!(g.run(0, job), Err(RunError::Device { .. })));
        g.stop();
        assert_eq!(g.state(), State::Closed);
        g.run(1, job).expect("no backoff after a stop");
    });
    assert_eq!(plan.opens.get(), 2);
}

#[test]
fn building_and_running_jobs_allocates_nothing() {
    let plan = Rc::new(Plan::default());
    let mut g = engine(&plan);
    let (a, b, d) = (
        buffer(4 * 64 * 64),
        buffer(4 * 64 * 64),
        buffer(4 * 64 * 64),
    );
    with_job(|job| g.run(0, job).expect("open"));
    let ((), allocated) = allocations(|| {
        for n in 0..1000u64 {
            let fg = Surface::new(a.as_fd(), PixelFormat::Argb8888, 64, 64).expect("surface");
            let bg = Surface::new(b.as_fd(), PixelFormat::Xrgb8888, 64, 64).expect("surface");
            let out = Surface::new(d.as_fd(), PixelFormat::Argb8888, 64, 64).expect("surface");
            let (g_, r) = (Gamut::Bt709, ColorRange::Limited);
            let jobs = [
                Job::fill(&out, out.full(), 0, g_, r),
                Job::blit(&fg, fg.full(), &out, out.full(), g_, r),
                Job::rotate(&fg, fg.full(), &out, out.full(), Turn::Cw90, true, false),
                Job::blend(
                    &fg,
                    fg.full(),
                    &bg,
                    bg.full(),
                    &out,
                    out.full(),
                    Blend::SrcOver,
                ),
            ];
            for job in jobs {
                g.run(n, &job.expect("job")).expect("run");
            }
        }
    });
    assert_eq!(allocated, 0, "surfaces, jobs and runs");
    assert_eq!(plan.runs.get(), 4001);
}

/// `EINVAL`.
fn libc_einval() -> i32 {
    22
}
