//! The engine as one caller holds it: opened when needed, claimed, buffers attached with their bus
//! addresses checked, registers read inside the window, completions waited for.
//!
//! # Failures
//!
//! What the caller got wrong (an unknown buffer, a range past a buffer's end, a register outside
//! the window, a buffer the kernel won't attach) is answered without touching the engine's state.
//! Anything else the kernel refuses closes the engine: the driver detaches its buffers, releases
//! its lock and its use of the engine (each as far as the kernel lets it), drops the file, and
//! starts a backoff. Until it is due, every call answers [`Error::BackingOff`] without touching
//! the device; the first call after it reopens. The delay doubles from 250 ms, is capped at 8 s,
//! and clears when a call succeeds. The driver never gives up and never turns itself off; the
//! caller claims again after a failure, and buffer ids from before it are stale.
//!
//! # Allocation
//!
//! Opening may allocate. On a claimed engine, attaching and detaching buffers, deriving
//! addresses, reading registers and waiting allocate nothing; a buffer table of fixed size lives
//! in the value.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use crate::uapi;

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Buffers one claim may hold attached at once.
pub const MAX_BUFFERS: usize = 64;

/// The longest wait for a completion, in seconds. The kernel multiplies the timeout by its tick
/// rate in an `int`; this keeps that far from overflowing.
pub const MAX_WAIT_S: u32 = 3600;

/// Which of the engine's locks a claim takes (`VE_LOCK_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// `VE_LOCK_VDEC`.
    Decoder,
    /// `VE_LOCK_VENC`.
    Encoder,
    /// `VE_LOCK_JDEC`.
    Jpeg,
}

impl Lock {
    /// The bit the kernel takes.
    pub const fn bit(self) -> u32 {
        match self {
            Self::Decoder => uapi::lock::VDEC,
            Self::Encoder => uapi::lock::VENC,
            Self::Jpeg => uapi::lock::JDEC,
        }
    }
}

/// Which interrupt the kernel's handler looks at (`VE_MODE_*`). The engine's, not the file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `VE_MODE_ENCPP`.
    EncoderPreprocess,
    /// `VE_MODE_ENC`.
    Encoder,
    /// `VE_MODE_DE`.
    Decoder,
    /// `VE_MODE_VCUENC`.
    VcuEncoder,
    /// `VE_MODE_VCUDEC`.
    VcuDecoder,
}

impl Mode {
    /// The value the kernel takes.
    pub const fn value(self) -> u32 {
        match self {
            Self::EncoderPreprocess => uapi::mode::ENCPP,
            Self::Encoder => uapi::mode::ENC,
            Self::Decoder => uapi::mode::DE,
            Self::VcuEncoder => uapi::mode::VCU_ENC,
            Self::VcuDecoder => uapi::mode::VCU_DEC,
        }
    }
}

/// Which interrupt a wait is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// `IOCTL_WAIT_VE_DE`.
    Decoder,
    /// `IOCTL_WAIT_VE_EN`.
    Encoder,
    /// `IOCTL_WAIT_VCU_ENC`.
    VcuEncoder,
    /// `IOCTL_WAIT_VCU_DEC`.
    VcuDecoder,
}

impl Completion {
    /// The request that waits for it.
    pub const fn request(self) -> u32 {
        match self {
            Self::Decoder => uapi::WAIT_VE_DE,
            Self::Encoder => uapi::WAIT_VE_EN,
            Self::VcuEncoder => uapi::WAIT_VCU_ENC,
            Self::VcuDecoder => uapi::WAIT_VCU_DEC,
        }
    }
}

/// The kernel's operations on one open file of the engine, one per request. A Linux
/// implementation issues the [`uapi`] requests and reads a mapping of exactly
/// [`uapi::WINDOW_BYTES`] at offset 0; tests play one back.
pub trait Device {
    /// `IOCTL_ENGINE_REQ`.
    fn request_engine(&self) -> io::Result<()>;
    /// `IOCTL_ENGINE_REL`.
    fn release_engine(&self) -> io::Result<()>;
    /// `IOCTL_GET_LOCK` with `lock`'s bit.
    fn lock(&self, lock: Lock) -> io::Result<()>;
    /// `IOCTL_RELEASE_LOCK` with `lock`'s bit.
    fn unlock(&self, lock: Lock) -> io::Result<()>;
    /// `IOCTL_RESET_VE`.
    fn reset(&self) -> io::Result<()>;
    /// `IOCTL_VE_MODE` with `mode`'s value.
    fn set_mode(&self, mode: Mode) -> io::Result<()>;
    /// `IOCTL_MAP_DMA_BUF` on `buffer`, answering `phy_addr`.
    fn map(&self, buffer: BorrowedFd<'_>) -> io::Result<u32>;
    /// `IOCTL_UNMAP_DMA_BUF` on `buffer`, the same descriptor that was mapped.
    fn unmap(&self, buffer: BorrowedFd<'_>) -> io::Result<()>;
    /// `IOCTL_CLEAR_EN_INT_FLAG`.
    fn clear_completion(&self) -> io::Result<()>;
    /// `completion`'s wait request with `seconds`, answering whether the interrupt came.
    fn wait(&self, completion: Completion, seconds: u32) -> io::Result<bool>;
    /// The register at `offset` in the window. Called only with `offset` a multiple of 4 and
    /// `offset + 4 <= WINDOW_BYTES`, while the engine is claimed (its clocks on).
    fn read(&self, offset: usize) -> u32;
}

/// A buffer attached to a claim. Stale once the buffer is detached or the claim ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferId {
    index: u16,
    generation: u32,
}

/// Why a call did nothing.
#[derive(Debug)]
pub enum Error {
    /// The engine isn't claimed; claim it first.
    NotClaimed,
    /// Claimed already, with this lock.
    AlreadyClaimed(Lock),
    /// No buffer has this id now.
    UnknownBuffer,
    /// Every buffer slot is taken.
    Full,
    /// The range doesn't lie inside the buffer, or is empty.
    OutOfBuffer,
    /// The offset isn't a register inside the window.
    OutOfWindow,
    /// The buffer can't be attached: not a dma-buf, empty, or its bus range would pass 32 bits.
    /// The engine stays open.
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

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotClaimed => f.write_str("the engine is not claimed"),
            Self::AlreadyClaimed(lock) => write!(f, "the engine is claimed already ({lock:?})"),
            Self::UnknownBuffer => f.write_str("no such buffer"),
            Self::Full => f.write_str("every buffer slot is taken"),
            Self::OutOfBuffer => f.write_str("the range is not inside the buffer"),
            Self::OutOfWindow => f.write_str("not a register in the window"),
            Self::Refused(e) => write!(f, "the buffer was refused: {e}"),
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

impl std::error::Error for Error {}

/// Where the engine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not open, and free to open on the next call.
    Closed,
    /// Open, not claimed.
    Open,
    /// Claimed with this lock.
    Claimed(Lock),
    /// Closed after a failure; the next call at or after `retry_at_ns` reopens it.
    BackingOff {
        /// When.
        retry_at_ns: u64,
        /// Failures in a row.
        failures: u32,
    },
}

/// An attached buffer: the driver's own duplicate of the caller's descriptor (the kernel matches
/// the detach by descriptor number, so the number must stay this buffer's until then), the bus
/// address the kernel answered, and the dma-buf's size.
#[derive(Debug)]
struct Slot {
    fd: OwnedFd,
    bus: u32,
    len: u64,
}

/// The engine, opened by `open` when it is needed.
pub struct Block<D: Device, F> {
    open: F,
    device: Option<D>,
    claimed: Option<Lock>,
    slots: [Option<Slot>; MAX_BUFFERS],
    generations: [u32; MAX_BUFFERS],
    failures: u32,
    retry_at_ns: u64,
}

impl<D: Device, F> std::fmt::Debug for Block<D, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block")
            .field("open", &self.device.is_some())
            .field("claimed", &self.claimed)
            .field("buffers", &self.slots.iter().flatten().count())
            .field("failures", &self.failures)
            .field("retry_at_ns", &self.retry_at_ns)
            .finish()
    }
}

/// The last byte's bus address of a buffer at `bus` of `len` bytes must stay inside 32 bits, the
/// width the kernel answers in and the engine's address registers take.
fn bus_range_fits(bus: u32, len: u64) -> bool {
    len > 0
        && u64::from(bus)
            .checked_add(len)
            .is_some_and(|end| end <= 1 << 32)
}

impl<D: Device, F: FnMut() -> io::Result<D>> Block<D, F> {
    /// An engine that `open` opens. Nothing is opened until the first [`claim`](Self::claim).
    pub fn new(open: F) -> Self {
        Self {
            open,
            device: None,
            claimed: None,
            slots: [const { None }; MAX_BUFFERS],
            generations: [0; MAX_BUFFERS],
            failures: 0,
            retry_at_ns: 0,
        }
    }

    /// Where the engine is.
    pub fn state(&self) -> State {
        match (&self.device, self.claimed) {
            (Some(_), Some(lock)) => State::Claimed(lock),
            (Some(_), None) => State::Open,
            (None, _) if self.failures > 0 => State::BackingOff {
                retry_at_ns: self.retry_at_ns,
                failures: self.failures,
            },
            (None, _) => State::Closed,
        }
    }

    /// Claims the engine with `lock` at `now_ns` on the caller's clock, opening it first if it is
    /// closed: counts this caller as a user (the first one's turns the clocks on), then takes the
    /// lock. The kernel's lock blocks while another opener holds it.
    pub fn claim(&mut self, now_ns: u64, lock: Lock) -> Result<(), Error> {
        if let Some(held) = self.claimed {
            return Err(Error::AlreadyClaimed(held));
        }
        if self.device.is_none() {
            self.reopen(now_ns)?;
        }
        let device = self.device.as_ref().ok_or(Error::NotClaimed)?;
        if let Err(error) = device.request_engine() {
            return Err(self.fail(now_ns, error));
        }
        if let Err(error) = device.lock(lock) {
            // The use counted above is undone by `fail`'s teardown.
            self.claimed = None;
            let _ = device.release_engine();
            return Err(self.fail(now_ns, error));
        }
        self.claimed = Some(lock);
        self.failures = 0;
        Ok(())
    }

    /// Ends the claim: detaches every buffer, releases the lock and this caller's use. The engine
    /// stays open for the next claim.
    pub fn release(&mut self, now_ns: u64) -> Result<(), Error> {
        let Some(lock) = self.claimed else {
            return Ok(());
        };
        for index in 0..MAX_BUFFERS {
            if let Err(error) = self.detach_slot(index) {
                return Err(self.fail(now_ns, error));
            }
        }
        let device = self.device.as_ref().ok_or(Error::NotClaimed)?;
        let unlocked = device.unlock(lock);
        self.claimed = None;
        let released = device.release_engine();
        match unlocked.and(released) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// Releases any claim and closes the engine. The next [`claim`](Self::claim) opens it again,
    /// with no backoff.
    pub fn stop(&mut self) {
        self.teardown();
        self.failures = 0;
        self.retry_at_ns = 0;
    }

    /// Attaches `buffer`, a dma-buf the caller holds, to the claim. The driver keeps its own
    /// duplicate of the descriptor until the buffer is detached. Refused, with nothing attached,
    /// when the descriptor is not a dma-buf the kernel can attach, is empty, or would end past
    /// 32 bits of bus address.
    pub fn attach(&mut self, now_ns: u64, buffer: BorrowedFd<'_>) -> Result<BufferId, Error> {
        if self.claimed.is_none() {
            return Err(Error::NotClaimed);
        }
        let index = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Full)?;
        let len = buffer_len(buffer).map_err(Error::Refused)?;
        if len == 0 {
            return Err(Error::Refused(io::ErrorKind::InvalidInput.into()));
        }
        let fd = buffer.try_clone_to_owned().map_err(Error::Refused)?;
        let device = self.device.as_ref().ok_or(Error::NotClaimed)?;
        let bus = device.map(fd.as_fd()).map_err(Error::Refused)?;
        if !bus_range_fits(bus, len) {
            if let Err(error) = device.unmap(fd.as_fd()) {
                return Err(self.fail(now_ns, error));
            }
            return Err(Error::Refused(io::ErrorKind::InvalidInput.into()));
        }
        self.slots[index] = Some(Slot { fd, bus, len });
        self.generations[index] = self.generations[index].wrapping_add(1);
        Ok(BufferId {
            index: index as u16,
            generation: self.generations[index],
        })
    }

    /// Detaches a buffer; its id is stale from now on.
    pub fn detach(&mut self, now_ns: u64, id: BufferId) -> Result<(), Error> {
        let index = self.slot_index(id)?;
        self.detach_slot(index)
            .map_err(|error| self.fail(now_ns, error))
    }

    /// The bus address of `offset` in an attached buffer, for a range of `len` bytes that lies
    /// inside it. The only addresses this driver hands out.
    pub fn address(&self, id: BufferId, offset: u64, len: u64) -> Result<u32, Error> {
        let slot = self.slots[self.slot_index(id)?]
            .as_ref()
            .ok_or(Error::UnknownBuffer)?;
        let end = offset.checked_add(len).ok_or(Error::OutOfBuffer)?;
        if len == 0 || end > slot.len {
            return Err(Error::OutOfBuffer);
        }
        // `bus + slot.len` fits 32 bits (checked at attach), and `offset < slot.len`.
        u32::try_from(u64::from(slot.bus) + offset).map_err(|_| Error::OutOfBuffer)
    }

    /// The size of an attached buffer, in bytes.
    pub fn buffer_len(&self, id: BufferId) -> Result<u64, Error> {
        let index = self.slot_index(id)?;
        self.slots[index]
            .as_ref()
            .map(|slot| slot.len)
            .ok_or(Error::UnknownBuffer)
    }

    /// The 32-bit register at `offset` in the window. `offset` must be a multiple of 4 inside
    /// [`uapi::WINDOW_BYTES`].
    pub fn read(&self, offset: usize) -> Result<u32, Error> {
        let device = self.claimed_device()?;
        let inside = offset
            .checked_add(4)
            .is_some_and(|end| end <= uapi::WINDOW_BYTES);
        if !inside || !offset.is_multiple_of(4) {
            return Err(Error::OutOfWindow);
        }
        Ok(device.read(offset))
    }

    /// Pulses the engine's reset.
    pub fn reset(&mut self, now_ns: u64) -> Result<(), Error> {
        let result = self.claimed_device()?.reset();
        self.check(now_ns, result)
    }

    /// Sets which interrupt the kernel's handler looks at. The setting is the engine's.
    pub fn set_mode(&mut self, now_ns: u64, mode: Mode) -> Result<(), Error> {
        let result = self.claimed_device()?.set_mode(mode);
        self.check(now_ns, result)
    }

    /// Forgets an encoder interrupt not yet waited for, so the next wait is for a new one.
    pub fn clear_completion(&mut self, now_ns: u64) -> Result<(), Error> {
        let result = self.claimed_device()?.clear_completion();
        self.check(now_ns, result)
    }

    /// Waits up to `seconds` (1 to [`MAX_WAIT_S`]; others are clamped) for `completion`.
    /// `Ok(false)` is a timeout: the engine stays claimed, and the caller decides (a reset, as a
    /// rule).
    pub fn wait(
        &mut self,
        now_ns: u64,
        completion: Completion,
        seconds: u32,
    ) -> Result<bool, Error> {
        let result = self
            .claimed_device()?
            .wait(completion, seconds.clamp(1, MAX_WAIT_S));
        self.check(now_ns, result)
    }

    fn claimed_device(&self) -> Result<&D, Error> {
        match (&self.device, self.claimed) {
            (Some(device), Some(_)) => Ok(device),
            _ => Err(Error::NotClaimed),
        }
    }

    fn slot_index(&self, id: BufferId) -> Result<usize, Error> {
        if self.claimed.is_none() {
            return Err(Error::NotClaimed);
        }
        let index = usize::from(id.index);
        match self.slots.get(index) {
            Some(Some(_)) if self.generations[index] == id.generation => Ok(index),
            _ => Err(Error::UnknownBuffer),
        }
    }

    /// Detaches slot `index` if it holds a buffer. The slot is emptied even when the kernel
    /// refuses, so a stale id never reaches the buffer again.
    fn detach_slot(&mut self, index: usize) -> io::Result<()> {
        let Some(slot) = self.slots[index].take() else {
            return Ok(());
        };
        match &self.device {
            Some(device) => device.unmap(slot.fd.as_fd()),
            None => Ok(()),
        }
    }

    fn check<T>(&mut self, now_ns: u64, result: io::Result<T>) -> Result<T, Error> {
        match result {
            Ok(value) => {
                self.failures = 0;
                Ok(value)
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    fn reopen(&mut self, now_ns: u64) -> Result<(), Error> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(Error::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match (self.open)() {
            Ok(device) => {
                self.device = Some(device);
                Ok(())
            }
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// Detaches every buffer, ends the claim, and closes the file, each as far as the kernel
    /// lets it. Closing the file releases the kernel's locks; its count of users is not, so the
    /// release is asked for first.
    fn teardown(&mut self) {
        for index in 0..MAX_BUFFERS {
            let _ = self.detach_slot(index);
        }
        if let (Some(device), Some(lock)) = (&self.device, self.claimed.take()) {
            let _ = device.unlock(lock);
            let _ = device.release_engine();
        }
        self.device = None;
    }

    /// Closes the engine and schedules the next attempt.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> Error {
        self.teardown();
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = retry_at_ns;
        Error::Device { error, retry_at_ns }
    }
}

impl<D: Device, F> Drop for Block<D, F> {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            if let (Some(slot), Some(device)) = (slot.take(), &self.device) {
                let _ = device.unmap(slot.fd.as_fd());
            }
        }
        if let (Some(device), Some(lock)) = (&self.device, self.claimed.take()) {
            let _ = device.unlock(lock);
            let _ = device.release_engine();
        }
    }
}

/// A dma-buf's size, from the kernel.
#[cfg(target_os = "linux")]
fn buffer_len(buffer: BorrowedFd<'_>) -> io::Result<u64> {
    lemnos_linux_sys::dma_buf::size(buffer)
}

/// Without Linux there is no dma-buf to size.
#[cfg(not(target_os = "linux"))]
fn buffer_len(_buffer: BorrowedFd<'_>) -> io::Result<u64> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
