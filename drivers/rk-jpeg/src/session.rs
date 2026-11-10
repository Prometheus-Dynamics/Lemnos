//! A session on one JPEG block: a service file it opens itself, bound to the block.
//!
//! # Failures
//!
//! Anything the device fails at (opening, binding, a query) closes the file and starts a backoff:
//! until it is due, calls answer [`SessionError::BackingOff`] without touching the device, and the
//! first call after it opens and binds again. The delay doubles from 250 ms, is capped at 8 s,
//! and clears when a call succeeds. The session never gives up and never turns itself off:
//! [`Jpeg::stop`] closes it, and the next call opens it again.

use std::io;

use crate::device::Device;
use crate::regs::Variant;

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Why a call did not go through.
#[derive(Debug)]
pub enum SessionError {
    /// The device failed; the file is closed and reopens on the first call from `retry_at_ns`.
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

/// Where the session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing open; the next call opens the service.
    Closed,
    /// The file is open and bound.
    Open,
    /// A failure closed the file; it reopens on the first call from `retry_at_ns`.
    BackingOff {
        /// Failures in a row.
        failures: u32,
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

/// One JPEG block, on a service file that `open` makes.
pub struct Jpeg<D: Device, F: FnMut() -> io::Result<D>> {
    variant: Variant,
    open: F,
    device: Option<D>,
    failures: u32,
    retry_at_ns: Option<u64>,
}

impl<D: Device, F: FnMut() -> io::Result<D>> Jpeg<D, F> {
    /// A session on the `variant` block (from its board entry's compatible string, see
    /// [`Variant::from_compatible`]) that opens its file with `open` when first used, and again
    /// after a failure. Nothing is opened here.
    pub fn new(variant: Variant, open: F) -> Self {
        Self {
            variant,
            open,
            device: None,
            failures: 0,
            retry_at_ns: None,
        }
    }

    /// The block.
    #[must_use]
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// Where the session is.
    #[must_use]
    pub fn state(&self) -> State {
        match (self.retry_at_ns, &self.device) {
            (Some(retry_at_ns), _) => State::BackingOff {
                failures: self.failures,
                retry_at_ns,
            },
            (None, Some(_)) => State::Open,
            (None, None) => State::Closed,
        }
    }

    /// Closes the file and forgets any backoff. The next call opens it again.
    pub fn stop(&mut self) {
        self.device = None;
        self.failures = 0;
        self.retry_at_ns = None;
    }

    /// The block's hardware id, at `now_ns` on the caller's monotonic clock.
    pub fn hw_id(&mut self, now_ns: u64) -> Result<u32, SessionError> {
        let result = self.ready(now_ns)?.hw_id();
        self.settle(now_ns, result)
    }

    /// Lets go of the dma-bufs the file holds. Nothing is held while it is closed, so then this
    /// does nothing.
    pub fn release_buffers(&mut self, now_ns: u64) -> Result<(), SessionError> {
        let Some(device) = self.device.as_mut() else {
            return Ok(());
        };
        let result = device.release_buffers();
        self.settle(now_ns, result)
    }

    fn settle<T>(&mut self, now_ns: u64, result: io::Result<T>) -> Result<T, SessionError> {
        match result {
            Ok(value) => {
                self.failures = 0;
                Ok(value)
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// The open file, opening and binding it if it is closed and no backoff is pending.
    fn ready(&mut self, now_ns: u64) -> Result<&mut D, SessionError> {
        if let Some(retry_at_ns) = self.retry_at_ns {
            if now_ns < retry_at_ns {
                return Err(SessionError::BackingOff { retry_at_ns });
            }
            self.retry_at_ns = None;
        }
        let device = match self.device.take() {
            Some(device) => device,
            None => {
                let device_type = self.variant.device_type();
                match (self.open)().and_then(|mut device| {
                    device.init_client(device_type)?;
                    Ok(device)
                }) {
                    Ok(device) => device,
                    Err(error) => return Err(self.fail(now_ns, error)),
                }
            }
        };
        Ok(self.device.insert(device))
    }

    /// Closes the file after a device failure and schedules the reopen.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> SessionError {
        self.device = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = Some(retry_at_ns);
        SessionError::Device { error, retry_at_ns }
    }
}
