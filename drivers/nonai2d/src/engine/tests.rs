use std::cell::Cell;
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::layout::PixelFormat;

const MS: u64 = 1_000_000;

/// What the played-back engine does, shared between the test and every engine it opens.
#[derive(Default)]
pub(crate) struct Plan {
    /// Opens that fail before one succeeds.
    pub(crate) open_failures: Cell<u32>,
    /// Opens are refused as the caller's.
    pub(crate) open_refused: Cell<bool>,
    /// The error the next conversions fail with, or none.
    pub(crate) convert_error: Cell<Option<io::ErrorKind>>,
    pub(crate) opens: Cell<u32>,
    pub(crate) runs: Cell<u32>,
}

pub(crate) struct Mock {
    plan: Rc<Plan>,
    layouts: (Layout, Layout),
}

impl Engine for Mock {
    fn layouts(&self) -> (Layout, Layout) {
        self.layouts
    }

    fn convert(&mut self, _: &Frame<'_>, _: &Frame<'_>) -> io::Result<()> {
        self.plan.runs.set(self.plan.runs.get() + 1);
        match self.plan.convert_error.get() {
            Some(kind) => Err(kind.into()),
            None => Ok(()),
        }
    }
}

pub(crate) fn engine(
    plan: &Rc<Plan>,
) -> Nonai2d<Mock, impl FnMut(Layout, Layout) -> io::Result<Mock> + use<>> {
    let plan = Rc::clone(plan);
    Nonai2d::new(move |src, dst| {
        plan.opens.set(plan.opens.get() + 1);
        if plan.open_refused.get() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let left = plan.open_failures.get();
        if left > 0 {
            plan.open_failures.set(left - 1);
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Mock {
            plan: Rc::clone(&plan),
            layouts: (src, dst),
        })
    })
}

pub(crate) fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"nonai2d-test", len).expect("memfd")
}

fn rgb(format: PixelFormat) -> Layout {
    Layout::new(format, 16, 16).expect("layout")
}

/// Runs `check` with a source frame of `src` and a destination frame of `dst`.
fn with_frames(src: Layout, dst: Layout, check: impl FnOnce(&Frame<'_>, &Frame<'_>)) {
    let (a, b) = (buffer(4096), buffer(4096));
    let src = Frame::new(src, &[a.as_fd()]).expect("source");
    let dst = Frame::new(dst, &[b.as_fd()]).expect("destination");
    check(&src, &dst);
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
fn the_engine_opens_for_the_layouts_of_the_first_conversion_and_stays_open() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    assert_eq!(engine.state(), State::Closed);
    let (argb, xbgr) = (rgb(PixelFormat::Argb32), rgb(PixelFormat::Xbgr32));
    with_frames(argb, xbgr, |src, dst| {
        for now in 0..3 {
            engine.run(now, src, dst).expect("converted");
        }
    });
    assert_eq!((plan.opens.get(), plan.runs.get()), (1, 3));
    assert_eq!(engine.state(), State::Open(argb, xbgr));
}

#[test]
fn other_layouts_reopen_the_engine_without_a_backoff() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    let (argb, xbgr) = (rgb(PixelFormat::Argb32), rgb(PixelFormat::Xbgr32));
    with_frames(argb, xbgr, |src, dst| {
        engine.run(0, src, dst).expect("one way")
    });
    with_frames(xbgr, argb, |src, dst| {
        engine.run(0, src, dst).expect("the other")
    });
    assert_eq!((plan.opens.get(), plan.runs.get()), (2, 2));
    assert_eq!(engine.state(), State::Open(xbgr, argb));
}

#[test]
fn frames_of_two_sizes_are_refused_before_anything_opens() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    let small = Layout::new(PixelFormat::Argb32, 8, 8).expect("layout");
    with_frames(small, rgb(PixelFormat::Argb32), |src, dst| {
        assert!(matches!(engine.run(0, src, dst), Err(RunError::Refused(_))));
    });
    assert_eq!(plan.opens.get(), 0);
    assert_eq!(engine.state(), State::Closed);
}

#[test]
fn a_refusal_closes_the_engine_and_starts_no_backoff() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    let argb = rgb(PixelFormat::Argb32);
    with_frames(argb, argb, |src, dst| {
        plan.open_refused.set(true);
        assert!(matches!(engine.run(0, src, dst), Err(RunError::Refused(_))));
        assert_eq!(engine.state(), State::Closed);
        plan.open_refused.set(false);
        plan.convert_error.set(Some(io::ErrorKind::InvalidInput));
        assert!(matches!(engine.run(0, src, dst), Err(RunError::Refused(_))));
        assert_eq!(engine.state(), State::Closed);
        plan.convert_error.set(None);
        engine.run(0, src, dst).expect("runs at once");
    });
    assert_eq!(plan.opens.get(), 3);
}

#[test]
fn a_failure_backs_off_and_the_engine_never_gives_up() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    let argb = rgb(PixelFormat::Argb32);
    with_frames(argb, argb, |src, dst| {
        plan.open_failures.set(100);
        let mut now = 0;
        for attempt in 0..8 {
            let Err(RunError::Device { retry_at_ns, .. }) = engine.run(now, src, dst) else {
                panic!("a device failure");
            };
            assert_eq!(retry_at_ns, now + backoff_ns(attempt));
            // Before it is due the device is not touched.
            let opens = plan.opens.get();
            assert!(matches!(
                engine.run(retry_at_ns - 1, src, dst),
                Err(RunError::BackingOff { .. })
            ));
            assert_eq!(plan.opens.get(), opens);
            now = retry_at_ns;
        }
        assert_eq!(
            engine.state(),
            State::BackingOff {
                retry_at_ns: now,
                failures: 8
            }
        );
        // A conversion that fails on an open engine backs off as well.
        plan.open_failures.set(0);
        plan.convert_error.set(Some(io::ErrorKind::TimedOut));
        assert!(matches!(
            engine.run(now, src, dst),
            Err(RunError::Device { .. })
        ));
        plan.convert_error.set(None);
        now += backoff_ns(8);
        engine.run(now, src, dst).expect("back");
        assert_eq!(engine.state(), State::Open(argb, argb));
        // A success clears the backoff.
        plan.convert_error.set(Some(io::ErrorKind::TimedOut));
        let Err(RunError::Device { retry_at_ns, .. }) = engine.run(now, src, dst) else {
            panic!("a device failure");
        };
        assert_eq!(retry_at_ns, now + backoff_ns(0));
    });
}

#[test]
fn stop_closes_and_clears_the_backoff() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    let argb = rgb(PixelFormat::Argb32);
    with_frames(argb, argb, |src, dst| {
        plan.open_failures.set(1);
        assert!(engine.run(0, src, dst).is_err());
        engine.stop();
        assert_eq!(engine.state(), State::Closed);
        engine.run(0, src, dst).expect("opens at once");
    });
}

#[test]
fn conversions_on_an_open_engine_allocate_nothing() {
    let plan = Rc::new(Plan::default());
    let mut engine = engine(&plan);
    let argb = rgb(PixelFormat::Argb32);
    with_frames(argb, argb, |src, dst| {
        engine.run(0, src, dst).expect("opened");
        let (outcome, count) = allocations(|| {
            for now in 1..1001 {
                engine.run(now, src, dst)?;
            }
            Ok::<_, RunError>(())
        });
        outcome.expect("converted");
        assert_eq!(count, 0);
    });
}
