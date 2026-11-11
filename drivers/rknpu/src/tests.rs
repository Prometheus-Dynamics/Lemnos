//! Layouts, the framing, the request builders, and the claim against a played kernel that maps
//! objects into domains the way `drivers/rknpu/` does (see [`crate::claim`]): an object lives in
//! the domain it was made in until it is destroyed, a job runs in the domain it names, and a
//! kernel built without switching folds every id into one domain. The played block follows the
//! addresses a program holds through that mapping, so a program that names an address outside
//! its domain faults. No device node is opened.

use std::collections::BTreeMap;
use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::sync::{Arc, Mutex};

use lemnos_contract_npu::{Npu, RESULT_BYTES, RunJob, RunResult, RunStatus};
use lemnos_core::{Decode, Encode, Reader, Status, Writer};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use crate::program::{
    self, BODY, FramingError, HEADER_BYTES, Header, MAX_SLOTS, RELOCATION_BYTES, Relocation,
    SCRATCH, Target,
};
use crate::request::{Create, Domain, Kind, Object, Query, RUN_TIMEOUT_MS, Run, RunError};
use crate::uapi::{
    self, Action, MemCreate, MemDestroy, MemMap, MemSync, SubcoreTask, Submit, TASK_BYTES, Task,
};
use crate::{Claim, ClaimError, Device, Domains, Failure, Served, Service, Session, backoff_ns};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

// ---------------------------------------------------------------------------------------------
// Layouts

#[test]
fn request_numbers_match_the_header() {
    // DRM_IOWR(DRM_COMMAND_BASE + n, struct ...), from gcc and aarch64-linux-gnu-gcc on
    // rknpu_ioctl.h (layout-check/).
    assert_eq!(uapi::IOCTL_ACTION, 0xc008_6440);
    assert_eq!(uapi::IOCTL_SUBMIT, 0xc068_6441);
    assert_eq!(uapi::IOCTL_MEM_CREATE, 0xc030_6442);
    assert_eq!(uapi::IOCTL_MEM_MAP, 0xc010_6443);
    assert_eq!(uapi::IOCTL_MEM_DESTROY, 0xc010_6444);
    assert_eq!(uapi::IOCTL_MEM_SYNC, 0xc020_6445);
}

#[test]
fn structure_layouts_match_the_header() {
    assert_eq!((size_of::<MemCreate>(), align_of::<MemCreate>()), (48, 8));
    assert_eq!(offset_of!(MemCreate, handle), 0);
    assert_eq!(offset_of!(MemCreate, flags), 4);
    assert_eq!(offset_of!(MemCreate, size), 8);
    assert_eq!(offset_of!(MemCreate, obj_addr), 16);
    assert_eq!(offset_of!(MemCreate, dma_addr), 24);
    assert_eq!(offset_of!(MemCreate, sram_size), 32);
    assert_eq!(offset_of!(MemCreate, iommu_domain_id), 40);
    assert_eq!(offset_of!(MemCreate, core_mask), 44);

    assert_eq!(size_of::<MemMap>(), 16);
    assert_eq!(offset_of!(MemMap, offset), 8);
    assert_eq!(size_of::<MemDestroy>(), 16);
    assert_eq!(offset_of!(MemDestroy, obj_addr), 8);
    assert_eq!(size_of::<MemSync>(), 32);
    assert_eq!(offset_of!(MemSync, obj_addr), 8);
    assert_eq!(offset_of!(MemSync, offset), 16);
    assert_eq!(offset_of!(MemSync, size), 24);

    assert_eq!((size_of::<Task>(), align_of::<Task>()), (40, 1));
    assert_eq!(size_of::<Task>(), TASK_BYTES as usize);
    assert_eq!(offset_of!(Task, int_mask), 12);
    assert_eq!(offset_of!(Task, int_status), 20);
    assert_eq!(offset_of!(Task, regcfg_amount), 24);
    assert_eq!(offset_of!(Task, regcfg_offset), 28);
    assert_eq!(offset_of!(Task, regcmd_addr), 32);

    assert_eq!(size_of::<SubcoreTask>(), 8);
    assert_eq!((size_of::<Submit>(), align_of::<Submit>()), (104, 8));
    assert_eq!(offset_of!(Submit, timeout), 4);
    assert_eq!(offset_of!(Submit, task_start), 8);
    assert_eq!(offset_of!(Submit, task_number), 12);
    assert_eq!(offset_of!(Submit, task_counter), 16);
    assert_eq!(offset_of!(Submit, priority), 20);
    assert_eq!(offset_of!(Submit, task_obj_addr), 24);
    assert_eq!(offset_of!(Submit, iommu_domain_id), 32);
    assert_eq!(offset_of!(Submit, task_base_addr), 40);
    assert_eq!(offset_of!(Submit, hw_elapse_time), 48);
    assert_eq!(offset_of!(Submit, core_mask), 56);
    assert_eq!(offset_of!(Submit, fence_fd), 60);
    assert_eq!(offset_of!(Submit, subcore_task), 64);

    assert_eq!(size_of::<Action>(), 8);
    assert_eq!(offset_of!(Action, value), 4);
}

#[test]
fn flags_and_ids_match_the_header() {
    assert_eq!(uapi::mem::KERNEL_MAPPING, 0x8);
    assert_eq!(uapi::mem::TRY_ALLOC_SRAM, 0x100);
    assert_eq!(uapi::mem::TRY_ALLOC_NBUF, 0x200);
    assert_eq!(uapi::mem::IOMMU_LIMIT_IOVA_ALIGNMENT, 0x400);
    assert_eq!(uapi::job::PC, 1);
    assert_eq!(uapi::job::NONBLOCK, 2);
    assert_eq!(uapi::job::PINGPONG, 4);
    assert_eq!(uapi::job::FENCE_OUT, 0x10);
    assert_eq!(uapi::action::GET_IOMMU_EN, 18);
    assert_eq!(uapi::action::GET_IOMMU_DOMAIN_ID, 24);
    assert_eq!(uapi::action::SET_IOMMU_DOMAIN_ID, 25);
    assert_eq!(uapi::DOMAINS, 16);
}

// ---------------------------------------------------------------------------------------------
// The framing

/// A program as a caller frames it.
#[derive(Clone)]
struct Framed {
    flags: u16,
    tasks: u32,
    slots: u16,
    scratch: u32,
    task_base: u32,
    reserved: u32,
    relocations: Vec<[u8; RELOCATION_BYTES]>,
    body: Vec<u8>,
}

fn relocation(at: u32, target: u16, width: u8, addend: u32) -> [u8; RELOCATION_BYTES] {
    let mut r = [0; RELOCATION_BYTES];
    r[..4].copy_from_slice(&at.to_le_bytes());
    r[4..6].copy_from_slice(&target.to_le_bytes());
    r[6] = width;
    r[8..].copy_from_slice(&addend.to_le_bytes());
    r
}

impl Framed {
    /// The played block's one operation: task 0's `regcmd_addr` is where it reads
    /// `regcfg_amount` bytes, and the eight bytes after the task array where it writes them, each
    /// plus one. Slot 0 is read and slot 1 written, both by relocation.
    fn copy(len: u32) -> Self {
        let mut body = vec![0u8; 48];
        body[24..28].copy_from_slice(&len.to_le_bytes());
        Self {
            flags: 0,
            tasks: 1,
            slots: 2,
            scratch: 0,
            task_base: 0,
            reserved: 0,
            relocations: vec![relocation(32, 0, 8, 0), relocation(40, 1, 8, 0)],
            body,
        }
    }

    fn header(&self) -> [u8; HEADER_BYTES] {
        let mut h = [0u8; HEADER_BYTES];
        h[..4].copy_from_slice(&program::MAGIC);
        h[4..6].copy_from_slice(&program::VERSION.to_le_bytes());
        h[6..8].copy_from_slice(&self.flags.to_le_bytes());
        h[8..12].copy_from_slice(&self.tasks.to_le_bytes());
        h[12..14].copy_from_slice(&self.slots.to_le_bytes());
        h[14..16].copy_from_slice(&(self.relocations.len() as u16).to_le_bytes());
        h[16..20].copy_from_slice(&(self.body.len() as u32).to_le_bytes());
        h[20..24].copy_from_slice(&self.scratch.to_le_bytes());
        h[24..28].copy_from_slice(&self.task_base.to_le_bytes());
        h[28..32].copy_from_slice(&self.reserved.to_le_bytes());
        h
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = self.header().to_vec();
        for r in &self.relocations {
            out.extend_from_slice(r);
        }
        out.extend_from_slice(&self.body);
        out
    }

    fn parse(&self) -> Result<Header, FramingError> {
        Header::parse(&self.header(), self.bytes().len() as u32)
    }
}

#[test]
fn a_well_framed_program_parses() {
    let framed = Framed::copy(4);
    let header = framed.parse().unwrap();
    assert_eq!(header.tasks, 1);
    assert_eq!(header.slots, 2);
    assert_eq!(header.relocations, 2);
    assert_eq!(header.body, 48);
    assert_eq!(header.body_at(), HEADER_BYTES + 2 * RELOCATION_BYTES);
    let r = Relocation::parse(&framed.relocations[1], &header).unwrap();
    assert_eq!(
        r,
        Relocation {
            at: 40,
            width: 8,
            target: Target::Slot(1),
            addend: 0
        }
    );
}

#[test]
fn every_header_field_is_checked() {
    let good = Framed::copy(4);
    let mut bytes = good.header();
    bytes[0] = b'X';
    assert_eq!(
        Header::parse(&bytes, good.bytes().len() as u32),
        Err(FramingError::Magic)
    );
    type Change = fn(&mut Framed);
    let cases: [(Change, FramingError); 9] = [
        (|f| f.flags = 2, FramingError::Reserved),
        (|f| f.reserved = 1, FramingError::Reserved),
        (|f| f.tasks = 0, FramingError::Tasks),
        // Two tasks are 80 bytes; the body holds 48.
        (|f| f.tasks = 2, FramingError::Tasks),
        (|f| f.body.clear(), FramingError::Body),
        (|f| f.slots = MAX_SLOTS + 1, FramingError::Slots),
        (
            |f| f.scratch = program::MAX_SCRATCH_BYTES + 1,
            FramingError::Scratch,
        ),
        (|f| f.task_base = 48, FramingError::TaskBase),
        (|f| f.tasks = u32::MAX, FramingError::Tasks),
    ];
    for (change, error) in cases {
        let mut framed = good.clone();
        change(&mut framed);
        assert_eq!(framed.parse(), Err(error));
    }
    // A program one byte longer or shorter than its framing says.
    assert_eq!(
        Header::parse(&good.header(), good.bytes().len() as u32 + 1),
        Err(FramingError::Length)
    );
    assert_eq!(
        Header::parse(&good.header(), good.bytes().len() as u32 - 1),
        Err(FramingError::Length)
    );
}

#[test]
fn every_relocation_field_is_checked() {
    let mut framed = Framed::copy(4);
    framed.scratch = 64;
    let header = framed.parse().unwrap();
    let bad = [
        relocation(44, 0, 8, 0),       // past the 48-byte body
        relocation(46, 0, 4, 0),       // not a multiple of 4
        relocation(0, 0, 2, 0),        // width
        relocation(0, 2, 4, 0),        // slot 2 of a two-slot program
        relocation(0, BODY, 4, 48),    // past the body
        relocation(0, SCRATCH, 4, 64), // past the scratch
        relocation(0, 0xfffd, 4, 0),   // no such target
        {
            let mut r = relocation(0, 0, 4, 0);
            r[7] = 1; // reserved
            r
        },
    ];
    for raw in bad {
        assert_eq!(
            Relocation::parse(&raw, &header),
            Err(FramingError::Relocation),
            "{raw:?}"
        );
    }
    assert!(Relocation::parse(&relocation(44, 0, 4, 0), &header).is_ok());
    assert!(Relocation::parse(&relocation(0, BODY, 8, 47), &header).is_ok());
    assert!(Relocation::parse(&relocation(0, SCRATCH, 8, 63), &header).is_ok());
}

#[test]
fn a_relocation_writes_only_its_place() {
    let mut body = [0xaau8; 16];
    let r = Relocation {
        at: 4,
        width: 4,
        target: Target::Body,
        addend: 2,
    };
    assert!(r.apply(&mut body, 0x1000));
    assert_eq!(body[..4], [0xaa; 4]);
    assert_eq!(body[4..8], 0x1002u32.to_le_bytes());
    assert_eq!(body[8..], [0xaa; 8]);
    // An address a four-byte place cannot hold, and a place past the body, write nothing.
    assert!(!r.apply(&mut body, u64::from(u32::MAX)));
    let far = Relocation { at: 16, ..r };
    assert!(!far.apply(&mut body, 0));
    assert_eq!(body[4..8], 0x1002u32.to_le_bytes());
}

// ---------------------------------------------------------------------------------------------
// The requests

#[test]
fn objects_are_made_zeroed_through_the_iommu_and_never_in_shared_memory() {
    let domain = Domain::new(3).unwrap();
    let body = Create::new(domain, 100, Kind::Body).unwrap().to_raw();
    let data = Create::new(domain, 100, Kind::Data).unwrap().to_raw();
    for raw in [body, data] {
        assert_eq!(raw.handle, 0, "an existing handle would return that object");
        assert_eq!(raw.size, 100);
        assert_eq!(raw.iommu_domain_id, 3);
        assert_eq!((raw.sram_size, raw.core_mask), (0, 0));
        let never = uapi::mem::TRY_ALLOC_SRAM
            | uapi::mem::TRY_ALLOC_NBUF
            | uapi::mem::CACHEABLE
            | uapi::mem::SECURE
            | uapi::mem::DMA32
            | uapi::mem::IOMMU_LIMIT_IOVA_ALIGNMENT;
        assert_eq!(raw.flags & never, 0);
        let always = uapi::mem::NON_CONTIGUOUS | uapi::mem::IOMMU | uapi::mem::ZEROING;
        assert_eq!(raw.flags & always, always);
    }
    assert_ne!(body.flags & uapi::mem::KERNEL_MAPPING, 0);
    assert_eq!(data.flags & uapi::mem::KERNEL_MAPPING, 0);
    assert_eq!(Create::new(domain, 0, Kind::Data), None);
    assert_eq!(Domain::new(16), None);
    assert_eq!(Domain::new(15).map(Domain::id), Some(15));
}

fn body_object(bytes: u32, kind: Kind) -> Object {
    Object {
        handle: 7,
        iova: 0x10_0000,
        bytes,
        size: 4096,
        domain: Domain::new(2).unwrap(),
        kind,
    }
}

#[test]
fn a_run_names_only_tasks_inside_its_body() {
    let body = body_object(80, Kind::Body);
    assert_eq!(Run::new(&body, 0, 0, false), Err(RunError::Tasks));
    // Three tasks are 120 bytes: past the 80 asked for, though inside the page the kernel made.
    assert_eq!(Run::new(&body, 3, 0, false), Err(RunError::Tasks));
    assert_eq!(Run::new(&body, 2, 80, false), Err(RunError::TaskBase));
    assert_eq!(
        Run::new(&body_object(80, Kind::Data), 1, 0, false),
        Err(RunError::NotBody)
    );
    let run = Run::new(&body, 2, 40, true).unwrap();
    let raw = run.to_raw(0xffff_0000_1234_5678);
    assert_eq!(raw.flags, uapi::job::PC | uapi::job::PINGPONG);
    assert_eq!(raw.timeout, RUN_TIMEOUT_MS);
    assert_eq!((raw.task_start, raw.task_number), (0, 2));
    assert_eq!(raw.task_obj_addr, 0xffff_0000_1234_5678);
    assert_eq!(raw.iommu_domain_id, 2);
    assert_eq!(raw.task_base_addr, 0x10_0000 + 40);
    assert_eq!(raw.core_mask, uapi::core::CORE0);
    assert_eq!(raw.fence_fd, -1);
    for range in raw.subcore_task {
        assert_eq!((range.task_start, range.task_number), (0, 2));
    }
    let plain = Run::new(&body, 1, 0, false).unwrap().to_raw(1);
    assert_eq!(plain.flags, uapi::job::PC);
}

#[test]
fn queries_are_reads_and_the_domain_probe() {
    assert_eq!(
        Query::IommuEnabled.to_raw(),
        Action {
            flags: 18,
            value: 0
        }
    );
    assert_eq!(
        Query::SetDomain(Domain::new(1).unwrap()).to_raw(),
        Action {
            flags: 25,
            value: 1
        }
    );
    assert_eq!(Query::Domain.to_raw().flags, 24);
}

// ---------------------------------------------------------------------------------------------
// The played kernel

const BASE_IOVA: u64 = 0x1000_0000;
const ENODEV: i32 = 19;
const PAGE: u64 = 4096;

#[derive(Clone, Debug)]
struct Played {
    file: u32,
    domain: u32,
    iova: u64,
    size: u64,
    kernel_mapped: bool,
}

/// What one job could reach: every object in its domain.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    domain: u32,
    reachable: Vec<u32>,
    /// Of those, objects another file made.
    foreign: usize,
}

#[derive(Default)]
struct Kernel {
    iommu: bool,
    switching: bool,
    current: u32,
    objects: BTreeMap<u32, Played>,
    next_handle: u32,
    next_iova: BTreeMap<u32, u64>,
    files: u32,
    fail_opens: u32,
    fail_destroys: u32,
    /// The device was removed: opens fail, and every call on an open file answers `ENODEV`.
    gone: bool,
    record: bool,
    seen: Vec<Seen>,
    submits: u64,
    /// Called as each run starts, with the runs so far, so a test can act on the stream while
    /// the block runs.
    hook: Option<Box<dyn FnMut(u64) + Send>>,
}

impl Kernel {
    fn new(switching: bool) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            iommu: true,
            switching,
            next_handle: 1,
            record: true,
            ..Self::default()
        }))
    }

    /// The domain an id lands in: itself, or the one domain on a kernel without switching.
    fn effective(&self, id: u32) -> u32 {
        assert!(id < uapi::DOMAINS, "domain id {id}");
        if self.switching { id } else { 0 }
    }

    /// Makes an object as `rknpu_gem_object_create` does: the device switches to the domain
    /// and the object is mapped there.
    fn make(&mut self, file: u32, domain: u32, size: u64, kernel_mapped: bool) -> (u32, u64) {
        let domain = self.effective(domain);
        self.current = domain;
        let next = self.next_iova.entry(domain).or_insert(BASE_IOVA);
        let iova = *next;
        // A gap page, so a run off one object's end faults rather than landing in the next.
        *next += size + PAGE;
        let handle = self.next_handle;
        self.next_handle += 1;
        self.objects.insert(
            handle,
            Played {
                file,
                domain,
                iova,
                size,
                kernel_mapped,
            },
        );
        (handle, iova)
    }

    fn ours(&self, file: u32) -> usize {
        self.objects.values().filter(|o| o.file == file).count()
    }
}

struct Node {
    kernel: Arc<Mutex<Kernel>>,
}

impl Device for Node {
    type Session = PlayedSession;

    fn open(&mut self) -> io::Result<PlayedSession> {
        let mut k = self.kernel.lock().unwrap();
        if k.gone {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        if k.fail_opens > 0 {
            k.fail_opens -= 1;
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        k.files += 1;
        Ok(PlayedSession {
            kernel: self.kernel.clone(),
            file: k.files,
            memory: BTreeMap::new(),
        })
    }
}

struct PlayedSession {
    kernel: Arc<Mutex<Kernel>>,
    file: u32,
    memory: BTreeMap<u32, Vec<u8>>,
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

impl PlayedSession {
    /// The object of this file that `[address, address + len)` falls in, among `objects`.
    fn translate(objects: &[(u32, u64, u64)], address: u64, len: u64) -> Option<(u32, usize)> {
        objects.iter().find_map(|&(handle, iova, size)| {
            let end = address.checked_add(len)?;
            (address >= iova && end <= iova + size).then_some((handle, (address - iova) as usize))
        })
    }
}

impl PlayedSession {
    /// Copies `len` bytes plus one each, in chunks so nothing is allocated.
    fn play(&mut self, src: (u32, usize), dst: (u32, usize), len: usize) {
        let mut chunk = [0u8; 256];
        let mut done = 0;
        while done < len {
            let n = chunk.len().min(len - done);
            chunk[..n].copy_from_slice(&self.memory[&src.0][src.1 + done..][..n]);
            for byte in &mut chunk[..n] {
                *byte = byte.wrapping_add(1);
            }
            self.memory.get_mut(&dst.0).unwrap()[dst.1 + done..][..n].copy_from_slice(&chunk[..n]);
            done += n;
        }
    }
}

impl Session for PlayedSession {
    fn query(&mut self, query: Query) -> io::Result<u32> {
        let mut k = self.kernel.lock().unwrap();
        if k.gone {
            return Err(io::Error::from_raw_os_error(ENODEV));
        }
        Ok(match query {
            Query::IommuEnabled => u32::from(k.iommu),
            Query::Domain => k.current,
            Query::SetDomain(domain) => {
                if k.switching {
                    k.current = domain.id();
                }
                0
            }
            Query::HardwareVersion | Query::DriverVersion => 0,
        })
    }

    fn create(&mut self, request: Create) -> io::Result<Object> {
        if self.kernel.lock().unwrap().gone {
            return Err(io::Error::from_raw_os_error(ENODEV));
        }
        let raw = request.to_raw();
        assert_eq!(raw.handle, 0);
        assert!(raw.size > 0);
        let size = raw.size.div_ceil(PAGE) * PAGE;
        let kernel_mapped = raw.flags & uapi::mem::KERNEL_MAPPING != 0;
        let (handle, iova) = self.kernel.lock().unwrap().make(
            self.file,
            raw.iommu_domain_id as u32,
            size,
            kernel_mapped,
        );
        self.memory.insert(handle, vec![0; size as usize]);
        Ok(Object {
            handle,
            iova,
            bytes: request.bytes(),
            size,
            domain: request.domain(),
            kind: request.kind(),
        })
    }

    fn memory(&mut self, handle: u32) -> Option<&mut [u8]> {
        self.memory.get_mut(&handle).map(Vec::as_mut_slice)
    }

    fn destroy(&mut self, handle: u32) -> io::Result<()> {
        let mut k = self.kernel.lock().unwrap();
        if k.gone {
            return Err(io::Error::from_raw_os_error(ENODEV));
        }
        if k.fail_destroys > 0 {
            // `rknpu_gem_destroy_ioctl` gives up when the device stays in another domain.
            k.fail_destroys -= 1;
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let object = k.objects.get(&handle).ok_or(io::ErrorKind::InvalidInput)?;
        assert_eq!(object.file, self.file, "a handle of another file");
        k.objects.remove(&handle);
        self.memory.remove(&handle);
        Ok(())
    }

    fn submit(&mut self, run: &Run) -> io::Result<()> {
        let mut guard = self.kernel.lock().unwrap();
        let k = &mut *guard;
        if k.gone {
            return Err(io::Error::from_raw_os_error(ENODEV));
        }
        let body = k
            .objects
            .get(&run.body())
            .filter(|o| o.file == self.file)
            .unwrap_or_else(|| panic!("the kernel would follow a pointer to no object of the file"))
            .clone();
        let raw = run.to_raw(0xffff_ff80_0000_0000 | u64::from(run.body()));
        assert_eq!(
            raw.flags & uapi::job::PC,
            uapi::job::PC,
            "the kernel needs PC mode"
        );
        assert_eq!(raw.flags & !(uapi::job::PC | uapi::job::PINGPONG), 0);
        assert_eq!(raw.core_mask, uapi::core::CORE0);
        assert!(
            body.kernel_mapped,
            "the kernel reads the tasks through a mapping it lacks"
        );
        for range in raw.subcore_task.iter().chain([&SubcoreTask {
            task_start: raw.task_start,
            task_number: raw.task_number,
        }]) {
            let end = u64::from(range.task_start) + u64::from(range.task_number);
            assert!(range.task_number > 0);
            assert!(
                end * u64::from(TASK_BYTES) <= body.size,
                "tasks past the body"
            );
        }
        let domain = k.effective(raw.iommu_domain_id);
        assert_eq!(domain, body.domain, "a body run outside its own domain");
        // `rknpu_job_schedule`: the device switches to the job's domain for the run.
        k.current = domain;
        k.submits += 1;
        if let Some(hook) = k.hook.as_mut() {
            hook(k.submits);
        }
        let mut own = [(0u32, 0u64, 0u64); 64];
        let mut count = 0;
        let mut foreign = 0;
        for (&handle, object) in &k.objects {
            if object.domain != domain {
                continue;
            }
            if object.file == self.file {
                own[count] = (handle, object.iova, object.size);
                count += 1;
            } else {
                foreign += 1;
            }
        }
        if k.record {
            let reachable = k
                .objects
                .iter()
                .filter(|(_, o)| o.domain == domain)
                .map(|(&h, _)| h)
                .collect();
            k.seen.push(Seen {
                domain,
                reachable,
                foreign,
            });
        }
        drop(guard);
        // The block: read `len` bytes at task 0's address, write them plus one at the address
        // after the task array, and at the next one too when it is set, all through the IOMMU.
        let tasks = &self.memory[&run.body()];
        let len = u64::from(u32_at(tasks, 24));
        let (from, to, also) = (u64_at(tasks, 32), u64_at(tasks, 40), u64_at(tasks, 48));
        let own = &own[..count];
        let mut fault = false;
        for (i, to) in [to, also].into_iter().enumerate() {
            if i == 1 && to == 0 {
                continue;
            }
            match (
                Self::translate(own, from, len),
                Self::translate(own, to, len),
            ) {
                (Some(src), Some(dst)) => self.play(src, dst, len as usize),
                _ => fault = true,
            }
        }
        if fault {
            // A page fault stops the block; the kernel times the job out and resets it.
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        // The kernel writes the last task's interrupt status back.
        let last = (run.tasks() as usize - 1) * TASK_BYTES as usize + 20;
        self.memory.get_mut(&run.body()).unwrap()[last..last + 4].copy_from_slice(&[1, 0, 0, 0]);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The rig

const BUFFERS: u32 = 10;
const BUFFER_BYTES: usize = 4096;

fn stream() -> MockStream {
    MockStream::with_bytes(BUFFERS, BUFFER_BYTES)
}

fn post(stream: &MockStream, job: RunJob) {
    let mut bytes = [0u8; 16];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    stream.post_job(&bytes[..used]);
}

/// Takes the next completion, gives its buffer back, and says what it completed with.
fn completion(stream: &MockStream) -> (u64, RunStatus) {
    let frame = stream.take_frame().expect("a completion");
    assert_eq!(frame.bytes as usize, RESULT_BYTES);
    let status = RunResult::decode(&mut Reader::new(&stream.contents(frame.buffer)[..1]))
        .unwrap()
        .status;
    stream.return_buffer(frame.buffer);
    (frame.identity, status)
}

/// Loads `framed` from `buffer` and attaches `slots` (buffer, bytes) in order.
fn load<D: Device>(
    claim: &mut Claim<D>,
    stream: &mut MockStream,
    buffer: u32,
    framed: &Framed,
    slots: &[(u32, u32)],
) -> Result<u32, Status> {
    let bytes = framed.bytes();
    stream.fill(buffer, &bytes);
    let program = claim.load(stream, buffer, bytes.len() as u32)?;
    for (slot, &(buffer, bytes)) in slots.iter().enumerate() {
        claim.attach(stream, program, slot as u16, buffer, bytes)?;
    }
    Ok(program)
}

fn run_one<D: Device>(
    claim: &mut Claim<D>,
    stream: &mut MockStream,
    job: RunJob,
) -> (u64, RunStatus) {
    post(stream, job);
    assert_eq!(claim.serve(stream, 0), Ok(1));
    completion(stream)
}

fn job(identity: u64, program: u32, result: u32) -> RunJob {
    RunJob {
        identity,
        program,
        result,
    }
}

/// The handles the claim holds for program `program`'s domain, sorted.
fn handles_in(kernel: &Arc<Mutex<Kernel>>, domain: u32) -> Vec<u32> {
    kernel
        .lock()
        .unwrap()
        .objects
        .iter()
        .filter(|(_, o)| o.domain == domain)
        .map(|(&h, _)| h)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The claim

#[test]
fn a_buffer_given_back_and_named_again_while_a_round_runs_completes_its_job() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let program = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(4),
        &[(1, 4), (2, 4)],
    )
    .unwrap();
    stream.fill(1, &[10, 20, 30, 40]);
    // While the block runs the second job, the consumer takes the first one's result, gives its
    // buffer back and posts a third job whose result is written into that buffer.
    let shared = stream.clone();
    kernel.lock().unwrap().hook = Some(Box::new(move |run| {
        if run != 2 {
            return;
        }
        assert_eq!(completion(&shared), (0xa, RunStatus::Done));
        post(&shared, job(0xc, program, 3));
    }));
    post(&stream, job(0xa, program, 3));
    post(&stream, job(0xb, program, 4));
    // One round reads all three: every job read gets its completion.
    assert_eq!(claim.serve(&mut stream, 0), Ok(3));
    assert_eq!(completion(&stream), (0xb, RunStatus::Done));
    assert_eq!(completion(&stream), (0xc, RunStatus::Done));
    assert!(stream.take_frame().is_none());
    assert_eq!(claim.counters().jobs, 3);
    assert_eq!(claim.counters().errors, 0);
    assert_eq!(&stream.contents(2)[..4], &[11, 21, 31, 41]);
}

#[test]
fn a_block_without_an_iommu_is_not_claimed() {
    let kernel = Kernel::new(true);
    kernel.lock().unwrap().iommu = false;
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    assert!(matches!(service.claim(0), Err(ClaimError::NotConfined)));
}

#[test]
fn a_failed_open_backs_off_doubling_to_eight_seconds_and_never_gives_up() {
    assert_eq!(backoff_ns(1), 250_000_000);
    assert_eq!(backoff_ns(2), 500_000_000);
    assert_eq!(backoff_ns(6), 8_000_000_000);
    assert_eq!(backoff_ns(60), 8_000_000_000);
    let kernel = Kernel::new(true);
    kernel.lock().unwrap().fail_opens = 3;
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let Err(ClaimError::Device { retry_at_ns, .. }) = service.claim(0) else {
        panic!("the open fails");
    };
    assert_eq!(retry_at_ns, 250_000_000);
    assert!(matches!(
        service.claim(100),
        Err(ClaimError::BackingOff {
            retry_at_ns: 250_000_000
        })
    ));
    assert!(matches!(
        service.claim(250_000_000),
        Err(ClaimError::Device { .. })
    ));
    assert!(matches!(
        service.claim(750_000_000),
        Err(ClaimError::Device { .. })
    ));
    let claim = service.claim(2_000_000_000).unwrap();
    assert_eq!(claim.domains(), Domains::PerProgram);
}

#[test]
fn the_claim_finds_out_whether_the_kernel_switches_domains() {
    let kernel = Kernel::new(false);
    let mut service = Service::new(Node { kernel });
    let claim = service.claim(0).unwrap();
    assert_eq!(claim.domains(), Domains::Shared);
    assert_eq!(claim.info().max_programs, 1);
    drop(claim);
    let mut service = Service::new(Node {
        kernel: Kernel::new(true),
    });
    let claim = service.claim(0).unwrap();
    assert_eq!(claim.info().max_programs, 15);
    assert_eq!(claim.info().max_attached, MAX_SLOTS);
}

// ---------------------------------------------------------------------------------------------
// npu/1

#[test]
fn a_loaded_program_runs_on_copies_of_its_attached_buffers() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let program = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(4),
        &[(1, 4), (2, 4)],
    )
    .unwrap();
    assert_eq!(program, 1);
    stream.fill(1, &[10, 20, 30, 40]);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(0xabc, program, 3)),
        (0xabc, RunStatus::Done)
    );
    assert_eq!(&stream.contents(2)[..4], &[11, 21, 31, 41]);
    assert_eq!(claim.counters().jobs, 1);
    // The run named its program's domain, and the block could reach exactly its body and the
    // two slots' objects.
    let seen = kernel.lock().unwrap().seen.clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].domain, 1);
    assert_eq!(seen[0].reachable.len(), 3);
    assert_eq!(seen[0].foreign, 0);
}

#[test]
fn a_run_that_cannot_start_is_refused_and_nothing_reaches_the_block() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let program = load(&mut claim, &mut stream, 0, &Framed::copy(4), &[(1, 4)]).unwrap();
    // Slot 1 is not attached.
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, program, 3)).1,
        RunStatus::Refused
    );
    claim.attach(&mut stream, program, 1, 2, 4).unwrap();
    // The result buffer is attached.
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, program, 2)).1,
        RunStatus::Refused
    );
    // No such program.
    assert_eq!(
        run_one(&mut claim, &mut stream, job(3, 9, 3)).1,
        RunStatus::Refused
    );
    assert_eq!(
        run_one(&mut claim, &mut stream, job(4, 0, 3)).1,
        RunStatus::Refused
    );
    // An unloaded one.
    claim.unload(program).unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(5, program, 3)).1,
        RunStatus::Refused
    );
    assert_eq!(kernel.lock().unwrap().submits, 0);
    assert_eq!(claim.counters().errors, 5);
    assert_eq!(claim.last_failure(), Some(Failure::Refused));
}

#[test]
fn bad_loads_and_attachments_are_refused() {
    let mut service = Service::new(Node {
        kernel: Kernel::new(true),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let mut bad = Framed::copy(4);
    bad.tasks = 2;
    assert_eq!(
        load(&mut claim, &mut stream, 0, &bad, &[]),
        Err(Status::INVALID_ARGUMENT)
    );
    // Longer than the buffer, a buffer not on the stream, nothing.
    assert_eq!(
        claim.load(&mut stream, 0, BUFFER_BYTES as u32 + 1),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.load(&mut stream, BUFFERS, 100),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(claim.load(&mut stream, 0, 0), Err(Status::INVALID_ARGUMENT));
    let program = load(&mut claim, &mut stream, 0, &Framed::copy(4), &[]).unwrap();
    assert_eq!(
        claim.attach(&mut stream, program, 2, 1, 4),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, program, 0, 1, BUFFER_BYTES as u32 + 1),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, program, 0, BUFFERS, 4),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, program, 0, 1, 0),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, 7, 0, 1, 4),
        Err(Status::NOT_FOUND)
    );
    claim.attach(&mut stream, program, 0, 1, 4).unwrap();
    assert_eq!(claim.detach(program, 0), Ok(()));
    assert_eq!(claim.detach(program, 0), Err(Status::NOT_FOUND));
    assert_eq!(claim.unload(program), Ok(()));
    assert_eq!(claim.unload(program), Err(Status::NOT_FOUND));
}

#[test]
fn a_relocation_past_the_attached_bytes_is_refused() {
    let mut service = Service::new(Node {
        kernel: Kernel::new(true),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let mut framed = Framed::copy(4);
    framed.relocations[0] = relocation(32, 0, 8, 100);
    let program = load(&mut claim, &mut stream, 0, &framed, &[]).unwrap();
    assert_eq!(
        claim.attach(&mut stream, program, 0, 1, 100),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(claim.attach(&mut stream, program, 0, 1, 101), Ok(()));
}

#[test]
fn the_contract_is_served_through_its_trait() {
    let mut service = Service::new(Node {
        kernel: Kernel::new(true),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let bytes = Framed::copy(4).bytes();
    stream.fill(0, &bytes);
    let mut npu = Served {
        claim: &mut claim,
        stream: &mut stream,
        name: "npu0",
    };
    let mut name = [0u8; 32];
    assert_eq!(npu.job_stream(&mut name), Ok(4));
    assert_eq!(&name[..4], b"npu0");
    assert_eq!(npu.info().unwrap().max_programs, 15);
    let program = npu.load(0, bytes.len() as u32).unwrap();
    npu.attach(program, 0, 1, 4).unwrap();
    npu.attach(program, 1, 2, 4).unwrap();
    npu.detach(program, 1).unwrap();
    assert_eq!(npu.detach(program, 1), Err(Status::NOT_FOUND));
    npu.unload(program).unwrap();
    assert_eq!(npu.counters().unwrap().jobs, 0);
}

#[test]
fn undecodable_jobs_and_results_off_the_stream_fault_the_consumer() {
    let mut service = Service::new(Node {
        kernel: Kernel::new(true),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    stream.post_job(&[1, 2, 3]);
    post(&stream, job(1, 1, 2));
    // Faulted at the bytes that are no job: the job behind them is never read.
    assert_eq!(claim.serve(&mut stream, 0), Ok(1));
    assert_eq!(stream.faults(), 1);
    assert!(stream.take_frame().is_none());
    assert_eq!(claim.counters().errors, 1);
    assert_eq!(claim.last_failure(), Some(Failure::Undecodable));
    assert!((0..BUFFERS).all(|buffer| !claim.holds(buffer)), "given up");
    // A result buffer past the stream's last is the same broken consumer.
    post(&stream, job(1, 1, BUFFERS + 5));
    assert_eq!(claim.serve(&mut stream, 0), Ok(1));
    assert_eq!(stream.faults(), 2);
    assert!(stream.take_frame().is_none());
    assert_eq!(claim.counters().errors, 2);
    assert_eq!(claim.last_failure(), Some(Failure::Result));
}

// ---------------------------------------------------------------------------------------------
// Confinement

/// A program that reads `len` bytes at a fixed address instead of a relocated one.
fn reading_at(address: u64, len: u32) -> Framed {
    let mut framed = Framed::copy(len);
    framed.relocations.remove(0);
    framed.body[32..40].copy_from_slice(&address.to_le_bytes());
    framed
}

#[test]
fn each_program_runs_in_a_domain_holding_only_its_own_objects() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let mut with_scratch = Framed::copy(4);
    with_scratch.scratch = 100;
    let a = load(&mut claim, &mut stream, 0, &with_scratch, &[(1, 4), (2, 4)]).unwrap();
    let b = load(
        &mut claim,
        &mut stream,
        4,
        &Framed::copy(4),
        &[(5, 4), (6, 4)],
    )
    .unwrap();
    let (da, db) = (
        claim.program_domain(a).unwrap(),
        claim.program_domain(b).unwrap(),
    );
    assert_ne!(da, db);
    assert_ne!(da, Domain::DEFAULT);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, a, 3)).1,
        RunStatus::Done
    );
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, b, 7)).1,
        RunStatus::Done
    );
    let seen = kernel.lock().unwrap().seen.clone();
    // Body, scratch and two slots for a; body and two slots for b; nothing of the other.
    assert_eq!(seen[0].domain, da.id());
    assert_eq!(seen[0].reachable, handles_in(&kernel, da.id()));
    assert_eq!(seen[0].reachable.len(), 4);
    assert_eq!(seen[1].domain, db.id());
    assert_eq!(seen[1].reachable, handles_in(&kernel, db.id()));
    assert_eq!(seen[1].reachable.len(), 3);
    assert!(
        seen[0]
            .reachable
            .iter()
            .all(|h| !seen[1].reachable.contains(h))
    );
}

#[test]
fn a_program_naming_another_programs_address_faults() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let a = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(4),
        &[(1, 4), (2, 4)],
    )
    .unwrap();
    stream.fill(1, b"mine");
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, a, 3)).1,
        RunStatus::Done
    );
    // Every address a's objects have, in a's domain.
    let targets: Vec<(u64, u64)> = kernel
        .lock()
        .unwrap()
        .objects
        .values()
        .map(|o| (o.iova, o.size))
        .collect();
    // b gets a domain of its own, so the same numbers there are b's own objects or nothing; one
    // past b's own body and slots is nothing.
    let probe = targets
        .iter()
        .map(|&(iova, size)| iova + size)
        .max()
        .unwrap()
        + 8 * PAGE;
    let b = load(
        &mut claim,
        &mut stream,
        4,
        &reading_at(probe, 4),
        &[(5, 4), (6, 4)],
    )
    .unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, b, 7)).1,
        RunStatus::Failed
    );
    assert_eq!(claim.last_failure(), Some(Failure::Device));
    // a's slot 0 address, named from another program, is that program's own slot 0 in its own
    // domain (made in the same order), never a's bytes.
    let a_slot0 = targets[1].0;
    let c = load(
        &mut claim,
        &mut stream,
        4,
        &reading_at(a_slot0, 4),
        &[(8, 4), (9, 4)],
    )
    .unwrap();
    stream.fill(8, b"cccc");
    assert_eq!(
        run_one(&mut claim, &mut stream, job(3, c, 3)).1,
        RunStatus::Done
    );
    assert_eq!(&stream.contents(9)[..4], b"dddd");
    assert_eq!(&stream.contents(2)[..4], b"njof");
}

#[test]
fn a_kernel_without_domain_switching_takes_one_program_at_a_time() {
    let kernel = Kernel::new(false);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let a = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(4),
        &[(1, 4), (2, 4)],
    )
    .unwrap();
    // A second program would share the one domain with a's objects.
    assert_eq!(
        load(&mut claim, &mut stream, 4, &Framed::copy(4), &[]),
        Err(Status::BUSY)
    );
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, a, 3)).1,
        RunStatus::Done
    );
    let seen = kernel.lock().unwrap().seen.clone();
    assert_eq!(seen[0].reachable, handles_in(&kernel, 0));
    assert_eq!(seen[0].reachable.len(), 3);
    claim.unload(a).unwrap();
    assert!(handles_in(&kernel, 0).is_empty());
    let b = load(
        &mut claim,
        &mut stream,
        4,
        &Framed::copy(4),
        &[(5, 4), (6, 4)],
    )
    .unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, b, 7)).1,
        RunStatus::Done
    );
    assert_eq!(kernel.lock().unwrap().seen[1].reachable.len(), 3);
}

#[test]
fn an_object_whose_destroy_failed_keeps_its_domain_out_of_use() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let a = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(4),
        &[(1, 4), (2, 4)],
    )
    .unwrap();
    let domain = claim.program_domain(a).unwrap();
    // Re-attaching slot 0 destroys the old copy; the kernel refuses, so the old copy stays
    // mapped in a's domain, attached to nothing.
    kernel.lock().unwrap().fail_destroys = 2;
    claim.attach(&mut stream, a, 0, 8, 4).unwrap();
    assert_eq!(handles_in(&kernel, domain.id()).len(), 4);
    // The run first tries again (refused once more), then refuses to start beside it.
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, a, 3)).1,
        RunStatus::Refused
    );
    assert_eq!(kernel.lock().unwrap().submits, 0);
    // The next try frees it, and the run sees only what is attached.
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, a, 3)).1,
        RunStatus::Done
    );
    let handles = handles_in(&kernel, domain.id());
    assert_eq!(kernel.lock().unwrap().seen[0].reachable, handles);
    assert_eq!(kernel.lock().unwrap().seen[0].reachable.len(), 3);
    // An unload whose destroys fail leaves the domain dirty: the next load takes another one.
    kernel.lock().unwrap().fail_destroys = 1;
    claim.unload(a).unwrap();
    assert_eq!(handles_in(&kernel, domain.id()).len(), 1);
    kernel.lock().unwrap().fail_destroys = 1;
    let b = load(
        &mut claim,
        &mut stream,
        4,
        &Framed::copy(4),
        &[(5, 4), (6, 4)],
    )
    .unwrap();
    assert_ne!(claim.program_domain(b), Some(domain));
}

#[test]
fn unload_and_detach_unmap_everything_they_made() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let mut framed = Framed::copy(4);
    framed.scratch = 4096;
    let a = load(&mut claim, &mut stream, 0, &framed, &[(1, 4), (2, 4)]).unwrap();
    let domain = claim.program_domain(a).unwrap().id();
    assert_eq!(handles_in(&kernel, domain).len(), 4);
    claim.detach(a, 1).unwrap();
    assert_eq!(handles_in(&kernel, domain).len(), 3);
    claim.unload(a).unwrap();
    assert!(handles_in(&kernel, domain).is_empty());
    assert_eq!(claim.objects().count(), 0);
    // The same domain is given to the next program, holding only that program.
    let b = load(
        &mut claim,
        &mut stream,
        4,
        &Framed::copy(4),
        &[(5, 4), (6, 4)],
    )
    .unwrap();
    assert_eq!(claim.program_domain(b).unwrap().id(), domain);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, b, 7)).1,
        RunStatus::Done
    );
    assert_eq!(kernel.lock().unwrap().seen[0].reachable.len(), 3);
    // Dropping the claim destroys the rest.
    let file = 1;
    drop(claim);
    assert_eq!(kernel.lock().unwrap().ours(file), 0);
}

#[test]
fn another_opener_in_the_same_domain_is_beyond_the_driver() {
    // Why the node must be the daemon's alone: an object another file makes in a program's
    // domain is mapped while the program runs, and the driver cannot see it.
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let a = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(4),
        &[(1, 4), (2, 4)],
    )
    .unwrap();
    let domain = claim.program_domain(a).unwrap().id();
    kernel.lock().unwrap().make(99, domain, PAGE, false);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, a, 3)).1,
        RunStatus::Done
    );
    assert_eq!(kernel.lock().unwrap().seen[0].foreign, 1);
}

#[test]
fn leftovers_past_a_buffers_bytes_are_cleared_before_each_run() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    // Reads 8 bytes from slot 0, of which only 4 are the attached buffer's, and writes them to
    // slot 1 and back over slot 0, past its 4 bytes.
    let mut framed = Framed::copy(8);
    framed.body.resize(56, 0);
    framed.relocations.push(relocation(48, 0, 8, 0));
    let a = load(&mut claim, &mut stream, 0, &framed, &[(1, 4), (2, 8)]).unwrap();
    stream.fill(1, &[1, 1, 1, 1, 9, 9, 9, 9]);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, a, 3)).1,
        RunStatus::Done
    );
    assert_eq!(&stream.contents(2)[..8], &[2, 2, 2, 2, 1, 1, 1, 1]);
    // Only the buffer's 4 bytes came back; the 1s the block left past them in the slot's object
    // are gone before the next run.
    assert_eq!(&stream.contents(1)[..8], &[2, 2, 2, 2, 9, 9, 9, 9]);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, a, 3)).1,
        RunStatus::Done
    );
    assert_eq!(&stream.contents(2)[..8], &[3, 3, 3, 3, 1, 1, 1, 1]);
}

#[test]
fn runs_in_a_row_allocate_nothing() {
    let kernel = Kernel::new(true);
    let mut service = Service::new(Node {
        kernel: kernel.clone(),
    });
    let mut claim = service.claim(0).unwrap();
    let mut stream = stream();
    let a = load(
        &mut claim,
        &mut stream,
        0,
        &Framed::copy(64),
        &[(1, 64), (2, 64)],
    )
    .unwrap();
    kernel.lock().unwrap().record = false;
    let results = [3u32, 4, 5, 6];
    let round = |n: u64, stream: &mut MockStream, claim: &mut Claim<Node>| {
        assert_eq!(claim.serve(stream, n), Ok(results.len() as u32));
        for _ in results {
            let frame = stream.take_frame().unwrap();
            stream.return_buffer(frame.buffer);
        }
    };
    let mut jobs = 0;
    let mut count = 0;
    for n in 0..260 {
        // Posting a job is the consumer's side, and the played consumer allocates to do it.
        for (i, &result) in results.iter().enumerate() {
            post(&stream, job(n * 4 + i as u64, a, result));
            jobs += 1;
        }
        let ((), allocated) = allocations(|| round(n, &mut stream, &mut claim));
        // The first rounds warm the played stream's queues up.
        if n >= 10 {
            count += allocated;
        }
    }
    assert_eq!(count, 0, "allocations in 1000 runs");
    assert_eq!(claim.counters().jobs, jobs);
    assert_eq!(kernel.lock().unwrap().submits, jobs);
}

#[test]
fn the_node_is_the_one_render_node_bound_to_the_npu_driver() {
    let root = std::env::temp_dir().join(format!("rknpu-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let drivers = root.join("bus/platform/drivers");
    let bind = |node: &str, driver: &str| {
        std::fs::create_dir_all(drivers.join(driver)).unwrap();
        let device = root.join("class/drm").join(node).join("device");
        std::fs::create_dir_all(&device).unwrap();
        std::os::unix::fs::symlink(drivers.join(driver), device.join("driver")).unwrap();
    };
    assert_eq!(crate::node::find_in(&root), None);
    bind("renderD128", "rockchip-drm");
    bind("card1", uapi::DRIVER_NAME);
    assert_eq!(crate::node::find_in(&root), None);
    bind("renderD129", uapi::DRIVER_NAME);
    assert_eq!(
        crate::node::find_in(&root).as_deref(),
        Some(std::path::Path::new("/dev/dri/renderD129"))
    );
    // Two would be a guess.
    bind("renderD130", uapi::DRIVER_NAME);
    assert_eq!(crate::node::find_in(&root), None);
    std::fs::remove_dir_all(&root).unwrap();
}

mod host;
