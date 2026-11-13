//! The engine through `lemnos.ko` (`route = "module"`): jobs that encode, as register values and
//! buffers, checked and run by the kernel module. The block's name is `cedar-ve2`, as on the
//! vendor route.
//!
//! The module drives the engine itself (it is bound to `lemnos.ko`, not to the vendor driver,
//! and listed in the module's `a733_ve` parameter). It refuses, before any register is written,
//! a write to a register the encoder's profile doesn't let a caller set, an address register
//! written as a value, an address whose buffer the session does not hold or whose range does
//! not lie inside it, and a range shorter than what the job's own geometry makes the engine
//! reach. It writes every address register itself. `docs/kernel-module.md` lists the checks;
//! this driver adds none, because none of it needs to be believed here.
//!
//! # The work
//!
//! - **Item [`TAG_WRITES`]**, exactly one: register writes, each the byte offset in the window
//!   and the value, two little-endian `u32`s, 1 to [`MAX_WRITES`] of them, written in order.
//! - **Item [`TAG_START`]**, exactly one: the value that starts the job and the time it has in
//!   milliseconds, 1 to [`MAX_TIMEOUT_MS`], two `u32`s.
//! - **Item [`TAG_READ`]**, at most one: the first register and how many, 1 to [`MAX_READ`],
//!   read back after the job.
//! - **A buffer reference** for each address register: `slot` is the register, `offset` the
//!   bytes into the buffer and `length` (not 0) how far the job may reach from there. The
//!   buffer is a stream buffer with a dma-buf handle.
//!
//! The read-back is the completion status the engine's interrupt gave, then the registers read,
//! each a little-endian `u32`.
//!
//! # Buffers and failure
//!
//! A stream buffer's dma-buf is imported into the session the first time a job names it, and
//! the session is closed when the consumer goes, which lets every one of them go (the module
//! stops the engine first if it may still use one). A job the module refuses completes as
//! refused; one that does not finish in time fails, after the module reset the engine. A
//! session that can't be opened, or that the module ends (the engine unbound, or wedged), fails
//! the job and is opened again by a later one after a backoff doubling from 250 ms to 8 s; it
//! never gives up and never turns itself off.

use std::io;
use std::os::fd::BorrowedFd;

use lemnos_contract_codec_block::BlockInfo;
use lemnos_contract_codec_block::work::Work;
use lemnos_core::Health;
use lemnos_driver_codec_jobs::{Block, Buffers, Start};
use lemnos_driver_regjob_sys::{self as regjob, Addr, Job, Write};

use crate::block::backoff_ns;
use crate::codec::{NAME, REOPENING, TAG_READ};

/// The module's control node when the board file names none.
pub const NODE: &str = "/dev/lemnos";
/// The item that carries the register writes.
pub const TAG_WRITES: u32 = 2;
/// The item that starts the job: the start value and the timeout.
pub const TAG_START: u32 = 3;
/// The most writes one job carries.
pub const MAX_WRITES: usize = regjob::MAX_WRITES;
/// The most registers one job reads back.
pub const MAX_READ: u32 = regjob::MAX_READS as u32;
/// The longest a job may take: the module allows 10 s, but a job runs inside the driver's call,
/// which the host bounds at 2 s, so a job here gets 1 s at most (an encode takes milliseconds).
pub const MAX_TIMEOUT_MS: u32 = 1000;
const _: () = assert!(MAX_TIMEOUT_MS <= regjob::MAX_TIMEOUT_MS);
/// What health says when the module says the engine may still be using memory.
pub const WEDGED: &str = "the video engine did not finish a job and could not be reset: its buffers are held until a reboot";

/// A session with the module, one request each. The Linux one is [`ModuleEngine`]; tests play
/// one back.
pub trait Engine {
    /// Imports `buffer`; the index a job's address names.
    fn import(&mut self, buffer: BorrowedFd<'_>) -> io::Result<u32>;
    /// Runs `job`; its reads land in `values`. The completion status.
    fn run(&mut self, job: &Job<'_>, values: &mut [u32]) -> io::Result<u32>;
}

/// The module's session on the Linux side.
#[derive(Debug)]
pub struct ModuleEngine(regjob::Session);

impl ModuleEngine {
    /// Opens the session on the block whose device name is `block`, through the control node
    /// at `node`. `InvalidData` when the block isn't run with the encoder's profile.
    pub fn open(node: &std::path::Path, block: &str) -> io::Result<Self> {
        let control = regjob::Control::open(node)?;
        let session = control.open_session(control.find_block(block)?)?;
        if session.profile() != regjob::PROFILE_A733_VE {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Self(session))
    }
}

impl Engine for ModuleEngine {
    fn import(&mut self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        self.0.import(buffer).map(|imported| imported.index)
    }

    fn run(&mut self, job: &Job<'_>, values: &mut [u32]) -> io::Result<u32> {
        self.0.run(job, values)
    }
}

/// What the module answered for a job.
fn refusal(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::EPERM | libc::EINVAL | libc::ENOENT | libc::EFAULT | libc::ENOSPC)
    )
}

/// A little-endian `u32` at `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

/// The engine as `codec.block/1` has it, through the module. Opened when a job comes.
pub struct ModuleBlock<E, F> {
    open: F,
    engine: Option<E>,
    /// Session failures in a row, and when the next open may be tried.
    failures: u32,
    retry_at_ns: u64,
    wedged: bool,
    /// The session's index for each stream buffer a job has named.
    ids: [Option<u32>; 64],
    writes: Box<[Write]>,
    addrs: [Addr; regjob::MAX_ADDRS],
    reads: [u32; regjob::MAX_READS],
    values: [u32; regjob::MAX_READS],
}

impl<E: Engine, F: FnMut() -> io::Result<E>> ModuleBlock<E, F> {
    /// The block `open` opens a session on. Nothing is opened until a job comes.
    pub fn new(open: F) -> Self {
        Self {
            open,
            engine: None,
            failures: 0,
            retry_at_ns: 0,
            wedged: false,
            ids: [None; 64],
            writes: vec![Write::default(); MAX_WRITES].into_boxed_slice(),
            addrs: [Addr::default(); regjob::MAX_ADDRS],
            reads: [0; regjob::MAX_READS],
            values: [0; regjob::MAX_READS],
        }
    }

    /// Closes the session after a failure and backs off.
    fn fail(&mut self, now_ns: u64) -> Start {
        self.engine = None;
        self.ids = [None; 64];
        self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        Start::Failed
    }

    /// Reads the work into the job's arrays: (writes, addresses, reads, start, timeout), or
    /// `None` when it isn't this block's work.
    fn parse(&mut self, work: &Work<'_>) -> Option<(usize, usize, usize, u32, u32)> {
        let (mut writes, mut start, mut reads) = (None, None, None);
        for (tag, bytes) in work.items() {
            match tag {
                TAG_WRITES if writes.is_none() => {
                    let pairs = bytes.as_chunks::<8>();
                    if !pairs.1.is_empty() || pairs.0.is_empty() || pairs.0.len() > MAX_WRITES {
                        return None;
                    }
                    for (into, pair) in self.writes.iter_mut().zip(pairs.0) {
                        *into = Write {
                            offset: word(pair, 0)?,
                            value: word(pair, 4)?,
                        };
                    }
                    writes = Some(pairs.0.len());
                }
                TAG_START if start.is_none() && bytes.len() == 8 => {
                    let timeout = word(bytes, 4)?;
                    if !(1..=MAX_TIMEOUT_MS).contains(&timeout) {
                        return None;
                    }
                    start = Some((word(bytes, 0)?, timeout));
                }
                TAG_READ if reads.is_none() && bytes.len() == 8 => {
                    let (first, count) = (word(bytes, 0)?, word(bytes, 4)?);
                    if !(1..=MAX_READ).contains(&count) {
                        return None;
                    }
                    for (n, into) in (0..count).zip(self.reads.iter_mut()) {
                        *into = first.checked_add(n.checked_mul(4)?)?;
                    }
                    reads = Some(count as usize);
                }
                _ => return None,
            }
        }
        let refs = work.ref_count();
        if refs == 0 || refs > regjob::MAX_ADDRS {
            return None;
        }
        let (start, timeout) = start?;
        Some((writes?, refs, reads.unwrap_or(0), start, timeout))
    }
}

impl<E: Engine, F> ModuleBlock<E, F> {
    /// Imports what the job's references name and runs it on `engine`: the status, or
    /// `Err(None)` for a reference this block refuses and `Err(Some(_))` for what the module
    /// answered.
    fn submit(
        &mut self,
        engine: &mut E,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        (nwrites, naddrs, nreads, start, timeout_ms): (usize, usize, usize, u32, u32),
    ) -> Result<u32, Option<io::Error>> {
        for (reference, addr) in work.refs().zip(self.addrs.iter_mut()) {
            if reference.length == 0 {
                return Err(None);
            }
            let slot = self.ids.get_mut(reference.buffer as usize).ok_or(None)?;
            let index = match *slot {
                Some(index) => index,
                None => {
                    let handle = buffers.handle(reference.buffer).ok_or(None)?;
                    *slot.insert(engine.import(handle.fd()).map_err(Some)?)
                }
            };
            *addr = Addr {
                reg: reference.slot,
                buffer: index,
                offset: u64::from(reference.offset),
                bytes: u64::from(reference.length),
            };
        }
        let job = Job {
            writes: &self.writes[..nwrites],
            addrs: &self.addrs[..naddrs],
            reads: &self.reads[..nreads],
            start,
            timeout_ms,
        };
        engine.run(&job, &mut self.values[..nreads]).map_err(Some)
    }
}

impl<E: Engine, F: FnMut() -> io::Result<E>> Block for ModuleBlock<E, F> {
    fn name(&self) -> &str {
        NAME
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            // The items' headers, the writes, the start and the read, and the references.
            max_work_bytes: 9 * 1024,
            max_refs: regjob::MAX_ADDRS as u16,
            max_items: 3,
            max_read_back: 4 + 4 * MAX_READ,
            max_buffers: 64,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        let Some((nwrites, naddrs, nreads, start, timeout_ms)) = self.parse(work) else {
            return Start::Refused;
        };
        let Some(into) = read_back.get_mut(..4 + 4 * nreads) else {
            return Start::Failed;
        };
        let mut engine = match self.engine.take() {
            Some(engine) => engine,
            None if self.failures > 0 && now_ns < self.retry_at_ns => return Start::Failed,
            None => match (self.open)() {
                Ok(engine) => engine,
                Err(_) => return self.fail(now_ns),
            },
        };
        let ran = self.submit(
            &mut engine,
            work,
            buffers,
            (nwrites, naddrs, nreads, start, timeout_ms),
        );
        self.engine = Some(engine);
        match ran {
            Ok(status) => {
                self.failures = 0;
                self.wedged = false;
                let (head, tail) = into.split_at_mut(4);
                head.copy_from_slice(&status.to_le_bytes());
                for (four, value) in tail.as_chunks_mut::<4>().0.iter_mut().zip(&self.values) {
                    *four = value.to_le_bytes();
                }
                Start::Done(4 + 4 * nreads)
            }
            Err(None) => Start::Refused,
            Err(Some(err)) if refusal(&err) => Start::Refused,
            // The module reset the engine: the session is as good as before.
            Err(Some(err)) if err.raw_os_error() == Some(libc::ETIMEDOUT) => Start::Failed,
            Err(Some(err)) => {
                self.wedged = err.raw_os_error() == Some(libc::EIO);
                self.fail(now_ns)
            }
        }
    }

    fn release(&mut self) {
        // Closing the session lets every buffer of the consumer's go.
        self.engine = None;
        self.ids = [None; 64];
    }

    fn health(&self) -> Health {
        if self.wedged {
            Health::Degraded(WEDGED)
        } else if self.failures > 0 {
            Health::Degraded(REOPENING)
        } else {
            Health::Ok
        }
    }
}

#[cfg(test)]
mod tests;
