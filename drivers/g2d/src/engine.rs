//! An engine that opens itself and reopens after a failure.
//!
//! # Failures
//!
//! A job the kernel refuses as invalid (`EINVAL`) is the caller's: it is answered with
//! [`RunError::Refused`] and the engine stays open. Anything else (opening, the version query, a
//! request that fails another way, such as a timeout) closes the engine and starts a backoff:
//! until it is due, [`G2d::run`] answers [`RunError::BackingOff`] without touching the device,
//! and the first call after it reopens. The delay doubles from 250 ms, is capped at 8 s, and
//! clears when a job runs. [`G2d::stop`] closes the engine; the next `run` opens it again.
//!
//! # Allocation
//!
//! Opening may allocate (the path, the error). A job run on an open engine allocates nothing.

use std::io;
use std::path::PathBuf;

use lemnos_driver_g2d_sys::{Device, HardwareVersion, Job};

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// What runs a job: a [`Device`] on Linux, something that plays one back in tests.
pub trait Engine {
    /// The engine's and the chip's versions.
    fn version(&self) -> io::Result<HardwareVersion>;
    /// Runs `job` and waits for it.
    fn run(&self, job: &Job<'_>) -> io::Result<()>;
}

impl Engine for Device {
    fn version(&self) -> io::Result<HardwareVersion> {
        Device::version(self)
    }

    fn run(&self, job: &Job<'_>) -> io::Result<()> {
        Device::run(self, job)
    }
}

/// Opens the device node at `path` each time it is called, for [`G2d::new`].
pub fn node(path: impl Into<PathBuf>) -> impl FnMut() -> io::Result<Device> {
    let path = path.into();
    move || Device::open(&path)
}

/// Why a job did not run.
#[derive(Debug)]
pub enum RunError {
    /// The kernel refused the job as invalid. The engine stays open.
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
            Self::Refused(e) => write!(f, "the engine refused the job: {e}"),
            Self::Device { error, retry_at_ns } => {
                write!(
                    f,
                    "the engine failed ({error}); retrying at {retry_at_ns} ns"
                )
            }
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

/// The G2D engine, opened by `open` when it is needed.
pub struct G2d<E, F> {
    open: F,
    engine: Option<E>,
    version: Option<HardwareVersion>,
    failures: u32,
    retry_at_ns: u64,
}

impl<E, F> std::fmt::Debug for G2d<E, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("G2d")
            .field("open", &self.engine.is_some())
            .field("version", &self.version)
            .field("failures", &self.failures)
            .field("retry_at_ns", &self.retry_at_ns)
            .finish()
    }
}

impl<E: Engine, F: FnMut() -> io::Result<E>> G2d<E, F> {
    /// An engine that `open` opens (for example [`node`]`(`[`DEVICE`](crate::DEVICE)`)`). Nothing
    /// is opened until the first [`run`](Self::run).
    pub fn new(open: F) -> Self {
        Self {
            open,
            engine: None,
            version: None,
            failures: 0,
            retry_at_ns: 0,
        }
    }

    /// Where the engine is.
    pub fn state(&self) -> State {
        if self.engine.is_some() {
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

    /// The versions the engine answered when it was last opened.
    pub fn version(&self) -> Option<HardwareVersion> {
        self.version
    }

    /// Closes the engine. The next [`run`](Self::run) opens it again, with no backoff.
    pub fn stop(&mut self) {
        self.engine = None;
        self.failures = 0;
        self.retry_at_ns = 0;
    }

    /// Runs `job` at `now_ns` on the caller's clock, opening the engine first if it is closed.
    pub fn run(&mut self, now_ns: u64, job: &Job<'_>) -> Result<(), RunError> {
        let engine = match self.engine.take() {
            Some(engine) => engine,
            None => self.reopen(now_ns)?,
        };
        match engine.run(job) {
            Ok(()) => {
                self.failures = 0;
                self.engine = Some(engine);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                self.engine = Some(engine);
                Err(RunError::Refused(error))
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    fn reopen(&mut self, now_ns: u64) -> Result<E, RunError> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(RunError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        let engine = (self.open)().map_err(|error| self.fail(now_ns, error))?;
        self.version = Some(engine.version().map_err(|error| self.fail(now_ns, error))?);
        Ok(engine)
    }

    /// Closes the engine (already taken) and schedules the next attempt.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> RunError {
        self.engine = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = retry_at_ns;
        RunError::Device { error, retry_at_ns }
    }
}

#[cfg(test)]
mod tests;
