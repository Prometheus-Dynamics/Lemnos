//! The scaler: a node it opens itself, the channels it holds in offline mode, and jobs run on it.
//!
//! # Failures
//!
//! A job is checked when it is built, so a bad one never reaches the scaler. A job that writes a
//! channel this scaler was not given is [`RunError::NotOurs`], and one the kernel refuses
//! (`EINVAL`) is [`RunError::Refused`]: the node stays open and no backoff starts. Anything the
//! device itself fails at (opening, selecting the channels, a job the block does not finish in
//! time, a channel that left offline mode) closes the node and starts a backoff: until it is due,
//! [`Scaler::run`] answers [`RunError::BackingOff`] without touching the device, and the first
//! call after it opens the node and selects the channels again. The delay doubles from 250 ms and
//! is capped at 8 s, and clears when a job runs. The scaler never gives up and never turns itself
//! off: [`Scaler::stop`] closes it, and the next `run` opens it again.
//!
//! # Allocation
//!
//! Opening allocates. A job run on an open node allocates nothing.

use std::io;
use std::path::PathBuf;

use lemnos_driver_rkvpss_sys::{CHANNELS, Job, Node};

use crate::device::Device;

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// Every channel's bit.
const ALL_CHANNELS: u8 = (1 << CHANNELS) - 1;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Why a job did not run.
#[derive(Debug)]
pub enum RunError {
    /// The job writes channels (these bits) this scaler was not given. The device was not
    /// touched.
    NotOurs {
        /// The channels outside the scaler's own.
        channels: u8,
    },
    /// The kernel refused the job. The node stays open.
    Refused(io::Error),
    /// The device failed; the node is closed and reopens on the first call from `retry_at_ns`.
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

/// Where the scaler is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing open; the next job opens the node.
    Closed,
    /// The node is open and its channels are in offline mode.
    Running,
    /// A failure closed the node; it reopens on the first call from `retry_at_ns`.
    BackingOff {
        /// Failures in a row.
        failures: u32,
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

/// The offline scaler on a node that `open` makes, holding the channels it was given.
pub struct Scaler<D: Device, F: FnMut() -> io::Result<D>> {
    open: F,
    channels: u8,
    device: Option<D>,
    failures: u32,
    retry_at_ns: Option<u64>,
}

/// A scaler on the node at `path` (see [`find`](crate::find)), holding `channels` (one bit per
/// [`Channel::bit`](crate::Channel::bit)).
pub fn open_at(path: PathBuf, channels: u8) -> Scaler<Node, impl FnMut() -> io::Result<Node>> {
    Scaler::new(move || Node::open(&path), channels)
}

impl<D: Device, F: FnMut() -> io::Result<D>> Scaler<D, F> {
    /// A scaler that opens its node with `open` when the first job comes (and again after a
    /// failure), and then hands `channels` to offline mode. Selecting a channel takes it from the
    /// block's camera path, so a scaler is given only the channels that path does not use. Bits
    /// past the sixth channel are dropped. Nothing is opened here.
    pub fn new(open: F, channels: u8) -> Self {
        Self {
            open,
            channels: channels & ALL_CHANNELS,
            device: None,
            failures: 0,
            retry_at_ns: None,
        }
    }

    /// The channels this scaler holds.
    #[must_use]
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Where the scaler is.
    #[must_use]
    pub fn state(&self) -> State {
        match (self.retry_at_ns, &self.device) {
            (Some(retry_at_ns), _) => State::BackingOff {
                failures: self.failures,
                retry_at_ns,
            },
            (None, Some(_)) => State::Running,
            (None, None) => State::Closed,
        }
    }

    /// Closes the node and forgets any backoff. The kernel lets go of the dma-bufs and, with no
    /// other opener, of the channels. The next [`Self::run`] opens it again.
    pub fn stop(&mut self) {
        self.device = None;
        self.failures = 0;
        self.retry_at_ns = None;
    }

    /// Runs `job` and waits for it, at `now_ns` on the caller's monotonic clock.
    pub fn run(&mut self, now_ns: u64, job: &mut Job<'_>) -> Result<(), RunError> {
        let foreign = job.channels() & !self.channels;
        if foreign != 0 {
            return Err(RunError::NotOurs { channels: foreign });
        }
        let device = self.ready(now_ns)?;
        match device.run(job) {
            Ok(()) => {
                self.failures = 0;
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::EINVAL) => {
                Err(RunError::Refused(error))
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// Lets go of the dma-bufs the node holds attached from earlier jobs. Nothing is held while
    /// the node is closed, so then this does nothing.
    pub fn release_buffers(&mut self, now_ns: u64) -> Result<(), RunError> {
        let Some(device) = self.device.as_mut() else {
            return Ok(());
        };
        if let Err(error) = device.release_buffers() {
            return Err(self.fail(now_ns, error));
        }
        Ok(())
    }

    /// The open node, opening it and selecting the channels if it is closed and no backoff is
    /// pending.
    fn ready(&mut self, now_ns: u64) -> Result<&mut D, RunError> {
        if let Some(retry_at_ns) = self.retry_at_ns {
            if now_ns < retry_at_ns {
                return Err(RunError::BackingOff { retry_at_ns });
            }
            self.retry_at_ns = None;
        }
        let device = match self.device.take() {
            Some(device) => device,
            None => match (self.open)().and_then(|mut device| {
                device.select_offline(self.channels)?;
                Ok(device)
            }) {
                Ok(device) => device,
                Err(error) => return Err(self.fail(now_ns, error)),
            },
        };
        Ok(self.device.insert(device))
    }

    /// Closes the node after a device failure and schedules the reopen.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> RunError {
        self.device = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = Some(retry_at_ns);
        RunError::Device { error, retry_at_ns }
    }
}
