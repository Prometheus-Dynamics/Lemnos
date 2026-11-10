use std::cell::{Cell, RefCell};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::rc::Rc;

use lemnos_measure::test_alloc::allocations;

use super::*;

const MS: u64 = 1_000_000;

pub(crate) const RGB: Format = Format {
    fourcc: u32::from_le_bytes(*b"RG24"),
    bytes_per_pixel: 3,
};
pub(crate) const LIMITS: Limits = Limits {
    max_width: 1024,
    max_height: 1024,
};

/// What the played-back block does and saw, shared between the test and every device it opens.
#[derive(Default)]
pub(crate) struct Plan {
    /// Opens that fail before one succeeds.
    pub(crate) open_failures: Cell<u32>,
    /// The next import fails.
    pub(crate) import_fails: Cell<bool>,
    /// The next run fails with this errno.
    pub(crate) run_errno: Cell<Option<i32>>,
    /// Every import answers this address, when set.
    pub(crate) same_address: Cell<Option<u64>>,
    pub(crate) opens: Cell<u32>,
    pub(crate) runs: Cell<u32>,
    /// The descriptors imported and not yet removed, by the devices still open.
    pub(crate) live: RefCell<Vec<i32>>,
    /// The last job run.
    pub(crate) last: Cell<Option<Job>>,
}

pub(crate) struct Mock {
    plan: Rc<Plan>,
    next_address: u64,
}

impl Drop for Mock {
    fn drop(&mut self) {
        // Closing the node drops every import.
        self.plan.live.borrow_mut().clear();
    }
}

impl Device for Mock {
    const FORMATS: &'static [Format] = &[RGB];
    const FILTERS: &'static [Filter] = &[Filter::Bilinear];

    fn import(&mut self, dmabuf: BorrowedFd<'_>) -> io::Result<u64> {
        if self.plan.import_fails.replace(false) {
            return Err(io::Error::from_raw_os_error(1));
        }
        self.plan.live.borrow_mut().push(dmabuf.as_raw_fd());
        let address = self.plan.same_address.get().unwrap_or(self.next_address);
        self.next_address += 1 << 24;
        Ok(address)
    }

    fn remove(&mut self, dmabuf: BorrowedFd<'_>) -> io::Result<()> {
        let mut live = self.plan.live.borrow_mut();
        let at = live
            .iter()
            .position(|fd| *fd == dmabuf.as_raw_fd())
            .ok_or_else(|| io::Error::from_raw_os_error(1))?;
        live.swap_remove(at);
        Ok(())
    }

    fn run(&mut self, job: &Job) -> io::Result<()> {
        self.plan.runs.set(self.plan.runs.get() + 1);
        self.plan.last.set(Some(*job));
        match self.plan.run_errno.get() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(()),
        }
    }
}

pub(crate) fn block(plan: &Rc<Plan>) -> Ai2d<Mock, impl FnMut() -> io::Result<Mock> + use<>> {
    let plan = Rc::clone(plan);
    Ai2d::new(
        move || {
            plan.opens.set(plan.opens.get() + 1);
            let left = plan.open_failures.get();
            if left > 0 {
                plan.open_failures.set(left - 1);
                return Err(io::ErrorKind::NotFound.into());
            }
            Ok(Mock {
                plan: Rc::clone(&plan),
                next_address: 0x1000_0000,
            })
        },
        LIMITS,
    )
}

pub(crate) fn buffer(len: u64) -> OwnedFd {
    lemnos_linux_sys::dma_buf::sealed_memfd(c"ai2d-test", len).expect("memfd")
}

pub(crate) fn shape(width: u32, height: u32) -> Shape {
    Shape {
        width,
        height,
        stride: width * 3,
        format: RGB,
    }
}

fn request(dmabuf: &OwnedFd, width: u32, height: u32) -> Request<'_> {
    Request {
        dmabuf: dmabuf.as_fd(),
        shape: shape(width, height),
    }
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
fn a_resize_runs_on_the_imported_addresses_and_removes_both_imports() {
    let plan = Rc::new(Plan::default());
    let mut ai2d = block(&plan);
    let (input, output) = (buffer(64 * 64 * 3), buffer(32 * 32 * 3));
    let bytes = ai2d
        .resize(
            5,
            request(&input, 64, 64),
            request(&output, 32, 32),
            Filter::Bilinear,
        )
        .expect("resized");
    assert_eq!(bytes, 32 * 32 * 3);
    let job = plan.last.get().expect("a job");
    assert_eq!(
        (job.input().address(), job.input().bytes()),
        (0x1000_0000, 64 * 64 * 3)
    );
    assert_eq!(
        (job.output().address(), job.output().bytes()),
        (0x1100_0000, 32 * 32 * 3)
    );
    assert!(plan.live.borrow().is_empty(), "both imports removed");
    assert_eq!(ai2d.state(), State::Open);
}

#[test]
fn what_the_checks_refuse_never_reaches_the_block() {
    let plan = Rc::new(Plan::default());
    let mut ai2d = block(&plan);
    let (input, output) = (buffer(4096), buffer(4096));
    let refused = |ai2d: &mut Ai2d<_, _>, input: Request<'_>, output: Request<'_>, filter, why| {
        let outcome = ai2d.resize(0, input, output, filter);
        assert!(
            matches!(outcome, Err(RunError::Refused(e)) if e == why),
            "{outcome:?}"
        );
    };
    // One row more than the dma-buf holds: 37 by 37 at three bytes is 4107.
    refused(
        &mut ai2d,
        request(&input, 37, 37),
        request(&output, 8, 8),
        Filter::Bilinear,
        JobError::Extent {
            need: 4107,
            have: 4096,
        },
    );
    refused(
        &mut ai2d,
        request(&input, 8, 8),
        request(&output, 37, 37),
        Filter::Bilinear,
        JobError::Extent {
            need: 4107,
            have: 4096,
        },
    );
    // A padded stride counts on every line.
    let mut padded = request(&input, 16, 16);
    padded.shape.stride = 4096 / 16 + 1;
    refused(
        &mut ai2d,
        padded,
        request(&output, 8, 8),
        Filter::Bilinear,
        JobError::Extent {
            need: 16 * 257,
            have: 4096,
        },
    );
    let mut narrow = request(&input, 16, 16);
    narrow.shape.stride = 47;
    refused(
        &mut ai2d,
        narrow,
        request(&output, 8, 8),
        Filter::Bilinear,
        JobError::Stride,
    );
    refused(
        &mut ai2d,
        request(&input, 0, 8),
        request(&output, 8, 8),
        Filter::Bilinear,
        JobError::Size,
    );
    refused(
        &mut ai2d,
        request(&input, 1025, 1),
        request(&output, 8, 8),
        Filter::Bilinear,
        JobError::Size,
    );
    refused(
        &mut ai2d,
        request(&input, 8, 8),
        request(&output, 8, 8),
        Filter::Area,
        JobError::Format,
    );
    let mut argb = request(&input, 8, 8);
    argb.shape.format = Format {
        fourcc: u32::from_le_bytes(*b"AR24"),
        bytes_per_pixel: 4,
    };
    argb.shape.stride = 32;
    refused(
        &mut ai2d,
        argb,
        request(&output, 8, 8),
        Filter::Bilinear,
        JobError::Format,
    );
    assert_eq!((plan.opens.get(), plan.runs.get()), (0, 0));
    // A descriptor that isn't a dma-buf is refused by the kernel's own answer.
    let (pipe, _writer) = std::io::pipe().expect("pipe");
    let not_a_buffer = Request {
        dmabuf: pipe.as_fd(),
        shape: shape(8, 8),
    };
    assert!(matches!(
        ai2d.resize(0, not_a_buffer, request(&output, 8, 8), Filter::Bilinear),
        Err(RunError::Unsized(_))
    ));
    assert_eq!(plan.opens.get(), 0);
}

#[test]
fn addresses_that_overlap_are_refused_after_import_and_both_are_removed() {
    let plan = Rc::new(Plan::default());
    let mut ai2d = block(&plan);
    plan.same_address.set(Some(0x2000_0000));
    let (input, output) = (buffer(4096), buffer(4096));
    let outcome = ai2d.resize(
        0,
        request(&input, 8, 8),
        request(&output, 8, 8),
        Filter::Bilinear,
    );
    assert!(matches!(outcome, Err(RunError::Refused(JobError::Overlap))));
    assert_eq!(plan.runs.get(), 0);
    assert!(plan.live.borrow().is_empty());
    assert_eq!(ai2d.state(), State::Open, "the caller's, not the block's");
    // An address the block can't add the image to.
    plan.same_address.set(Some(u64::MAX - 10));
    let outcome = ai2d.resize(
        0,
        request(&input, 8, 8),
        request(&output, 8, 8),
        Filter::Bilinear,
    );
    assert!(matches!(outcome, Err(RunError::Refused(JobError::Address))));
    assert!(plan.live.borrow().is_empty());
}

#[test]
fn a_failure_closes_the_block_drops_its_imports_and_backs_off() {
    let plan = Rc::new(Plan::default());
    let mut ai2d = block(&plan);
    let (input, output) = (buffer(4096), buffer(4096));
    let go = |ai2d: &mut Ai2d<_, _>, now| {
        ai2d.resize(
            now,
            request(&input, 8, 8),
            request(&output, 8, 8),
            Filter::Bilinear,
        )
    };
    go(&mut ai2d, 0).expect("runs");
    plan.run_errno.set(Some(110)); // ETIMEDOUT
    let Err(RunError::Device { retry_at_ns, .. }) = go(&mut ai2d, 0) else {
        panic!("a device failure");
    };
    assert_eq!(retry_at_ns, backoff_ns(0));
    assert!(plan.live.borrow().is_empty());
    assert!(matches!(
        go(&mut ai2d, retry_at_ns - 1),
        Err(RunError::BackingOff { .. })
    ));
    assert_eq!(plan.opens.get(), 1);
    plan.run_errno.set(None);
    plan.import_fails.set(true);
    let Err(RunError::Device { retry_at_ns, .. }) = go(&mut ai2d, retry_at_ns) else {
        panic!("a device failure");
    };
    assert_eq!(retry_at_ns, backoff_ns(0) + backoff_ns(1));
    assert!(plan.live.borrow().is_empty());
    // Opening fails for a while; the backoff holds at 8 s and it never gives up.
    plan.open_failures.set(8);
    let mut now = retry_at_ns;
    for attempt in 2..10 {
        let Err(RunError::Device { retry_at_ns, .. }) = go(&mut ai2d, now) else {
            panic!("a device failure");
        };
        assert_eq!(retry_at_ns, now + backoff_ns(attempt));
        now = retry_at_ns;
    }
    assert_eq!(
        ai2d.state(),
        State::BackingOff {
            retry_at_ns: now,
            failures: 10
        }
    );
    go(&mut ai2d, now).expect("back");
    assert_eq!(ai2d.state(), State::Open);
    ai2d.stop();
    assert_eq!(ai2d.state(), State::Closed);
}

#[test]
fn resizes_on_an_open_block_allocate_nothing() {
    let plan = Rc::new(Plan::default());
    let mut ai2d = block(&plan);
    let (input, output) = (buffer(4096), buffer(4096));
    ai2d.resize(
        0,
        request(&input, 8, 8),
        request(&output, 8, 8),
        Filter::Bilinear,
    )
    .expect("opened");
    // The mock's own list of imports is sized before counting.
    plan.live.borrow_mut().reserve(8);
    let (outcome, count) = allocations(|| {
        for now in 1..1001 {
            ai2d.resize(
                now,
                request(&input, 8, 8),
                request(&output, 4, 4),
                Filter::Bilinear,
            )?;
        }
        Ok::<_, RunError>(())
    });
    outcome.expect("resized");
    assert_eq!(count, 0);
    assert_eq!(plan.runs.get(), 1001);
}
