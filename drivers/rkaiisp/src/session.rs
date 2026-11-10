//! The parameter ring: the node set up for the caller's work, and parameter buffers carried to it.
//!
//! # The node, step by step
//!
//! Opening (on the first call, and again after a failure): [`Device::set_param_info`] with the
//! [`Settings`]' [`ParamInfo`] (it sizes the kernel's parameter buffers, so it comes first),
//! [`Device::set_memory_mode`], then [`Device::start`] for the parameter buffers, which says how
//! many the kernel gave and how large each is.
//!
//! [`Aiisp::submit`] checks a parameter buffer ([`params::check`](crate::params::check)), copies
//! it into a kernel buffer the kernel does not hold and queues it. The kernel takes the oldest
//! queued buffer when a frame comes, keeps it while it is the current one (the block reads its
//! weights on every frame), and gives the one before it back. [`Aiisp::reclaim`] takes back what
//! the kernel gave back.
//!
//! # Failures
//!
//! A buffer that fails its check never reaches the device. One the kernel refuses (`EINVAL`) is
//! [`RunError::Refused`]: the node stays open. Anything else the device fails at closes it (which
//! lets go of every buffer) and starts a backoff: until it is due, every call answers
//! [`RunError::BackingOff`] without touching the device, and the first call after it opens the
//! node again. The delay doubles from 250 ms and is capped at 8 s, and clears when a call
//! succeeds. The ring never gives up and never turns itself off: [`Aiisp::stop`] closes it.
//!
//! # Allocation
//!
//! Opening may allocate (the device does). Checking, submitting and reclaiming allocate nothing.

use std::io;

use crate::params::{self, Checked, ParamError};
use crate::uapi::{
    MAX_RUNCNT, PARAM_BUFFERS_MAX, PARAM_BUFFERS_MIN, ParamInfo, Params, exealgo, exemode,
    mem_mode, param_buffer_bytes,
};

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The most weight bytes per run a [`Settings`] asks the kernel for: Lemnos's own bound, so that
/// eight runs' buffers stay well inside the 32 bits the kernel sizes them in.
pub const MAX_PARA_SIZE: u32 = 64 << 20;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// What the node is set up for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    info: ParamInfo,
    memory_mode: u32,
    buffers: u32,
}

impl Settings {
    /// `exealgo` one of [`exealgo`], `exemode` one of [`exemode`], `para_size` weight bytes per
    /// run (1 to [`MAX_PARA_SIZE`]), `max_runcnt` runs per frame (1 to [`MAX_RUNCNT`]),
    /// `memory_mode` one of [`mem_mode`] and `buffers` parameter buffers to ask for
    /// ([`PARAM_BUFFERS_MIN`] to [`PARAM_BUFFERS_MAX`]). `None` for anything out of range.
    #[must_use]
    pub fn new(
        exealgo: u32,
        exemode: u32,
        para_size: u32,
        max_runcnt: u32,
        memory_mode: u32,
        buffers: u32,
    ) -> Option<Self> {
        let ok = exealgo <= exealgo::AIYNR
            && exemode <= exemode::BOTHEVENT_IN_KERNEL
            && (1..=MAX_PARA_SIZE).contains(&para_size)
            && (1..=MAX_RUNCNT).contains(&max_runcnt)
            && memory_mode <= mem_mode::COMBO_MEMODE
            && (PARAM_BUFFERS_MIN..=PARAM_BUFFERS_MAX).contains(&buffers)
            && param_buffer_bytes(para_size, max_runcnt).is_some();
        ok.then_some(Self {
            info: ParamInfo {
                exealgo,
                exemode,
                para_size,
                max_runcnt,
            },
            memory_mode,
            buffers,
        })
    }

    /// The [`ParamInfo`] the node is given.
    #[must_use]
    pub const fn info(&self) -> ParamInfo {
        self.info
    }
}

/// The parameter node, as the ring uses it. A Linux implementation is the vendor driver's video
/// node (`rkaiisp`, `V4L2_BUF_TYPE_META_OUTPUT`, memory-mapped buffers) and the private requests
/// in [`uapi`](crate::uapi); none is in this crate (see the crate documentation).
pub trait Device {
    /// `RKAIISP_CMD_SET_PARAM_INFO`.
    fn set_param_info(&mut self, info: &ParamInfo) -> io::Result<()>;
    /// `RKAIISP_CMD_SET_MEMORY_MODE`.
    fn set_memory_mode(&mut self, mode: u32) -> io::Result<()>;
    /// Asks for `count` memory-mapped parameter buffers, maps them and starts the queue. Returns
    /// how many the kernel gave and the bytes of each.
    fn start(&mut self, count: u32) -> io::Result<(u32, usize)>;
    /// Copies `bytes` to the start of parameter buffer `slot`, which the kernel does not hold.
    fn write(&mut self, slot: u32, bytes: &[u8]) -> io::Result<()>;
    /// Queues parameter buffer `slot` with `used` bytes.
    fn queue(&mut self, slot: u32, used: usize) -> io::Result<()>;
    /// A parameter buffer the kernel gave back, or `None` when none is waiting.
    fn dequeue(&mut self) -> io::Result<Option<u32>>;
    /// `RKAIISP_CMD_CLEAR_IQPARAMS`: the kernel gives back the queued buffers it has not used.
    fn clear(&mut self) -> io::Result<()>;
}

/// Why a call did not go through.
#[derive(Debug)]
pub enum RunError {
    /// The parameter buffer failed its check; the device was not touched.
    Invalid(ParamError),
    /// Every kernel buffer is with the kernel; [`Aiisp::reclaim`] first.
    Busy,
    /// The kernel refused the call. The node stays open.
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

/// Where the ring is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing open; the next call opens the node.
    Closed,
    /// The node is open with its parameter buffers.
    Running,
    /// A failure closed the node; it reopens on the first call from `retry_at_ns`.
    BackingOff {
        /// Failures in a row.
        failures: u32,
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

/// An open node and its parameter buffers.
struct Open<D> {
    device: D,
    count: u32,
    capacity: usize,
    /// Buffers the kernel does not hold, one bit each.
    free: u64,
}

/// The AI-ISP's parameter ring on a node that `open` makes.
pub struct Aiisp<D: Device, F: FnMut() -> io::Result<D>> {
    open: F,
    settings: Settings,
    node: Option<Open<D>>,
    failures: u32,
    retry_at_ns: Option<u64>,
}

impl<D: Device, F: FnMut() -> io::Result<D>> Aiisp<D, F> {
    /// A ring set up with `settings` that opens its node with `open` on the first call (and
    /// again after a failure). Nothing is opened here.
    pub fn new(settings: Settings, open: F) -> Self {
        Self {
            open,
            settings,
            node: None,
            failures: 0,
            retry_at_ns: None,
        }
    }

    /// Where the ring is.
    #[must_use]
    pub fn state(&self) -> State {
        match (self.retry_at_ns, &self.node) {
            (Some(retry_at_ns), _) => State::BackingOff {
                failures: self.failures,
                retry_at_ns,
            },
            (None, Some(_)) => State::Running,
            (None, None) => State::Closed,
        }
    }

    /// Closes the node and forgets any backoff. The next call opens it again.
    pub fn stop(&mut self) {
        self.node = None;
        self.failures = 0;
        self.retry_at_ns = None;
    }

    /// Checks `bytes` (a parameter buffer: [`Params`] and the weights) and queues a copy of it, at
    /// `now_ns` on the caller's monotonic clock. Returns the kernel buffer it went into and what
    /// the check found. `bytes` may change as soon as this returns.
    pub fn submit(&mut self, now_ns: u64, bytes: &[u8]) -> Result<(u32, Checked), RunError> {
        let max_runcnt = self.settings.info.max_runcnt;
        let node = self.ready(now_ns)?;
        let checked = params::check(bytes, max_runcnt, node.capacity).map_err(RunError::Invalid)?;
        if node.free == 0 {
            return Err(RunError::Busy);
        }
        let slot = node.free.trailing_zeros();
        let result = node
            .device
            .write(slot, bytes)
            .and_then(|()| node.device.queue(slot, bytes.len()));
        match result {
            Ok(()) => {
                node.free &= !(1 << slot);
                self.failures = 0;
                Ok((slot, checked))
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                Err(RunError::Refused(error))
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// Takes back every parameter buffer the kernel has given back. Returns how many.
    pub fn reclaim(&mut self, now_ns: u64) -> Result<u32, RunError> {
        let node = self.ready(now_ns)?;
        let mut taken = 0;
        for _ in 0..node.count {
            match node.device.dequeue() {
                Ok(None) => break,
                Ok(Some(slot)) if slot < node.count && node.free & (1 << slot) == 0 => {
                    node.free |= 1 << slot;
                    taken += 1;
                }
                Ok(Some(_)) => {
                    let error = io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the kernel gave back a buffer it did not hold",
                    );
                    return Err(self.fail(now_ns, error));
                }
                Err(error) => return Err(self.fail(now_ns, error)),
            }
        }
        self.failures = 0;
        Ok(taken)
    }

    /// Has the kernel give back the queued parameter buffers it has not used
    /// (`RKAIISP_CMD_CLEAR_IQPARAMS`); [`Self::reclaim`] then takes them.
    pub fn clear(&mut self, now_ns: u64) -> Result<(), RunError> {
        let node = self.ready(now_ns)?;
        match node.device.clear() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                Err(RunError::Refused(error))
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// The open node, opening it if it is closed and no backoff is pending.
    fn ready(&mut self, now_ns: u64) -> Result<&mut Open<D>, RunError> {
        if let Some(retry_at_ns) = self.retry_at_ns {
            if now_ns < retry_at_ns {
                return Err(RunError::BackingOff { retry_at_ns });
            }
            self.retry_at_ns = None;
        }
        if self.node.is_none() {
            match self.open_node() {
                Ok(node) => self.node = Some(node),
                Err(error) => return Err(self.fail(now_ns, error)),
            }
        }
        // Just set when it was empty.
        self.node.as_mut().ok_or(RunError::Busy)
    }

    fn open_node(&mut self) -> io::Result<Open<D>> {
        let mut device = (self.open)()?;
        device.set_param_info(&self.settings.info)?;
        device.set_memory_mode(self.settings.memory_mode)?;
        let (count, capacity) = device.start(self.settings.buffers)?;
        if !(PARAM_BUFFERS_MIN..=PARAM_BUFFERS_MAX).contains(&count)
            || capacity < size_of::<Params>()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the kernel gave parameter buffers out of range",
            ));
        }
        Ok(Open {
            device,
            count,
            capacity,
            free: (1 << count) - 1,
        })
    }

    /// Closes the node after a device failure and schedules the reopen.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> RunError {
        self.node = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = Some(retry_at_ns);
        RunError::Device { error, retry_at_ns }
    }
}
