//! The node, a claim on it, programs and their buffers, and runs.
//!
//! # What the IOMMU maps while a job runs
//!
//! Read from rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`,
//! `drivers/rknpu/`:
//!
//! - Domains belong to the device, not to an open file. There are up to
//!   [`crate::uapi::DOMAINS`], numbered from 0 (`rknpu_dev->iommu_domains[]`, `rknpu_drv.h`);
//!   every opener of the node shares them, and a domain, once made, lives until the driver is
//!   removed (`rknpu_iommu_free_domains`).
//! - An object made with `MEM_CREATE` is mapped into the domain its request names, by switching
//!   the whole device to that domain first (`rknpu_gem_object_create` calls
//!   `rknpu_iommu_domain_get_and_switch`), and stays mapped there until the object is freed:
//!   its handle closed and every other hold (a CPU mapping, a job) dropped
//!   (`rknpu_gem_object_destroy`, switching back to the object's domain to unmap it). Nothing is
//!   mapped or unmapped per job, and closing a file frees only that file's handles.
//! - A job runs in the domain its request names: `rknpu_job_schedule` switches the device to it,
//!   waiting while jobs of another domain hold the device, and the job holds the domain until it
//!   ends (`rknpu_job_done`, `rknpu_job_abort`). So while a job runs, the IOMMU maps exactly the
//!   objects alive in that domain, whoever made them.
//! - The kernel can be built without domain switching (`rknpu_iommu.c` switches only with
//!   `CONFIG_IOMMU_API`, `CONFIG_NO_GKI` and not `CONFIG_ARM`). Then every id is the same single
//!   domain and setting one changes nothing; the id read back stays 0.
//! - An imported dma-buf (`PRIME_FD_TO_HANDLE`) is mapped into whichever domain the device is in
//!   at the time, but recorded as domain 0 (`rknpu_gem_prime_import_sg_table` never sets
//!   `iommu_domain_id`), so freeing it unmaps from domain 0 and leaves its pages mapped in the
//!   domain it went into. The driver therefore imports nothing: see "Copies" below.
//!
//! # How a job is confined
//!
//! Each program gets a domain of its own, made of nothing but its own objects: its body, its
//! scratch, and one object per attached slot. Program `n` (from 1) lives in domain `n`; domain 0,
//! the device's own, is never used while the kernel switches domains. A run names its program's
//! domain, so while it runs the IOMMU maps those objects and no others. The checks that keep it
//! so:
//!
//! - **One claim per service, one service per node.** A [`Service`] gives out one [`Claim`] at a
//!   time ([`ClaimError::Claimed`] while one is alive). No other process may open the node: the kernel's domains are shared by every opener, so an
//!   object another opener makes in a program's domain would be mapped during its runs, and
//!   nothing this side can see it. That is a property of the board's device permissions, not of
//!   this crate (and another opener of the node can already make the kernel follow any address
//!   it likes as `task_obj_addr`, so the node must be the daemon's alone in any case).
//! - **A program is loaded only into a domain holding nothing.** The claim keeps a ledger of
//!   every object it made and has not seen freed, with its domain. A domain with any entry,
//!   another program's or one whose destroy failed, is not given out (`BUSY`).
//! - **A run starts only if every object in its domain is its program's.** The ledger is checked
//!   again before each run; an object left over from a failed destroy refuses runs in its domain
//!   until a later destroy succeeds.
//! - **A kernel without domain switching shares one domain**: then one program at a time, by the
//!   same rule.
//! - **No IOMMU, no claim.** `RKNPU_GET_IOMMU_EN` must answer 1; without it an address is a
//!   physical address and nothing confines anything.
//! - **Runs are one at a time and block.** The kernel lets go of a timed-out job's domain before
//!   it resets the block (`rknpu_job_abort`), so a job of another domain submitted in between
//!   could switch the IOMMU under a block still running. A blocking submit returns only after the
//!   reset, and the claim submits nothing else meanwhile.
//!
//! # Copies
//!
//! A slot's buffer is on the job stream, not in the program's domain. Before a run the driver
//! copies each attached buffer into the slot's own object and zeroes the rest of it; after the
//! run it copies the object back. That is the price of the domain rule while the kernel cannot
//! unmap an imported dma-buf from the domain it mapped it into. Closing that gap (a kernel that
//! records the domain an import went into, or a Lemnos kernel path for the block) would let the
//! stream's own dma-bufs be imported per program instead.
//!
//! # Failures
//!
//! A claim that fails to open starts a backoff: until it is due, [`Service::claim`] answers
//! [`ClaimError::BackingOff`] without touching the device. The delay doubles from 250 ms, is
//! capped at 8 s, and clears when a claim is taken. The service never gives up. A run the block
//! does not finish completes as [`RunStatus::Failed`] and the claim stays: the kernel has reset
//! the block before the call returns. A session call the kernel answers with `ENODEV` (the node's
//! device was removed) marks the claim [`Claim::lost`]; its holder drops it and claims again.
//!
//! # Allocation
//!
//! Taking a claim, loading and attaching allocate. A run allocates nothing.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lemnos_contract_npu::{
    Counters, JOB_BYTES, MAX_STREAM_NAME, Npu, NpuInfo, RESULT_BYTES, RunJob, RunResult, RunStatus,
};
use lemnos_core::{Decode, Encode, Reader, Status, Writer};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::program::{
    HEADER_BYTES, Header, MAX_PROGRAM_BYTES, MAX_SLOTS, RELOCATION_BYTES, Relocation, Target,
};
use crate::request::{Create, Domain, Kind, Object, Query, Run};
use crate::uapi::DOMAINS;

/// Programs loaded at once, at most: one per domain other than the device's own.
pub const MAX_PROGRAMS: usize = DOMAINS as usize - 1;

/// Buffers of a stream this driver can hold.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken in one [`Claim::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// The longest job read off the queue: a longer one is taken off and faults its consumer.
const LONGEST_JOB: usize = 256;

/// `ENODEV` (`include/uapi/asm-generic/errno-base.h`): what the kernel answers on a file whose
/// device was removed.
const ENODEV: i32 = 19;

/// First delay before a claim is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay after `failures` failures in a row (from 1), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(failures: u32) -> u64 {
    let attempt = if failures == 0 { 0 } else { failures - 1 };
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// One open of the node.
///
/// The implementation holds the `unsafe` and the kernel's object addresses. It must issue only
/// what it is given, and check each request against the objects it made: a [`Run`]'s body must
/// be an object of this session, alive, made as [`Kind::Body`] in the run's domain and at least
/// the run's task array long; a handle given to [`Self::memory`] or [`Self::destroy`] must be one
/// it made.
pub trait Session {
    /// Sends `RKNPU_ACTION` and returns the value the kernel answers.
    fn query(&mut self, query: Query) -> io::Result<u32>;

    /// Makes an object (`MEM_CREATE`) and maps it for the CPU (`MEM_MAP`, `mmap`), zeroed.
    fn create(&mut self, request: Create) -> io::Result<Object>;

    /// The CPU's mapping of an object this session made, [`Object::size`] long.
    fn memory(&mut self, handle: u32) -> Option<&mut [u8]>;

    /// Unmaps the CPU's mapping, then closes the handle (`MEM_DESTROY`). After an error the
    /// handle is still the session's and may be destroyed again.
    fn destroy(&mut self, handle: u32) -> io::Result<()>;

    /// Runs a job (`SUBMIT`, blocking) and returns when the block has finished or been reset.
    fn submit(&mut self, run: &Run) -> io::Result<()>;
}

/// The node.
pub trait Device {
    /// A session on it.
    type Session: Session;

    /// Opens a new session.
    fn open(&mut self) -> io::Result<Self::Session>;
}

/// How the kernel the claim found keeps domains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domains {
    /// It switches domains: each program gets its own.
    PerProgram,
    /// Built without switching: one domain for everything, so one program at a time.
    Shared,
}

/// Why a claim was not taken.
#[derive(Debug)]
pub enum ClaimError {
    /// The block is not behind an IOMMU: nothing would confine a program's addresses.
    NotConfined,
    /// Opening or asking the node failed; the next claim may be tried from `retry_at_ns`.
    Device {
        /// What failed.
        error: io::Error,
        /// When, on the caller's clock.
        retry_at_ns: u64,
    },
    /// An earlier failure's backoff has not run out. The device was not touched.
    BackingOff {
        /// When the next claim may be tried.
        retry_at_ns: u64,
    },
    /// A claim taken on this service is still alive.
    Claimed,
}

/// The node, through `D`.
#[derive(Debug)]
pub struct Service<D: Device> {
    device: D,
    failures: u32,
    retry_at_ns: u64,
    /// Set while a claim taken here is alive; the claim clears it when dropped.
    claimed: Arc<AtomicBool>,
}

impl<D: Device> Service<D> {
    /// The service on `device`. Nothing is opened.
    pub fn new(device: D) -> Self {
        Self {
            device,
            failures: 0,
            retry_at_ns: 0,
            claimed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The device.
    pub fn device(&self) -> &D {
        &self.device
    }

    /// Takes the claim: a session, checked to have the block behind an IOMMU, and the kernel's
    /// domain switching found out. `now_ns` is the caller's clock, for the backoff.
    pub fn claim(&mut self, now_ns: u64) -> Result<Claim<D>, ClaimError> {
        if self.claimed.load(Ordering::Acquire) {
            return Err(ClaimError::Claimed);
        }
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(ClaimError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match self.open() {
            Ok(Some((session, domains))) => {
                self.failures = 0;
                self.claimed.store(true, Ordering::Release);
                Ok(Claim::new(Arc::clone(&self.claimed), session, domains))
            }
            Ok(None) => Err(ClaimError::NotConfined),
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
                Err(ClaimError::Device {
                    error,
                    retry_at_ns: self.retry_at_ns,
                })
            }
        }
    }

    fn open(&mut self) -> io::Result<Option<(D::Session, Domains)>> {
        let mut session = self.device.open()?;
        if session.query(Query::IommuEnabled)? != 1 {
            return Ok(None);
        }
        // A kernel that switches reads back the domain it was set to; one built without
        // switching answers the set and reads back 0.
        let probe = Domain::new(1).ok_or_else(|| io::Error::other("no domain 1"))?;
        session.query(Query::SetDomain(probe))?;
        let domains = if session.query(Query::Domain)? == probe.id() {
            Domains::PerProgram
        } else {
            Domains::Shared
        };
        Ok(Some((session, domains)))
    }
}

/// Why the last failed job failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job. The consumer's stream was faulted.
    Undecodable,
    /// The result buffer is not one the driver holds, so the job cannot complete. One that is
    /// not on the stream at all faulted the consumer's stream.
    Result,
    /// Refused before it ran: an unknown program, a slot not attached, the result buffer
    /// attached, or an object in the program's domain that is not the program's.
    Refused,
    /// A buffer could not be copied in or out.
    Copy,
    /// The kernel refused the job or the block did not finish it.
    Device,
    /// The stream refused the completion.
    Completion,
}

/// One object the claim made and has not seen freed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    handle: u32,
    domain: Domain,
    /// The program it belongs to, by index; `None` once its destroy failed and no program
    /// holds it, until a later destroy succeeds.
    owner: Option<usize>,
}

#[derive(Debug)]
struct Attached {
    buffer: u32,
    bytes: u32,
    object: Object,
}

#[derive(Debug)]
struct Program {
    domain: Domain,
    header: Header,
    body: Object,
    scratch: Option<Object>,
    relocations: Vec<Relocation>,
    run: Run,
    attached: [Option<Attached>; MAX_SLOTS as usize],
}

/// The claim on the node: a session, its programs and objects, and the stream's buffers held.
pub struct Claim<D: Device> {
    session: D::Session,
    domains: Domains,
    programs: [Option<Program>; MAX_PROGRAMS],
    ledger: Vec<Entry>,
    /// The stream's buffers this driver holds, one bit each.
    held: u64,
    counters: Counters,
    last_failure: Option<Failure>,
    sequence: u64,
    /// Whether the kernel said the node's device is gone.
    lost: bool,
    /// The service's flag, cleared when the claim is dropped.
    claimed: Arc<AtomicBool>,
}

impl<D: Device> std::fmt::Debug for Claim<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Claim")
            .field("domains", &self.domains)
            .field("ledger", &self.ledger)
            .field("held", &self.held)
            .field("counters", &self.counters)
            .field("last_failure", &self.last_failure)
            .finish_non_exhaustive()
    }
}

fn program_index(program: u32) -> Option<usize> {
    let index = usize::try_from(program).ok()?.checked_sub(1)?;
    (index < MAX_PROGRAMS).then_some(index)
}

impl<D: Device> Claim<D> {
    fn new(claimed: Arc<AtomicBool>, session: D::Session, domains: Domains) -> Self {
        Self {
            session,
            domains,
            programs: std::array::from_fn(|_| None),
            ledger: Vec::with_capacity(MAX_PROGRAMS * (MAX_SLOTS as usize + 2)),
            held: 0,
            counters: Counters::default(),
            last_failure: None,
            sequence: 0,
            lost: false,
            claimed,
        }
    }

    /// Whether the kernel answered a call with `ENODEV`: the node's device is gone, and nothing
    /// on this session will work again. Drop the claim and take a new one.
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// Notes an error from the session.
    fn seen(&mut self, error: &io::Error) {
        self.lost |= error.raw_os_error() == Some(ENODEV);
    }

    /// The session.
    pub fn session(&self) -> &D::Session {
        &self.session
    }

    /// How the kernel keeps domains.
    pub fn domains(&self) -> Domains {
        self.domains
    }

    /// What the block takes.
    pub fn info(&self) -> NpuInfo {
        NpuInfo {
            max_program_bytes: MAX_PROGRAM_BYTES,
            max_programs: match self.domains {
                Domains::PerProgram => MAX_PROGRAMS as u16,
                Domains::Shared => 1,
            },
            max_attached: MAX_SLOTS,
        }
    }

    /// Runs completed and failed since the claim was taken.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Why the last failed job failed.
    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }

    /// The domain program `program` (from 1) runs in, if it is loaded.
    pub fn program_domain(&self, program: u32) -> Option<Domain> {
        let index = program_index(program)?;
        self.programs[index].as_ref().map(|p| p.domain)
    }

    /// Objects made and not seen freed, with their domains: those whose destroy failed included.
    pub fn objects(&self) -> impl Iterator<Item = (u32, Domain)> + '_ {
        self.ledger.iter().map(|e| (e.handle, e.domain))
    }

    /// The domain program index `index` gets.
    fn domain_of(&self, index: usize) -> Option<Domain> {
        match self.domains {
            Domains::PerProgram => Domain::new(u32::try_from(index).ok()? + 1),
            Domains::Shared => Some(Domain::DEFAULT),
        }
    }

    /// Whether `domain` holds no object of the claim's.
    fn domain_empty(&self, domain: Domain) -> bool {
        !self.ledger.iter().any(|e| e.domain == domain)
    }

    /// Whether every object in program `index`'s domain is the program's.
    fn confined(&self, index: usize, domain: Domain) -> bool {
        self.ledger
            .iter()
            .filter(|e| e.domain == domain)
            .all(|e| e.owner == Some(index))
    }

    /// Makes an object for program `index` and enters it in the ledger.
    fn create(&mut self, index: usize, request: Create) -> Result<Object, Status> {
        let object = match self.session.create(request) {
            Ok(object) => object,
            Err(error) => {
                self.seen(&error);
                return Err(Status::IO);
            }
        };
        self.ledger.push(Entry {
            handle: object.handle,
            domain: object.domain,
            owner: Some(index),
        });
        Ok(object)
    }

    /// Destroys an object. If the kernel refuses, it stays in the ledger with no owner, which
    /// keeps its domain from being given out or run in until [`Self::sweep`] frees it.
    fn release(&mut self, handle: u32) {
        let freed = match self.session.destroy(handle) {
            Ok(()) => true,
            Err(error) => {
                self.seen(&error);
                false
            }
        };
        if let Some(at) = self.ledger.iter().position(|e| e.handle == handle) {
            if freed {
                self.ledger.swap_remove(at);
            } else {
                self.ledger[at].owner = None;
            }
        }
    }

    /// Tries again to destroy every object whose destroy failed. Allocates nothing.
    fn sweep(&mut self) {
        let session = &mut self.session;
        self.ledger
            .retain(|e| e.owner.is_some() || session.destroy(e.handle).is_err());
    }

    /// The stream's buffers held, one bit each, to hand to the next claim on the same stream
    /// with [`Self::adopt`] when this one is dropped.
    pub fn held(&self) -> u64 {
        self.held
    }

    /// Takes on the buffers an earlier claim on the same stream held: the stream counts them as
    /// the producer's, and no other claim will be given them again.
    pub fn adopt(&mut self, held: u64) {
        self.held |= held;
    }

    /// Whether the driver holds `buffer`.
    pub fn holds(&self, buffer: u32) -> bool {
        buffer < MAX_BUFFERS && self.held & (1 << buffer) != 0
    }

    fn set_held(&mut self, buffer: u32, held: bool) {
        if buffer < MAX_BUFFERS {
            if held {
                self.held |= 1 << buffer;
            } else {
                self.held &= !(1 << buffer);
            }
        }
    }

    /// Takes every buffer the stream has free or returned, at most one round of the stream.
    fn take_free<S: StreamEndpoint + ?Sized>(&mut self, stream: &mut S) -> Result<(), S::Error> {
        for _ in 0..stream.buffer_count() {
            let Some(buffer) = stream.next_buffer()? else {
                break;
            };
            self.set_held(buffer, true);
        }
        Ok(())
    }

    /// Gives up `buffer` if it was taken under a consumer that has gone, so the next
    /// [`Self::take_free`] takes it again under the consumer attached now. A buffer taken before
    /// the consumer came has nothing behind it, and its completion would reach no one.
    fn renew<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
    ) -> Result<(), S::Error> {
        if !self.holds(buffer) || !matches!(stream.buffer_current(buffer), Ok(false)) {
            return Ok(());
        }
        // Nothing is behind it, so this reaches no one.
        stream.publish(StreamFrame {
            buffer,
            bytes: 0,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
        })?;
        self.set_held(buffer, false);
        Ok(())
    }

    /// Whether `bytes` from the start of `buffer` are inside it: its last byte reads.
    fn fits<S: StreamEndpoint + ?Sized>(&self, stream: &mut S, buffer: u32, bytes: u32) -> bool {
        let Some(last) = bytes.checked_sub(1) else {
            return false;
        };
        self.holds(buffer) && matches!(stream.read(buffer, last as usize, &mut [0]), Ok(true))
    }

    /// Loads the program in the first `bytes` of `buffer` on the stream into a domain holding
    /// nothing, and returns its number. See [`crate::program`] for the framing.
    pub fn load<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
        bytes: u32,
    ) -> Result<u32, Status> {
        if bytes == 0 || bytes > MAX_PROGRAM_BYTES {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.renew(stream, buffer).map_err(|_| Status::IO)?;
        self.take_free(stream).map_err(|_| Status::IO)?;
        if !self.fits(stream, buffer, bytes) {
            return Err(Status::INVALID_ARGUMENT);
        }
        // The header and the table are read once into memory of the driver's own, and only
        // those copies are checked and used: the consumer may change the buffer meanwhile.
        let mut head = [0u8; HEADER_BYTES];
        if !matches!(stream.read(buffer, 0, &mut head), Ok(true)) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let header = Header::parse(&head, bytes).map_err(|_| Status::INVALID_ARGUMENT)?;
        let mut table = vec![0u8; header.table_bytes()];
        if !table.is_empty()
            && !matches!(stream.read(buffer, header.table_at(), &mut table), Ok(true))
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut relocations = Vec::with_capacity(usize::from(header.relocations));
        let (entries, _) = table.as_chunks::<RELOCATION_BYTES>();
        for raw in entries {
            relocations
                .push(Relocation::parse(raw, &header).map_err(|_| Status::INVALID_ARGUMENT)?);
        }
        self.sweep();
        let free = (0..MAX_PROGRAMS).find_map(|index| {
            let domain = self.domain_of(index)?;
            (self.programs[index].is_none() && self.domain_empty(domain)).then_some((index, domain))
        });
        let (index, domain) = free.ok_or(Status::BUSY)?;
        let body = Create::new(domain, header.body, Kind::Body).ok_or(Status::INVALID_ARGUMENT)?;
        let body = self.create(index, body)?;
        let loaded = self.fill(stream, buffer, index, domain, &header, body);
        let scratch = match loaded {
            Ok(scratch) => scratch,
            Err(status) => {
                self.release(body.handle);
                return Err(status);
            }
        };
        let run = Run::new(&body, header.tasks, header.task_base, header.pingpong());
        let Ok(run) = run else {
            self.release(body.handle);
            if let Some(scratch) = scratch {
                self.release(scratch.handle);
            }
            return Err(Status::INVALID_ARGUMENT);
        };
        let program = Program {
            domain,
            header,
            body,
            scratch,
            relocations,
            run,
            attached: std::array::from_fn(|_| None),
        };
        let placed = self.place(&program, |target| match target {
            Target::Body => Some(body.iova),
            Target::Scratch => scratch.map(|s| s.iova),
            Target::Slot(_) => None,
        });
        self.programs[index] = Some(program);
        if !placed {
            let _ = self.unload(index as u32 + 1);
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(index as u32 + 1)
    }

    /// Copies the body in and makes the scratch object.
    fn fill<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
        index: usize,
        domain: Domain,
        header: &Header,
        body: Object,
    ) -> Result<Option<Object>, Status> {
        let memory = self.session.memory(body.handle).ok_or(Status::IO)?;
        let into = memory.get_mut(..header.body as usize).ok_or(Status::IO)?;
        if !matches!(stream.read(buffer, header.body_at(), into), Ok(true)) {
            return Err(Status::INVALID_ARGUMENT);
        }
        if header.scratch == 0 {
            return Ok(None);
        }
        let scratch =
            Create::new(domain, header.scratch, Kind::Data).ok_or(Status::INVALID_ARGUMENT)?;
        self.create(index, scratch).map(Some)
    }

    /// Writes the address of every relocation whose target `address` answers into the body.
    /// `false` if one did not fit.
    fn place(&mut self, program: &Program, address: impl Fn(Target) -> Option<u64>) -> bool {
        let Some(body) = self.session.memory(program.body.handle) else {
            return false;
        };
        let len = (program.body.bytes as usize).min(body.len());
        let body = &mut body[..len];
        program.relocations.iter().all(|relocation| {
            address(relocation.target).is_none_or(|base| relocation.apply(body, base))
        })
    }

    /// Unloads a program: every object of its own is destroyed, its buffers detached.
    pub fn unload(&mut self, program: u32) -> Result<(), Status> {
        let index = program_index(program).ok_or(Status::NOT_FOUND)?;
        let program = self.programs[index].take().ok_or(Status::NOT_FOUND)?;
        for attached in program.attached.iter().flatten() {
            self.release(attached.object.handle);
        }
        if let Some(scratch) = program.scratch {
            self.release(scratch.handle);
        }
        self.release(program.body.handle);
        Ok(())
    }

    /// Attaches the first `bytes` of `buffer` on the stream as slot `slot` of `program`: an
    /// object of that size in the program's domain, whose address goes into the body at the
    /// places the program named. A slot attached already is replaced.
    pub fn attach<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        program: u32,
        slot: u16,
        buffer: u32,
        bytes: u32,
    ) -> Result<(), Status> {
        let index = program_index(program).ok_or(Status::NOT_FOUND)?;
        let (domain, slots) = match &self.programs[index] {
            Some(p) => (p.domain, p.header.slots),
            None => return Err(Status::NOT_FOUND),
        };
        if slot >= slots || bytes == 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.renew(stream, buffer).map_err(|_| Status::IO)?;
        self.take_free(stream).map_err(|_| Status::IO)?;
        if !self.fits(stream, buffer, bytes) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let named = self.programs[index].as_ref().is_some_and(|p| {
            p.relocations
                .iter()
                .all(|r| r.target != Target::Slot(slot) || r.addend < bytes)
        });
        if !named {
            return Err(Status::INVALID_ARGUMENT);
        }
        let request = Create::new(domain, bytes, Kind::Data).ok_or(Status::INVALID_ARGUMENT)?;
        let object = self.create(index, request)?;
        let Some(mut program) = self.programs[index].take() else {
            return Err(Status::NOT_FOUND);
        };
        let old = program.attached[usize::from(slot)].replace(Attached {
            buffer,
            bytes,
            object,
        });
        let placed = self.place(&program, |target| {
            (target == Target::Slot(slot)).then_some(object.iova)
        });
        self.programs[index] = Some(program);
        if let Some(old) = old {
            self.release(old.object.handle);
        }
        if !placed {
            let _ = self.detach(index as u32 + 1, slot);
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// Detaches one slot, destroying its object.
    pub fn detach(&mut self, program: u32, slot: u16) -> Result<(), Status> {
        let index = program_index(program).ok_or(Status::NOT_FOUND)?;
        let program = self.programs[index].as_mut().ok_or(Status::NOT_FOUND)?;
        let attached = program
            .attached
            .get_mut(usize::from(slot))
            .and_then(Option::take)
            .ok_or(Status::NOT_FOUND)?;
        self.release(attached.object.handle);
        Ok(())
    }

    /// Takes the stream's free buffers and runs the jobs waiting on it, at `now_ns` on the
    /// caller's clock. Returns how many jobs it read. An error is the stream's, and leaves the
    /// jobs not yet read where they are. Allocates nothing.
    pub fn serve<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        now_ns: u64,
    ) -> Result<u32, S::Error> {
        self.take_free(stream)?;
        let mut encoded = [0u8; LONGEST_JOB];
        let mut read = 0;
        while read < MAX_JOBS_PER_ROUND {
            let Some(len) = stream.next_job(&mut encoded)? else {
                break;
            };
            read += 1;
            let job = encoded
                .get(..len)
                .filter(|bytes| bytes.len() <= JOB_BYTES)
                .and_then(|bytes| RunJob::decode(&mut Reader::new(bytes)).ok());
            // A broken consumer, by the contract: bytes that are no job, or a result buffer that
            // isn't on the stream. It loses the stream, and no job of its is run after.
            let Some(job) = job else {
                self.fault(stream, Failure::Undecodable)?;
                break;
            };
            if job.result >= stream.buffer_count() {
                self.fault(stream, Failure::Result)?;
                break;
            }
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look, or come since the buffer was taken.
            self.renew(stream, job.result)?;
            self.take_free(stream)?;
            if !self.holds(job.result) {
                // On the stream but not the driver's now: there is nothing to complete on.
                self.failed(Failure::Result);
                continue;
            }
            let outcome = self.run(stream, &job);
            self.complete(stream, &job, outcome, now_ns)?;
        }
        Ok(read)
    }

    /// Faults the consumer whose job was no job. Every buffer held was taken under it and has
    /// nothing behind it now, so each is given up, to be taken again under the next consumer.
    fn fault<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        failure: Failure,
    ) -> Result<(), S::Error> {
        self.failed(failure);
        stream.fault_consumer()?;
        for buffer in 0..MAX_BUFFERS {
            if !self.holds(buffer) {
                continue;
            }
            // Nothing is behind it, so this reaches no one.
            stream.publish(StreamFrame {
                buffer,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: 0,
            })?;
            self.set_held(buffer, false);
        }
        Ok(())
    }

    fn failed(&mut self, failure: Failure) {
        self.counters.errors = self.counters.errors.saturating_add(1);
        self.last_failure = Some(failure);
    }

    /// Writes the run's record into its result buffer and publishes it.
    fn complete<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &RunJob,
        outcome: Result<(), (RunStatus, Failure)>,
        now_ns: u64,
    ) -> Result<(), S::Error> {
        let status = match outcome {
            Ok(()) => RunStatus::Done,
            Err((status, _)) => status,
        };
        let mut record = [0u8; RESULT_BYTES];
        let mut writer = Writer::new(&mut record);
        let encoded = RunResult { status }.encode(&mut writer).is_ok();
        let used = writer.len();
        let written = encoded && matches!(stream.write(job.result, 0, &record[..used]), Ok(true));
        let frame = StreamFrame {
            buffer: job.result,
            bytes: if written { used as u32 } else { 0 },
            sequence: self.sequence,
            identity: job.identity,
            timestamp_ns: now_ns,
        };
        let published = stream.publish(frame);
        if published.is_ok() {
            self.set_held(job.result, false);
            self.sequence = self.sequence.wrapping_add(1);
        }
        match outcome {
            Ok(()) if written => self.counters.jobs = self.counters.jobs.saturating_add(1),
            Ok(()) => self.failed(Failure::Completion),
            Err((_, failure)) => self.failed(failure),
        }
        published
    }

    /// Runs one job: checks, copies in, submits, copies out.
    fn run<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &RunJob,
    ) -> Result<(), (RunStatus, Failure)> {
        const REFUSED: (RunStatus, Failure) = (RunStatus::Refused, Failure::Refused);
        let index = program_index(job.program).ok_or(REFUSED)?;
        let program = self.programs[index].as_ref().ok_or(REFUSED)?;
        let slots = usize::from(program.header.slots);
        let attached = &program.attached[..slots];
        if attached.iter().any(Option::is_none) {
            return Err(REFUSED);
        }
        if attached.iter().flatten().any(|a| a.buffer == job.result) {
            return Err(REFUSED);
        }
        let domain = program.domain;
        self.sweep();
        if !self.confined(index, domain) {
            return Err(REFUSED);
        }
        let Some(program) = self.programs[index].as_ref() else {
            return Err(REFUSED);
        };
        for a in program.attached[..slots].iter().flatten() {
            let memory = self
                .session
                .memory(a.object.handle)
                .ok_or((RunStatus::Failed, Failure::Copy))?;
            let (data, rest) = memory.split_at_mut((a.bytes as usize).min(memory.len()));
            if !matches!(stream.read(a.buffer, 0, data), Ok(true)) {
                return Err((RunStatus::Refused, Failure::Copy));
            }
            // Whatever the last run left past the buffer's bytes goes.
            rest.fill(0);
        }
        if let Err(error) = self.session.submit(&program.run) {
            self.lost |= error.raw_os_error() == Some(ENODEV);
            return Err((RunStatus::Failed, Failure::Device));
        }
        for a in program.attached[..slots].iter().flatten() {
            let memory = self
                .session
                .memory(a.object.handle)
                .ok_or((RunStatus::Failed, Failure::Copy))?;
            let data = memory
                .get(..a.bytes as usize)
                .ok_or((RunStatus::Failed, Failure::Copy))?;
            if !matches!(stream.write(a.buffer, 0, data), Ok(true)) {
                return Err((RunStatus::Failed, Failure::Copy));
            }
        }
        Ok(())
    }
}

impl<D: Device> Drop for Claim<D> {
    /// Destroys every object the claim made before the session closes. Closing the file would
    /// free what is left anyway (the kernel releases a file's handles with it).
    fn drop(&mut self) {
        for index in 0..MAX_PROGRAMS {
            let _ = self.unload(index as u32 + 1);
        }
        self.sweep();
        self.claimed.store(false, Ordering::Release);
    }
}

/// `npu/1` served on a claim and a stream: the contract's calls, with the stream they need.
pub struct Served<'c, D: Device, S: StreamEndpoint + ?Sized> {
    /// The claim.
    pub claim: &'c mut Claim<D>,
    /// The job stream.
    pub stream: &'c mut S,
    /// The stream's name, as [`Npu::job_stream`] answers it.
    pub name: &'c str,
}

impl<D: Device, S: StreamEndpoint + ?Sized> Npu for Served<'_, D, S> {
    fn info(&mut self) -> lemnos_core::Result<NpuInfo> {
        Ok(self.claim.info())
    }

    fn job_stream(&mut self, into: &mut [u8]) -> lemnos_core::Result<usize> {
        let name = self.name.as_bytes();
        if name.len() > MAX_STREAM_NAME {
            return Err(Status::INVALID_ARGUMENT);
        }
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn load(&mut self, buffer: u32, bytes: u32) -> lemnos_core::Result<u32> {
        self.claim.load(self.stream, buffer, bytes)
    }

    fn unload(&mut self, program: u32) -> lemnos_core::Result<()> {
        self.claim.unload(program)
    }

    fn attach(
        &mut self,
        program: u32,
        slot: u16,
        buffer: u32,
        bytes: u32,
    ) -> lemnos_core::Result<()> {
        self.claim.attach(self.stream, program, slot, buffer, bytes)
    }

    fn detach(&mut self, program: u32, slot: u16) -> lemnos_core::Result<()> {
        self.claim.detach(program, slot)
    }

    fn counters(&mut self) -> lemnos_core::Result<Counters> {
        Ok(self.claim.counters())
    }
}
