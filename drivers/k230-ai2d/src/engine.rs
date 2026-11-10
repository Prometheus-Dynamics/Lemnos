//! The block, opened when it is needed and reopened after a failure, running one resize at a
//! time between two dma-bufs.
//!
//! # A resize
//!
//! [`Ai2d::resize`] checks everything it can before the block is touched: the format and filter
//! are ones the [`Device`] is driven with, the sizes are inside the limits, and each image fits
//! its dma-buf by the size the kernel answers for it. Then both dma-bufs are imported, the
//! [`Job`] is built from the addresses the kernel answered (checked again, and for overlap), the
//! block runs it to its end, and both imports are removed, whatever happened. The kernel holds an
//! imported dma-buf's memory until it is removed, so the block never works on memory that was
//! freed meanwhile.
//!
//! # Failures
//!
//! A job refused by the checks is the caller's: [`RunError::Refused`], and the block is not
//! touched. Anything else (opening, an import, the run, a removal) closes the block and starts a
//! backoff: until it is due, [`Ai2d::resize`] answers [`RunError::BackingOff`] without touching
//! the block, and the first call after it reopens. Closing the node makes the kernel drop every
//! import it still holds. The delay doubles from 250 ms, is capped at 8 s, and clears when a
//! resize runs. [`Ai2d::stop`] closes the block; the next resize opens it again.
//!
//! # Allocation
//!
//! Opening may allocate. A resize on an open block allocates nothing here.

use std::io;
use std::os::fd::BorrowedFd;

use lemnos_contract_image_scaler::Filter;
use lemnos_linux_sys::dma_buf;

use crate::job::{Format, Image, Imported, Job, JobError, Limits, Shape};

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// What drives the block. No Linux implementation exists yet: see the crate docs for what it
/// needs that the reference does not give.
pub trait Device {
    /// The formats it drives the block with.
    const FORMATS: &'static [Format];
    /// The filters it drives the block with.
    const FILTERS: &'static [Filter];
    /// Imports `dmabuf` for the block (`AI2D_IMPORT_DMABUF`) and answers its address. The
    /// memory stays the block's to reach until [`remove`](Self::remove) or the device is
    /// dropped.
    fn import(&mut self, dmabuf: BorrowedFd<'_>) -> io::Result<u64>;
    /// Removes an import (`AI2D_REMOVE_DMABUF`).
    fn remove(&mut self, dmabuf: BorrowedFd<'_>) -> io::Result<()>;
    /// Runs `job` and waits for the block to finish it. Programs nothing outside the job's
    /// images.
    fn run(&mut self, job: &Job) -> io::Result<()>;
}

/// One image of a resize, as a caller gives it: its dma-buf and its shape.
#[derive(Clone, Copy, Debug)]
pub struct Request<'fd> {
    /// The dma-buf the image is in, from its start.
    pub dmabuf: BorrowedFd<'fd>,
    /// The image.
    pub shape: Shape,
}

/// Why a resize did not run.
#[derive(Debug)]
pub enum RunError {
    /// The job was refused before the block was touched.
    Refused(JobError),
    /// The kernel could not say how large a dma-buf is: the descriptor isn't one.
    Unsized(io::Error),
    /// The block failed; it is closed and reopens on the first call from `retry_at_ns`.
    Device {
        /// What failed.
        error: io::Error,
        /// When the next call may reopen it, on the caller's clock.
        retry_at_ns: u64,
    },
    /// An earlier failure's backoff has not run out. The block was not touched.
    BackingOff {
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "refused: {e}"),
            Self::Unsized(e) => write!(f, "a dma-buf of unknown size: {e}"),
            Self::Device { error, retry_at_ns } => {
                write!(
                    f,
                    "the block failed ({error}); retrying at {retry_at_ns} ns"
                )
            }
            Self::BackingOff { retry_at_ns } => write!(f, "backing off until {retry_at_ns} ns"),
        }
    }
}

impl std::error::Error for RunError {}

/// Where the block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not open, and free to open on the next call.
    Closed,
    /// Open.
    Open,
    /// Closed after a failure; the next call at or after `retry_at_ns` reopens it.
    BackingOff {
        /// When.
        retry_at_ns: u64,
        /// Failures in a row.
        failures: u32,
    },
}

/// The block, opened by `open` when it is needed.
pub struct Ai2d<D, F> {
    open: F,
    device: Option<D>,
    limits: Limits,
    failures: u32,
    retry_at_ns: u64,
}

impl<D, F> std::fmt::Debug for Ai2d<D, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ai2d")
            .field("open", &self.device.is_some())
            .field("limits", &self.limits)
            .field("failures", &self.failures)
            .field("retry_at_ns", &self.retry_at_ns)
            .finish()
    }
}

/// The format `fourcc` names, if `D` drives the block with it.
pub fn format<D: Device>(fourcc: u32) -> Option<Format> {
    D::FORMATS.iter().find(|f| f.fourcc == fourcc).copied()
}

impl<D: Device, F: FnMut() -> io::Result<D>> Ai2d<D, F> {
    /// A block that `open` opens, taking images up to `limits`. Nothing is opened until the
    /// first [`resize`](Self::resize).
    pub fn new(open: F, limits: Limits) -> Self {
        Self {
            open,
            device: None,
            limits,
            failures: 0,
            retry_at_ns: 0,
        }
    }

    /// The largest image a resize takes.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Where the block is.
    pub fn state(&self) -> State {
        if self.device.is_some() {
            State::Open
        } else if self.failures > 0 {
            State::BackingOff {
                retry_at_ns: self.retry_at_ns,
                failures: self.failures,
            }
        } else {
            State::Closed
        }
    }

    /// Closes the block. The next [`resize`](Self::resize) opens it again, with no backoff.
    pub fn stop(&mut self) {
        self.device = None;
        self.failures = 0;
        self.retry_at_ns = 0;
    }

    /// Resizes `input` into `output` with `filter`, at `now_ns` on the caller's clock. Answers
    /// the bytes the output covers. The block has finished with both dma-bufs, and holds neither,
    /// when this returns.
    pub fn resize(
        &mut self,
        now_ns: u64,
        input: Request<'_>,
        output: Request<'_>,
        filter: Filter,
    ) -> Result<u64, RunError> {
        let known = |shape: &Shape| D::FORMATS.contains(&shape.format);
        if !known(&input.shape) || !known(&output.shape) || !D::FILTERS.contains(&filter) {
            return Err(RunError::Refused(JobError::Format));
        }
        if input.shape.format != output.shape.format {
            return Err(RunError::Refused(JobError::Format));
        }
        let sizes = (
            dma_buf::size(input.dmabuf).map_err(RunError::Unsized)?,
            dma_buf::size(output.dmabuf).map_err(RunError::Unsized)?,
        );
        input
            .shape
            .fits(self.limits, sizes.0)
            .and_then(|_| output.shape.fits(self.limits, sizes.1))
            .map_err(RunError::Refused)?;
        let mut device = match self.device.take() {
            Some(device) => device,
            None => self.reopen(now_ns)?,
        };
        let outcome = Self::run_imported(
            &mut device,
            self.limits,
            (input, sizes.0),
            (output, sizes.1),
            filter,
        );
        match outcome {
            Ok(Ok(bytes)) => {
                self.failures = 0;
                self.device = Some(device);
                Ok(bytes)
            }
            Ok(Err(refused)) => {
                self.device = Some(device);
                Err(RunError::Refused(refused))
            }
            // Dropping the device closes the node, and the kernel drops what it still holds.
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// Imports both dma-bufs, runs the job, and removes both imports. An `Err` is the block's;
    /// an `Ok(Err)` the job's, refused by the checks on the addresses the kernel answered.
    fn run_imported(
        device: &mut D,
        limits: Limits,
        (input, input_size): (Request<'_>, u64),
        (output, output_size): (Request<'_>, u64),
        filter: Filter,
    ) -> io::Result<Result<u64, JobError>> {
        let input_at = device.import(input.dmabuf)?;
        // On a failure the first import goes with the device, which the caller then drops.
        let output_at = device.import(output.dmabuf)?;
        let job =
            Image::new(Imported::new(input_at, input_size), input.shape, limits).and_then(|from| {
                let to = Image::new(Imported::new(output_at, output_size), output.shape, limits)?;
                Job::new(from, to, filter)
            });
        let ran = match &job {
            Ok(job) => device.run(job).map(|()| Ok(job.output().bytes())),
            Err(refused) => Ok(Err(*refused)),
        };
        let removed = device
            .remove(input.dmabuf)
            .and_then(|()| device.remove(output.dmabuf));
        let result = ran?;
        removed?;
        Ok(result)
    }

    fn reopen(&mut self, now_ns: u64) -> Result<D, RunError> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(RunError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        (self.open)().map_err(|error| self.fail(now_ns, error))
    }

    /// Closes the block (already taken) and schedules the next attempt.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> RunError {
        self.device = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = retry_at_ns;
        RunError::Device { error, retry_at_ns }
    }
}

#[cfg(test)]
pub(crate) mod tests;
