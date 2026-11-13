//! A block as `codec.block/1` has it: what a job's work is made of, and how it becomes a
//! [`Job`] on a claim.
//!
//! The block's name is `rk-mpp:` and the block ([`block_name`]): `rk-mpp:vdpu2`. Its work is:
//!
//! - **Item [`TAG_WRITE`]**, exactly one: the first register as a `u32`, then the register
//!   values, each a little-endian `u32`. They are the caller's and are carried as they are,
//!   except the registers its references name.
//! - **Item [`TAG_READ`]**, at most one: the first register and how many, two `u32`s. The
//!   registers come back as the job's read-back, each a little-endian `u32`.
//! - **A buffer reference** for each register that holds a buffer's address: `slot` is the
//!   register, `offset` the bytes into the buffer. `length` may say how much of the buffer the
//!   job means to use from there and is checked to lie inside it; 0 leaves it out. The
//!   driver puts the buffer in the register; whatever the work wrote there is replaced.
//!
//! Everything else is refused: another tag, an item of another shape, a register that doesn't
//! fit 16 bits. Then the job goes through [`crate::Claim::submit`], whose checks are the ones
//! [`crate::job`] lists (the write and the read inside the block's registers, each address a
//! register of the write named once, its buffer one the claim holds, its offset inside the
//! buffer's measured size, the translation table exactly the job's addresses).
//!
//! # Buffers
//!
//! A reference's buffer is a buffer of the job stream. The first job that names one takes its
//! dma-buf from the stream, duplicates the descriptor and adds it to the claim, which measures
//! its real size; later jobs use the same claim buffer. A stream buffer without a handle
//! (a plain stream, or a consumer that has gone) refuses the job. At most
//! [`crate::MAX_BUFFERS`] stream buffers can be named, the first ones.
//!
//! # One consumer, one claim
//!
//! The claim is taken when the first job comes and given up when the consumer goes
//! ([`Block::release`]): the kernel then unmaps everything the session mapped, so the next
//! consumer's jobs run on a session that has only its buffers. A claim that can't be taken
//! fails the job and is tried again by the next one, after the service's own backoff
//! (doubling to 8 s; it never gives up). A job runs to its end inside
//! [`Block::start`]: the kernel's poll waits for the task, and bounds the wait itself.

use std::io;
use std::os::fd::BorrowedFd;

use lemnos_contract_codec_block::BlockInfo;
use lemnos_contract_codec_block::work::Work;
use lemnos_core::Health;
use lemnos_driver_codec_jobs::{Block, Buffers, Start};

use crate::block::{MAX_JOB_WORDS, MAX_REGISTERS};
use crate::claim::{ClaimError, Device, MAX_BUFFERS, OwnedClaim, Service, Session, SubmitError};
use crate::job::{Address, BufferId, Job, MAX_ADDRESSES, MAX_RUNS, ReadBack, Run};
use crate::message::Message;

/// The item that carries the register values: the first register, then the values.
pub const TAG_WRITE: u32 = 1;
/// The item that names the registers to read back: the first, and how many.
pub const TAG_READ: u32 = 2;

/// What health says while the block's claim can't be taken because the node isn't answering.
pub const NOT_THERE: &str = "the codec service did not take a claim: tried again by the next job";
/// What health says when the chip has no such block.
pub const ABSENT: &str = "the codec service has no such block on this chip";
/// What health says when the block is not behind an IOMMU, so nothing would confine a job.
pub const NOT_CONFINED: &str =
    "the block is not behind an IOMMU: nothing would confine a job, so none is run";

/// The name `codec.block/1` gives `block`.
#[must_use]
pub const fn block_name(block: crate::Block) -> &'static str {
    match block {
        crate::Block::Vdpu1 => "rk-mpp:vdpu1",
        crate::Block::Vdpu2 => "rk-mpp:vdpu2",
        crate::Block::Vepu1 => "rk-mpp:vepu1",
        crate::Block::Vepu2 => "rk-mpp:vepu2",
        crate::Block::JpegDecoder => "rk-mpp:jpeg-decoder",
        crate::Block::JpegEncoder => "rk-mpp:jpeg-encoder",
        crate::Block::Rkvenc => "rk-mpp:rkvenc",
        crate::Block::Rkvenc2 => "rk-mpp:rkvenc2",
        crate::Block::Iep2 => "rk-mpp:iep2",
    }
}

/// The register-file block a board file names: the part of [`block_name`] after `rk-mpp:`. A
/// parameter block (`iep2`) is not one: its jobs are served by its own driver.
#[must_use]
pub fn block_named(name: &str) -> Option<crate::Block> {
    crate::Block::ALL
        .into_iter()
        .filter(|block| block.params().is_none())
        .find(|&block| block_name(block).strip_prefix("rk-mpp:") == Some(name))
}

/// A session behind a box, so a driver can hold a device of any kind.
pub type BoxedSession = Box<dyn Session + Send>;

impl Session for BoxedSession {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        (**self).send(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        (**self).read_back(slot, into);
    }
}

/// [`Device`] with its session boxed, which is what makes it usable behind a box.
trait Erased: Send {
    fn open(&mut self) -> io::Result<BoxedSession>;
    fn confined(&mut self, block: crate::Block) -> io::Result<bool>;
    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64>;
}

impl<D> Erased for D
where
    D: Device + Send,
    D::Session: Send + 'static,
{
    fn open(&mut self) -> io::Result<BoxedSession> {
        Ok(Box::new(Device::open(self)?))
    }

    fn confined(&mut self, block: crate::Block) -> io::Result<bool> {
        Device::confined(self, block)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        Device::buffer_size(self, fd)
    }
}

/// A [`Device`] of any kind behind a box: the real node, or a test's stand-in.
pub struct BoxedDevice(Box<dyn Erased>);

impl BoxedDevice {
    /// `device`, boxed.
    pub fn new<D>(device: D) -> Self
    where
        D: Device + Send + 'static,
        D::Session: Send + 'static,
    {
        Self(Box::new(device))
    }
}

impl Device for BoxedDevice {
    type Session = BoxedSession;

    fn open(&mut self) -> io::Result<BoxedSession> {
        self.0.open()
    }

    fn confined(&mut self, block: crate::Block) -> io::Result<bool> {
        self.0.confined(block)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        self.0.buffer_size(fd)
    }
}

/// The service, claimed or not.
enum Link<D: Device> {
    Idle(Box<Service<D>>),
    Claimed(Box<OwnedClaim<D>>),
    /// Between the two, for the length of a call.
    Moving,
}

/// One block's claim as a `codec.block/1` driver holds it: taken when a consumer's first job
/// comes, given up when the consumer goes, and the claim's buffer for each stream buffer a job
/// has named. [`MppBlock`] serves register-file blocks with it; a parameter block's driver
/// (`lemnos-driver-rk-iep`) serves its own work format with it.
pub struct BlockClaim<D: Device> {
    block: crate::Block,
    link: Link<D>,
    /// The claim's buffer for each stream buffer a job has named.
    ids: [Option<BufferId>; MAX_BUFFERS],
    fault: Option<&'static str>,
}

impl<D: Device> BlockClaim<D> {
    /// `block` of the service on `device`. Nothing is opened until [`Self::claim`].
    pub fn new(device: D, block: crate::Block) -> Self {
        Self {
            block,
            link: Link::Idle(Box::new(Service::new(device))),
            ids: [None; MAX_BUFFERS],
            fault: None,
        }
    }

    /// The block.
    pub fn block(&self) -> crate::Block {
        self.block
    }

    /// Whether the claim is held.
    pub fn claimed(&self) -> bool {
        matches!(self.link, Link::Claimed(_))
    }

    /// The claim, taken now if it isn't held. `None` when it can't be taken: the reason is in
    /// [`Self::health`], and the next call tries again behind the service's backoff.
    pub fn claim(&mut self, now_ns: u64) -> Option<&mut OwnedClaim<D>> {
        if let Link::Idle(_) = self.link {
            let Link::Idle(service) = std::mem::replace(&mut self.link, Link::Moving) else {
                return None;
            };
            match service.claim_owned(self.block, now_ns) {
                Ok(claim) => {
                    self.fault = None;
                    self.link = Link::Claimed(Box::new(claim));
                }
                Err((service, error)) => {
                    self.fault = Some(match error {
                        ClaimError::Absent => ABSENT,
                        ClaimError::NotConfined => NOT_CONFINED,
                        _ => NOT_THERE,
                    });
                    self.link = Link::Idle(service);
                }
            }
        }
        self.held()
    }

    /// The claim, if it is held.
    pub fn held(&mut self) -> Option<&mut OwnedClaim<D>> {
        match &mut self.link {
            Link::Claimed(claim) => Some(&mut **claim),
            _ => None,
        }
    }

    /// The claim's buffer for stream buffer `index`: the one an earlier job made, or the
    /// stream buffer's dma-buf, its descriptor duplicated and added to the claim now (which
    /// measures its real size). [`Start::Refused`] for a buffer the claim can't take (no
    /// handle, out of range, the claim full), [`Start::Failed`] when no claim is held or the
    /// descriptor can't be duplicated.
    pub fn buffer(&mut self, index: u32, buffers: &Buffers<'_>) -> Result<BufferId, Start> {
        let Link::Claimed(claim) = &mut self.link else {
            return Err(Start::Failed);
        };
        let Some(slot) = self.ids.get_mut(index as usize) else {
            return Err(Start::Refused);
        };
        if let Some(id) = *slot {
            return Ok(id);
        }
        let Some(handle) = buffers.handle(index) else {
            return Err(Start::Refused);
        };
        let Ok(fd) = handle.fd().try_clone_to_owned() else {
            return Err(Start::Failed);
        };
        let Ok(id) = claim.add_buffer(fd) else {
            return Err(Start::Refused);
        };
        *slot = Some(id);
        Ok(id)
    }

    /// Gives the claim up, keeping the service and its backoff: the kernel unmaps every buffer
    /// the session mapped, and the next claim starts with none.
    pub fn unclaim(&mut self) {
        self.ids = [None; MAX_BUFFERS];
        if let Link::Claimed(_) = self.link {
            let Link::Claimed(claim) = std::mem::replace(&mut self.link, Link::Moving) else {
                return;
            };
            if let Some(service) = claim.release() {
                self.link = Link::Idle(service);
            }
        }
    }

    /// `Ok`, or `Degraded` with why the last claim wasn't taken.
    pub fn health(&self) -> Health {
        match self.fault {
            None => Health::Ok,
            Some(reason) => Health::Degraded(reason),
        }
    }
}

/// One block of the service as `codec.block/1` has it.
pub struct MppBlock<D: Device> {
    claim: BlockClaim<D>,
    /// A job's addresses and read-back, built here.
    addresses: [Address; MAX_ADDRESSES],
    registers: Box<[u32; MAX_JOB_WORDS]>,
    identity: u64,
}

impl<D: Device> MppBlock<D> {
    /// `block` of the service on `device`. Nothing is opened until a job comes.
    pub fn new(device: D, block: crate::Block) -> Self {
        let nowhere = Address {
            register: 0,
            buffer: BufferId {
                slot: 0,
                generation: 0,
            },
            offset: 0,
        };
        Self {
            claim: BlockClaim::new(device, block),
            addresses: [nowhere; MAX_ADDRESSES],
            registers: Box::new([0; MAX_JOB_WORDS]),
            identity: 0,
        }
    }

    /// Whether the block holds its claim.
    pub fn claimed(&self) -> bool {
        self.claim.claimed()
    }
}

/// A little-endian `u32` at `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

impl<D: Device> Block for MppBlock<D> {
    fn name(&self) -> &str {
        block_name(self.claim.block())
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: (4 * MAX_JOB_WORDS + 8 * MAX_RUNS + 8) as u32,
            max_refs: MAX_ADDRESSES as u16,
            max_items: (MAX_RUNS + 1) as u16,
            max_read_back: (4 * MAX_REGISTERS) as u32,
            max_buffers: MAX_BUFFERS as u32,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        // The work's shape first: nothing is opened for work that is refused anyway. Each write
        // item is one run: its first register, then its values, copied to the block's staging
        // area one after another.
        let mut runs = [(0u16, 0usize, 0usize); MAX_RUNS];
        let mut count_runs = 0;
        let mut words = 0;
        let mut read = None;
        for (tag, bytes) in work.items() {
            match tag {
                TAG_WRITE if count_runs < MAX_RUNS && bytes.len() >= 8 && bytes.len() % 4 == 0 => {
                    let count = bytes.len() / 4 - 1;
                    let first = word(bytes, 0).and_then(|first| u16::try_from(first).ok());
                    let Some(first) = first else {
                        return Start::Refused;
                    };
                    if words + count > MAX_JOB_WORDS {
                        return Start::Refused;
                    }
                    for (at, value) in self.registers[words..words + count].iter_mut().enumerate() {
                        *value = word(bytes, 4 + 4 * at).unwrap_or(0);
                    }
                    runs[count_runs] = (first, words, count);
                    count_runs += 1;
                    words += count;
                }
                TAG_READ if read.is_none() && bytes.len() == 8 => read = Some(bytes),
                _ => return Start::Refused,
            }
        }
        if count_runs == 0 {
            return Start::Refused;
        }
        let read = match read {
            None => None,
            Some(bytes) => {
                let first = word(bytes, 0).and_then(|v| u16::try_from(v).ok());
                let count = word(bytes, 4).and_then(|v| u16::try_from(v).ok());
                let (Some(first), Some(count)) = (first, count) else {
                    return Start::Refused;
                };
                Some(ReadBack { first, count })
            }
        };
        if work.ref_count() > MAX_ADDRESSES {
            return Start::Refused;
        }
        if self.claim.claim(now_ns).is_none() {
            return Start::Failed;
        }

        // Each reference: a register, and a stream buffer the claim holds or takes now.
        let mut named = 0;
        for reference in work.refs() {
            let Ok(register) = u16::try_from(reference.slot) else {
                return Start::Refused;
            };
            let id = match self.claim.buffer(reference.buffer, buffers) {
                Ok(id) => id,
                Err(start) => return start,
            };
            let Some(claim) = self.claim.held() else {
                return Start::Failed;
            };
            // The offset is checked by the claim; the length, when one is given, here.
            let end = u64::from(reference.offset) + u64::from(reference.length);
            if claim.buffer_size(id).is_none_or(|size| end > size) {
                return Start::Refused;
            }
            self.addresses[named] = Address {
                register,
                buffer: id,
                offset: reference.offset,
            };
            named += 1;
        }

        self.identity = self.identity.wrapping_add(1);
        let runs = &runs[..count_runs];
        let registers = &self.registers[..];
        let mut more = [Run {
            first: 0,
            registers: &[],
        }; MAX_RUNS];
        for (slot, &(first, from, count)) in more.iter_mut().zip(&runs[1..]) {
            *slot = Run {
                first,
                registers: &registers[from..from + count],
            };
        }
        let (first, from, count) = runs[0];
        let job = Job {
            identity: self.identity,
            first,
            registers: &registers[from..from + count],
            more: &more[..runs.len() - 1],
            addresses: &self.addresses[..named],
            read,
        };
        let Some(claim) = self.claim.held() else {
            return Start::Failed;
        };
        match claim.submit(&job) {
            Ok(()) => {}
            Err(SubmitError::Job(_)) => return Start::Refused,
            Err(_) => {
                // The kernel refused the call, or the session is in a state nobody knows:
                // the next job starts on a new one.
                self.claim.unclaim();
                return Start::Failed;
            }
        }
        let done = claim.complete(&mut self.registers[..]);
        let Ok(done) = done else {
            self.claim.unclaim();
            return Start::Failed;
        };
        let bytes = 4 * done.registers;
        let Some(into) = read_back.get_mut(..bytes) else {
            return Start::Failed;
        };
        for (four, register) in into
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&self.registers[..])
        {
            *four = register.to_le_bytes();
        }
        Start::Done(bytes)
    }

    fn release(&mut self) {
        self.claim.unclaim();
    }

    fn health(&self) -> Health {
        self.claim.health()
    }
}
