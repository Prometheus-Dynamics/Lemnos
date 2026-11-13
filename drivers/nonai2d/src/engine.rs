//! An engine that opens itself for the layouts a conversion needs, and reopens after a failure.
//!
//! # Layouts
//!
//! The node is set up for one source and one destination layout. A conversion in the layouts
//! the engine is open for runs at once; one in others closes it and opens it again for those.
//! That is not a failure and starts no backoff.
//!
//! # Failures
//!
//! What the device refuses as the caller's ([`io::ErrorKind::InvalidInput`]: a layout it did not
//! keep, a request it refused) is answered with [`RunError::Refused`]; the engine is closed, so
//! the next conversion starts on a clean node, and no backoff starts. Anything else (opening, a
//! timeout, a frame the device marked failed) closes the engine and starts a backoff: until it is
//! due, [`Nonai2d::run`] answers [`RunError::BackingOff`] without touching the device, and the
//! first call after it reopens. The delay doubles from 250 ms, is capped at 8 s, and clears when
//! a conversion runs. [`Nonai2d::stop`] closes the engine; the next `run` opens it again.
//!
//! # Allocation
//!
//! Opening may allocate (the path, the error). A conversion on an open engine allocates nothing.

use std::io;
use std::path::PathBuf;

use lemnos_driver_m2m_decode::Node;

use crate::device::Device;
use crate::frame::Frame;
use crate::layout::Layout;

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// What converts a frame: a [`Device`] on Linux, something that plays one back in tests.
pub trait Engine {
    /// The source and destination layouts it was opened for.
    fn layouts(&self) -> (Layout, Layout);
    /// Converts `src` into `dst` and waits for it.
    fn convert(&mut self, src: &Frame<'_>, dst: &Frame<'_>) -> io::Result<()>;
}

impl<E: Engine + ?Sized> Engine for Box<E> {
    fn layouts(&self) -> (Layout, Layout) {
        (**self).layouts()
    }

    fn convert(&mut self, src: &Frame<'_>, dst: &Frame<'_>) -> io::Result<()> {
        (**self).convert(src, dst)
    }
}

impl<N: Node> Engine for Device<N> {
    fn layouts(&self) -> (Layout, Layout) {
        Device::layouts(self)
    }

    fn convert(&mut self, src: &Frame<'_>, dst: &Frame<'_>) -> io::Result<()> {
        Device::convert(self, src, dst)
    }
}

/// Opens the node at `path` for the layouts it is called with, for [`Nonai2d::new`]. Use
/// [`find`](crate::find) for the path.
pub fn node(path: impl Into<PathBuf>) -> impl FnMut(Layout, Layout) -> io::Result<Device> {
    let path = path.into();
    move |src, dst| Device::open(&path, src, dst)
}

/// Why a conversion did not run.
#[derive(Debug)]
pub enum RunError {
    /// The device refused it as the caller's. No backoff started.
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
            Self::Refused(e) => write!(f, "the engine refused the conversion: {e}"),
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
    /// Open for these source and destination layouts.
    Open(Layout, Layout),
    /// Closed after a failure; the next call at or after `retry_at_ns` reopens it.
    BackingOff {
        /// When.
        retry_at_ns: u64,
        /// Failures in a row.
        failures: u32,
    },
}

/// The engine, opened by `open` for the layouts it is needed for.
pub struct Nonai2d<E, F> {
    open: F,
    engine: Option<E>,
    failures: u32,
    retry_at_ns: u64,
}

impl<E: Engine, F> std::fmt::Debug for Nonai2d<E, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nonai2d")
            .field("open", &self.engine.as_ref().map(Engine::layouts))
            .field("failures", &self.failures)
            .field("retry_at_ns", &self.retry_at_ns)
            .finish()
    }
}

impl<E: Engine, F: FnMut(Layout, Layout) -> io::Result<E>> Nonai2d<E, F> {
    /// An engine that `open` opens (for example [`node`]`(path)`). Nothing is opened until the
    /// first [`run`](Self::run).
    pub fn new(open: F) -> Self {
        Self {
            open,
            engine: None,
            failures: 0,
            retry_at_ns: 0,
        }
    }

    /// Where the engine is.
    pub fn state(&self) -> State {
        match &self.engine {
            Some(engine) => {
                let (src, dst) = engine.layouts();
                State::Open(src, dst)
            }
            None if self.failures > 0 => State::BackingOff {
                retry_at_ns: self.retry_at_ns,
                failures: self.failures,
            },
            None => State::Closed,
        }
    }

    /// Closes the engine. The next [`run`](Self::run) opens it again, with no backoff.
    pub fn stop(&mut self) {
        self.engine = None;
        self.failures = 0;
        self.retry_at_ns = 0;
    }

    /// Converts `src` into `dst` at `now_ns` on the caller's clock, opening the engine for their
    /// layouts first if it is closed or open for others.
    pub fn run(&mut self, now_ns: u64, src: &Frame<'_>, dst: &Frame<'_>) -> Result<(), RunError> {
        let want = (src.layout(), dst.layout());
        if want.0.size() != want.1.size() {
            return Err(RunError::Refused(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the engine does not scale",
            )));
        }
        let engine = match self.engine.take() {
            Some(engine) if engine.layouts() == want => engine,
            // One open for other layouts is dropped, and so closed, first.
            _ => self.reopen(now_ns, want)?,
        };
        let mut engine = engine;
        match engine.convert(src, dst) {
            Ok(()) => {
                self.failures = 0;
                self.engine = Some(engine);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                Err(RunError::Refused(error))
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    fn reopen(&mut self, now_ns: u64, (src, dst): (Layout, Layout)) -> Result<E, RunError> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(RunError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match (self.open)(src, dst) {
            Ok(engine) => Ok(engine),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                Err(RunError::Refused(error))
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
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
pub(crate) mod tests;
