//! A deinterlacer that opens itself, applies a job's settings when they change, and reopens
//! after a failure.
//!
//! # Failures
//!
//! A job or setting the kernel refuses as invalid (`EINVAL`) is the caller's: it is answered with
//! [`RunError::Refused`], the engine stays open, and the settings are applied afresh (from a
//! reset) on the next job. Anything else (opening, the version query, a request that fails another
//! way, such as a timeout) closes the engine and starts a backoff: until it is due,
//! [`Deinterlacer::run`] answers [`RunError::BackingOff`] without touching the device, and the
//! first call after it reopens. The delay doubles from 250 ms, is capped at 8 s, and clears when a
//! job runs. The driver never gives up and never turns itself off; [`Deinterlacer::stop`] closes
//! it and the next `run` opens it again.
//!
//! # Allocation
//!
//! Opening may allocate (the path, the error). A job on an open engine allocates nothing, whether
//! or not its settings change.

use std::io;

use crate::job::{Config, Job, Settings};
use crate::uapi;

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// The kernel's operations on one open file of the block, one per request. A Linux
/// implementation issues the [`uapi`] requests; it never maps the node (its `mmap` maps any
/// physical page asked for) and never asks for kernel memory. Tests play one back.
pub trait Device {
    /// `DI_IOC_GET_VERSION`.
    fn version(&self) -> io::Result<uapi::Version>;
    /// `DI_IOC_RESET`.
    fn reset(&self) -> io::Result<()>;
    /// `DI_IOC_SET_VIDEO_SIZE`, `DI_IOC_SET_VIDEO_CROP`, `DI_IOC_SET_DIT_MODE`,
    /// `DI_IOC_SET_TNR_MODE` and `DI_IOC_SET_FMD_ENABLE`, in that order.
    fn configure(&self, settings: &Settings) -> io::Result<()>;
    /// `DI_IOC_CHECK_PARA`.
    fn check(&self) -> io::Result<()>;
    /// `DI_IOC_PROCESS_FB` with a copy of `job`'s request, answering the call's result.
    fn process(&self, job: &Job<'_>) -> io::Result<i32>;
}

/// What a job found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Processed {
    /// The block's motion detector took the input to be progressive
    /// (`FB_PROCESS_ERROR_INTERLACE_TYPE`). The outputs are written either way.
    pub progressive: bool,
}

/// Why a job did not run.
#[derive(Debug)]
pub enum RunError {
    /// The kernel refused the job or its settings as invalid. The engine stays open.
    Refused(io::Error),
    /// The engine failed; it is closed and reopens on the first call from `retry_at_ns`.
    Device {
        /// What failed.
        error: io::Error,
        /// When the next call may reopen it, on the caller's clock.
        retry_at_ns: u64,
    },
    /// An earlier failure's backoff has not run out. The device was not touched.
    BackingOff {
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "the deinterlacer refused the job: {e}"),
            Self::Device { error, retry_at_ns } => write!(
                f,
                "the deinterlacer failed ({error}); retrying at {retry_at_ns} ns"
            ),
            Self::BackingOff { retry_at_ns } => write!(f, "backing off until {retry_at_ns} ns"),
        }
    }
}

impl std::error::Error for RunError {}

/// Where the engine is.
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

/// The deinterlacer, opened by `open` when it is needed.
pub struct Deinterlacer<D, F> {
    open: F,
    device: Option<D>,
    version: Option<uapi::Version>,
    applied: Option<Config>,
    failures: u32,
    retry_at_ns: u64,
}

impl<D, F> std::fmt::Debug for Deinterlacer<D, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deinterlacer")
            .field("open", &self.device.is_some())
            .field("version", &self.version)
            .field("applied", &self.applied)
            .field("failures", &self.failures)
            .field("retry_at_ns", &self.retry_at_ns)
            .finish()
    }
}

impl<D: Device, F: FnMut() -> io::Result<D>> Deinterlacer<D, F> {
    /// A deinterlacer that `open` opens. Nothing is opened until the first [`run`](Self::run).
    pub fn new(open: F) -> Self {
        Self {
            open,
            device: None,
            version: None,
            applied: None,
            failures: 0,
            retry_at_ns: 0,
        }
    }

    /// Where the engine is.
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

    /// The version the block answered when it was last opened.
    pub fn version(&self) -> Option<uapi::Version> {
        self.version
    }

    /// Closes the engine. The next [`run`](Self::run) opens it again, with no backoff.
    pub fn stop(&mut self) {
        self.device = None;
        self.applied = None;
        self.failures = 0;
        self.retry_at_ns = 0;
    }

    /// Runs `job` at `now_ns` on the caller's clock, opening the engine first if it is closed and
    /// applying the job's settings (from a reset) when they differ from the last applied.
    pub fn run(&mut self, now_ns: u64, job: &Job<'_>) -> Result<Processed, RunError> {
        let device = match self.device.take() {
            Some(device) => device,
            None => self.reopen(now_ns)?,
        };
        let config = job.config();
        if self.applied != Some(config) {
            self.applied = None;
            let applied = device
                .reset()
                .and_then(|()| device.configure(&config.settings()))
                .and_then(|()| device.check());
            if let Err(error) = applied {
                return Err(self.settle(now_ns, device, error));
            }
            self.applied = Some(config);
        }
        match device.process(job) {
            Ok(result) => {
                self.failures = 0;
                self.device = Some(device);
                Ok(Processed {
                    progressive: result == uapi::PROCESSED_PROGRESSIVE,
                })
            }
            Err(error) => Err(self.settle(now_ns, device, error)),
        }
    }

    /// Keeps the engine open for a refusal (with the settings to be applied afresh), closes it
    /// for anything else.
    fn settle(&mut self, now_ns: u64, device: D, error: io::Error) -> RunError {
        self.applied = None;
        if error.kind() == io::ErrorKind::InvalidInput {
            self.device = Some(device);
            RunError::Refused(error)
        } else {
            self.fail(now_ns, error)
        }
    }

    fn reopen(&mut self, now_ns: u64) -> Result<D, RunError> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(RunError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        let device = (self.open)().map_err(|error| self.fail(now_ns, error))?;
        self.version = Some(device.version().map_err(|error| self.fail(now_ns, error))?);
        Ok(device)
    }

    /// Closes the engine (already taken) and schedules the next attempt.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> RunError {
        self.device = None;
        self.applied = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = retry_at_ns;
        RunError::Device { error, retry_at_ns }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
