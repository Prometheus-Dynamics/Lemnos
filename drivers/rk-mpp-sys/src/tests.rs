//! The session against files that are not the service (the request fails cleanly), and against a
//! played kernel that reads and writes through the very pointers the session issues. No device
//! node is opened; buffers are sealed memfds.

use std::cell::RefCell;
use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lemnos_driver_rk_mpp::uapi::{
    Command, IOC_CFG_V1, MAX_MSG_NUM, MsgV1, RegOffsetElem, client, flags,
};
use lemnos_driver_rk_mpp::{
    Address, Block, ClaimError, Device, Job, Message, READ_BACK_BYTES, READ_BACK_SLOTS, ReadBack,
    Service, Session, SubmitError,
};
use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::{DATA_BYTES, ServiceNode, ServiceSession, driver_name};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

// ---------------------------------------------------------------------------------------------
// The played kernel

/// What the played kernel saw and holds.
pub(crate) struct Record {
    /// Calls issued.
    pub(crate) calls: usize,
    /// The last call's chain, as issued.
    pub(crate) wire: [MsgV1; MAX_MSG_NUM],
    pub(crate) n: usize,
    /// The bytes the kernel read at each message's `data_ptr` during the last call.
    pub(crate) bytes: [[u8; 1024]; 4],
    /// Read-backs queued (`data_ptr`, size), oldest at `head`.
    reads: [(u64, u32); READ_BACK_SLOTS],
    head: usize,
    queued: usize,
    /// The session's read-back area 0 and scratch area, noted when it was opened.
    pub(crate) area0: usize,
    pub(crate) scratch: std::ops::Range<usize>,
}

/// Issues a chain the way `mpp_common.c` treats it, as far as memory goes: reads `size` bytes at
/// each input's `data_ptr` during the call, writes a query's `u32` answer, keeps each read-back's
/// pointer and writes the registers there at the poll that finishes the task.
pub(crate) struct Played {
    rec: Rc<RefCell<Record>>,
}

/// The registers the played block reads back: word `k` of every read is this.
fn pattern(k: usize) -> u32 {
    0xC0DE_0000 | k as u32
}

/// The played kernel's answers: every client type present, and this hardware id.
const HW_ID: u32 = 0x5A5A_0001;

impl Played {
    pub(crate) fn issue(&mut self, wire: &[MsgV1]) -> io::Result<()> {
        let mut rec = self.rec.borrow_mut();
        rec.calls += 1;
        rec.n = wire.len();
        rec.wire[..wire.len()].copy_from_slice(wire);
        for (i, m) in wire.iter().enumerate() {
            let at = std::ptr::with_exposed_provenance_mut::<u8>(m.data_ptr as usize);
            match m.cmd {
                c if c == Command::QueryHwSupport.code() || c == Command::QueryHwId.code() => {
                    let answer = if c == Command::QueryHwId.code() {
                        HW_ID
                    } else {
                        (1 << client::END) - 1
                    };
                    // SAFETY: the session pointed this message at 4 bytes of its scratch area,
                    // live for the call.
                    unsafe { at.cast::<[u8; 4]>().write_unaligned(answer.to_ne_bytes()) };
                }
                c if c == Command::SetRegRead.code() => {
                    let slot = (rec.head + rec.queued) % READ_BACK_SLOTS;
                    rec.reads[slot] = (m.data_ptr, m.size);
                    rec.queued += 1;
                }
                c if c == Command::PollHwFinish.code() => {
                    if rec.queued == 0 {
                        continue;
                    }
                    let (ptr, size) = rec.reads[rec.head];
                    rec.head = (rec.head + 1) % READ_BACK_SLOTS;
                    rec.queued -= 1;
                    for k in 0..size as usize / 4 {
                        let word =
                            std::ptr::with_exposed_provenance_mut::<u8>(ptr as usize + 4 * k);
                        // SAFETY: the session pointed the read-back at one of its read-back
                        // areas, `size` bytes, which it keeps until its file is closed; this is a
                        // later call on that session, as the kernel's poll is.
                        unsafe {
                            word.cast::<[u8; 4]>()
                                .write_unaligned(pattern(k).to_ne_bytes())
                        };
                    }
                }
                _ if m.size > 0 => {
                    let size = m.size as usize;
                    let into = rec.bytes[i].as_mut_ptr();
                    // SAFETY: the session pointed the message at `size` bytes of its scratch
                    // area, live for the call; `into` is the record's own 1024 bytes, more than
                    // any message here.
                    unsafe { std::ptr::copy_nonoverlapping(at.cast_const(), into, size) };
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// A file that exists and is not the service node.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rk-mpp-sys-{name}-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A node whose sessions issue to a played kernel.
struct PlayedNode {
    file: TempFile,
    rec: Rc<RefCell<Record>>,
}

impl PlayedNode {
    fn new(name: &str) -> Self {
        Self {
            file: TempFile::new(name),
            rec: Rc::new(RefCell::new(Record {
                calls: 0,
                wire: [MsgV1::default(); MAX_MSG_NUM],
                n: 0,
                bytes: [[0; 1024]; 4],
                reads: [(0, 0); READ_BACK_SLOTS],
                head: 0,
                queued: 0,
                area0: 0,
                scratch: 0..0,
            })),
        }
    }

    fn session(&self, allowed: &[Block]) -> ServiceSession {
        let mut session = ServiceSession::open(&self.file.0, allowed).unwrap();
        session.played = Some(Played {
            rec: self.rec.clone(),
        });
        let mut rec = self.rec.borrow_mut();
        rec.area0 = session.area(0).addr();
        rec.scratch = session.scratch_range();
        session
    }
}

impl Device for PlayedNode {
    type Session = ServiceSession;

    fn open(&mut self) -> io::Result<ServiceSession> {
        Ok(self.session(&Block::ALL))
    }

    fn confined(&mut self, _: Block) -> io::Result<bool> {
        Ok(true)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn memfd(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"mpp-sys-test", len).unwrap()
}

/// A fake sysfs: `confined` blocks have one device with an `iommu_group`, `bare` blocks one
/// without.
struct Sysfs(PathBuf);

impl Sysfs {
    fn new(name: &str, confined: &[Block], bare: &[Block]) -> Self {
        let root =
            std::env::temp_dir().join(format!("rk-mpp-sys-sysfs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let devices = root.join("devices");
        std::fs::create_dir_all(root.join("kernel/iommu_groups/3")).unwrap();
        for (i, (&block, iommu)) in confined
            .iter()
            .map(|b| (b, true))
            .chain(bare.iter().map(|b| (b, false)))
            .enumerate()
        {
            let device = devices.join(format!("fdee{i}000.{}", driver_name(block)));
            std::fs::create_dir_all(&device).unwrap();
            if iommu {
                std::os::unix::fs::symlink(
                    root.join("kernel/iommu_groups/3"),
                    device.join("iommu_group"),
                )
                .unwrap();
            }
            let driver = root.join("bus/platform/drivers").join(driver_name(block));
            std::fs::create_dir_all(&driver).unwrap();
            std::os::unix::fs::symlink(&device, driver.join(device.file_name().unwrap())).unwrap();
            std::os::unix::fs::symlink(root.join("module"), driver.join("module")).unwrap();
            std::fs::write(driver.join("bind"), b"").unwrap();
        }
        Self(root)
    }
}

impl Drop for Sysfs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn einval(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EINVAL)
}

// ---------------------------------------------------------------------------------------------
// Layouts

#[test]
fn the_request_and_message_layout_are_pinned() {
    // _IOW('v', 1, unsigned int), and struct mpp_msg_v1 as gcc lays it out on LP64.
    assert_eq!(IOC_CFG_V1, 0x4004_7601);
    assert_eq!(size_of::<MsgV1>(), 24);
    assert_eq!(align_of::<MsgV1>(), 8);
    assert_eq!(offset_of!(MsgV1, cmd), 0);
    assert_eq!(offset_of!(MsgV1, flags), 4);
    assert_eq!(offset_of!(MsgV1, size), 8);
    assert_eq!(offset_of!(MsgV1, offset), 12);
    assert_eq!(offset_of!(MsgV1, data_ptr), 16);
    assert_eq!(size_of::<RegOffsetElem>(), 8);
    assert_eq!(MAX_MSG_NUM, 16);
    // The largest job fits the scratch area: every register of the largest block, a full table
    // and a full offset list.
    let largest = 4 * lemnos_driver_rk_mpp::MAX_REGISTERS
        + 2 * lemnos_driver_rk_mpp::MAX_ADDRESSES
        + 8 * lemnos_driver_rk_mpp::MAX_ADDRESSES;
    assert!(largest + 3 * 8 <= DATA_BYTES);
}

// ---------------------------------------------------------------------------------------------
// Files that are not the service

#[test]
fn a_regular_file_answers_unsupported() {
    let file = TempFile::new("regular");
    let mut session = ServiceSession::open(&file.0, &Block::ALL).unwrap();
    let e = session.hw_support().unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{e:?}");
    let e = session.bind(Block::JpegDecoder).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{e:?}");
    assert_eq!(session.bound(), None);
    assert_eq!(
        session.reset().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    // Unbound: refused before any call.
    assert!(einval(&session.hw_id().unwrap_err()));
    drop(session);

    // Through the driver: the claim fails as a device failure and backs off.
    let sysfs = Sysfs::new("regular", &[Block::JpegDecoder], &[]);
    let mut service = Service::new(ServiceNode::with_sysfs(&file.0, &sysfs.0));
    match service.claim(Block::JpegDecoder, 0) {
        Err(ClaimError::Device { error, retry_at_ns }) => {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error:?}");
            assert_eq!(retry_at_ns, 250_000_000);
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
}

#[test]
fn a_pipe_answers_unsupported() {
    let (reader, writer) = std::io::pipe().unwrap();
    let path = PathBuf::from(format!("/proc/self/fd/{}", writer.as_raw_fd()));
    let mut session = ServiceSession::open(&path, &Block::ALL).unwrap();
    let e = session.hw_support().unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{e:?}");
    drop((reader, writer));
}

#[test]
fn a_missing_node_fails_to_open() {
    let e = ServiceSession::open(Path::new("/nonexistent/mpp-node"), &Block::ALL).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}

// ---------------------------------------------------------------------------------------------
// Confinement

#[test]
fn confinement_is_read_from_sysfs() {
    let sysfs = Sysfs::new(
        "confined",
        &[Block::JpegDecoder, Block::Vdpu2],
        &[Block::Vepu2],
    );
    let node = ServiceNode::with_sysfs("/nonexistent", &sysfs.0);
    assert!(node.is_confined(Block::JpegDecoder).unwrap());
    assert!(node.is_confined(Block::Vdpu2).unwrap());
    // A device bound without an IOMMU group.
    assert!(!node.is_confined(Block::Vepu2).unwrap());
    // No driver.
    assert!(!node.is_confined(Block::JpegEncoder).unwrap());

    // A second device of the same driver without a group makes the block unconfined.
    let driver = sysfs.0.join("bus/platform/drivers/mpp_jpgdec");
    let extra = sysfs.0.join("devices/fdef0000.mpp_jpgdec");
    std::fs::create_dir_all(&extra).unwrap();
    std::os::unix::fs::symlink(&extra, driver.join("fdef0000.mpp_jpgdec")).unwrap();
    assert!(!node.is_confined(Block::JpegDecoder).unwrap());
}

// ---------------------------------------------------------------------------------------------
// What is issued

#[test]
fn a_job_is_issued_from_the_sessions_own_memory() {
    let node = PlayedNode::new("job");
    let rec = node.rec.clone();
    let mut service = Service::new(node);
    let mut claim = service.claim(Block::JpegDecoder, 0).unwrap();
    assert_eq!(rec.borrow().calls, 2, "query, bind");

    let a = memfd(4096);
    let b = memfd(8192);
    let (fd_a, fd_b) = (a.as_raw_fd() as u32, b.as_raw_fd() as u32);
    let a = claim.add_buffer(a).unwrap();
    let b = claim.add_buffer(b).unwrap();
    let registers: Vec<u32> = (0..42).map(|i| i * 0x10).collect();
    let addresses = [
        Address {
            register: 5,
            buffer: a,
            offset: 0,
        },
        Address {
            register: 6,
            buffer: b,
            offset: 16,
        },
    ];
    let job = Job {
        identity: 7,
        first: 0,
        registers: &registers,
        addresses: &addresses,
        read: Some(ReadBack { first: 1, count: 3 }),
    };
    claim.submit(&job).unwrap();

    let rec = rec.borrow();
    assert_eq!(rec.calls, 3);
    assert_eq!(rec.n, 4);
    let wire = &rec.wire[..4];
    let cmds: Vec<u32> = wire.iter().map(|m| m.cmd).collect();
    assert_eq!(
        cmds,
        [
            Command::InitTransTable.code(),
            Command::SetRegWrite.code(),
            Command::SetRegAddrOffset.code(),
            Command::SetRegRead.code(),
        ]
    );
    let m = flags::MULTI_MSG;
    let flags: Vec<u32> = wire.iter().map(|m| m.flags).collect();
    assert_eq!(flags, [m, m, m, m | flags::LAST_MSG]);
    let sizes: Vec<u32> = wire.iter().map(|m| m.size).collect();
    assert_eq!(sizes, [4, 168, 8, 12]);
    let offsets: Vec<u32> = wire.iter().map(|m| m.offset).collect();
    assert_eq!(offsets, [0, 0, 0, 4]);

    // Inputs: one after the other in the scratch area, 8-byte steps.
    let base = rec.scratch.start as u64;
    assert_eq!(wire[0].data_ptr, base);
    assert_eq!(wire[1].data_ptr, base + 8);
    assert_eq!(wire[2].data_ptr, base + 8 + 168);
    for m in &wire[..3] {
        let start = m.data_ptr as usize;
        assert!(rec.scratch.contains(&start) && start + m.size as usize <= rec.scratch.end);
    }
    // The read-back: area 0, the first job's slot.
    assert_eq!(wire[3].data_ptr, rec.area0 as u64);

    // The bytes the kernel read at those pointers.
    let mut table = [0u8; 4];
    table[..2].copy_from_slice(&5u16.to_ne_bytes());
    table[2..].copy_from_slice(&6u16.to_ne_bytes());
    assert_eq!(rec.bytes[0][..4], table);
    let mut file: Vec<u8> = registers.iter().flat_map(|r| r.to_ne_bytes()).collect();
    file[20..24].copy_from_slice(&fd_a.to_ne_bytes());
    file[24..28].copy_from_slice(&fd_b.to_ne_bytes());
    assert_eq!(rec.bytes[1][..168], file[..]);
    let elem = RegOffsetElem {
        index: 6,
        offset: 16,
    };
    assert_eq!(rec.bytes[2][..8], elem.to_ne_bytes());
}

#[test]
fn read_back_outlives_the_callers_inputs() {
    let node = PlayedNode::new("lifetime");
    let rec = node.rec.clone();
    let mut service = Service::new(node);
    let mut claim = service.claim(Block::JpegEncoder, 0).unwrap();
    let buffer = claim.add_buffer(memfd(4096)).unwrap();

    // Two jobs in flight; everything the caller built them from is gone before they complete.
    for identity in [1, 2] {
        let registers: Vec<u32> = vec![0; 79];
        let addresses = vec![Address {
            register: 10,
            buffer,
            offset: 0,
        }];
        let job = Job {
            identity,
            first: 0,
            registers: &registers,
            addresses: &addresses,
            read: Some(ReadBack {
                first: 0,
                count: 79,
            }),
        };
        claim.submit(&job).unwrap();
        drop((registers, addresses));
    }
    let second = rec.borrow().wire[2].data_ptr;
    assert_eq!(rec.borrow().wire[2].cmd, Command::SetRegRead.code());
    assert_eq!(second, (rec.borrow().area0 + READ_BACK_BYTES) as u64);

    // The claim (and the session in it) moves to the heap and back; the areas do not move.
    let mut claim = *Box::new(claim);
    let mut into = [0u32; 79];
    for identity in [1, 2] {
        into.fill(0);
        let done = claim.complete(&mut into).unwrap();
        assert_eq!(done.identity, identity);
        assert_eq!(done.registers, 79);
        for (k, word) in into.iter().enumerate() {
            assert_eq!(*word, pattern(k));
        }
    }
}

#[test]
fn steady_state_submit_and_complete_allocate_nothing() {
    let node = PlayedNode::new("alloc");
    let mut service = Service::new(node);
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    let input = claim.add_buffer(memfd(8192)).unwrap();
    let output = claim.add_buffer(memfd(8192)).unwrap();
    let registers = [0u32; 184];
    let addresses = [
        Address {
            register: 12,
            buffer: input,
            offset: 64,
        },
        Address {
            register: 13,
            buffer: output,
            offset: 0,
        },
    ];
    let job = Job {
        identity: 0,
        first: 0,
        registers: &registers,
        addresses: &addresses,
        read: Some(ReadBack {
            first: 100,
            count: 84,
        }),
    };
    let mut into = [0u32; 184];
    let ((), n) = allocations(|| {
        for _ in 0..100 {
            claim.submit(&job).unwrap();
            claim.submit(&job).unwrap();
            claim.complete(&mut into).unwrap();
            claim.complete(&mut into).unwrap();
        }
    });
    assert_eq!(n, 0);
    assert_eq!(into[83], pattern(83));
}

// ---------------------------------------------------------------------------------------------
// What is refused

/// How a hand-written session in front of the real one mangles the driver's chains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mangle {
    /// Leaves the translation table out of a job, so the kernel's own table would apply.
    DropTable,
    /// Leaves the last message out of a job, so the chain no longer ends.
    DropLast,
    /// Passes every chain on as it is.
    None,
}

struct Wrapper {
    inner: ServiceSession,
    mangle: Mangle,
}

impl Session for Wrapper {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        let n = messages.len();
        match self.mangle {
            Mangle::DropTable if n > 1 => self.inner.send(&mut messages[1..]),
            Mangle::DropLast if n > 1 => self.inner.send(&mut messages[..n - 1]),
            _ => self.inner.send(messages),
        }
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        self.inner.read_back(slot, into);
    }
}

struct WrapperNode {
    node: PlayedNode,
    mangle: Mangle,
}

impl Device for WrapperNode {
    type Session = Wrapper;

    fn open(&mut self) -> io::Result<Wrapper> {
        Ok(Wrapper {
            inner: self.node.session(&Block::ALL),
            mangle: self.mangle,
        })
    }

    fn confined(&mut self, _: Block) -> io::Result<bool> {
        Ok(true)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn mangled_submit(mangle: Mangle) -> (Result<(), SubmitError>, usize) {
    let node = PlayedNode::new(&format!("{mangle:?}"));
    let rec = node.rec.clone();
    let mut service = Service::new(WrapperNode { node, mangle });
    let mut claim = service.claim(Block::JpegDecoder, 0).unwrap();
    let buffer = claim.add_buffer(memfd(4096)).unwrap();
    let registers = [0u32; 42];
    let addresses = [Address {
        register: 3,
        buffer,
        offset: 8,
    }];
    let job = Job {
        identity: 0,
        first: 0,
        registers: &registers,
        addresses: &addresses,
        read: Some(ReadBack { first: 0, count: 2 }),
    };
    let before = rec.borrow().calls;
    let result = claim.submit(&job);
    let issued = rec.borrow().calls - before;
    (result, issued)
}

#[test]
fn chains_the_driver_never_builds_are_refused_before_the_call() {
    for mangle in [Mangle::DropTable, Mangle::DropLast] {
        match mangled_submit(mangle) {
            (Err(SubmitError::Device(e)), 0) if einval(&e) => {}
            (result, issued) => panic!("{mangle:?}: {result:?}, {issued} calls"),
        }
    }
    let (result, issued) = mangled_submit(Mangle::None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(issued, 1);
}

#[test]
fn a_session_binds_once_and_only_to_blocks_it_was_opened_for() {
    let node = PlayedNode::new("bind");
    let mut session = node.session(&[Block::JpegDecoder]);
    assert!(einval(&session.bind(Block::Vdpu2).unwrap_err()));
    assert!(einval(&session.hw_id().unwrap_err()));
    assert_eq!(node.rec.borrow().calls, 0);

    assert_eq!(session.hw_support().unwrap(), (1 << client::END) - 1);
    session.bind(Block::JpegDecoder).unwrap();
    assert_eq!(session.bound(), Some(Block::JpegDecoder));
    assert_eq!(session.hw_id().unwrap(), HW_ID);
    {
        let rec = node.rec.borrow();
        assert_eq!(rec.wire[0].cmd, Command::QueryHwId.code());
        assert_eq!(rec.wire[0].size, 4);
        assert_eq!(rec.wire[0].data_ptr, rec.scratch.start as u64);
    }
    let calls = node.rec.borrow().calls;
    assert!(einval(&session.bind(Block::JpegDecoder).unwrap_err()));
    assert_eq!(node.rec.borrow().calls, calls);
    session.reset().unwrap();
    assert_eq!(node.rec.borrow().wire[0].cmd, Command::ResetSession.code());

    // A session opened for no block binds to none.
    let mut other = node.session(&[]);
    let calls = node.rec.borrow().calls;
    assert!(einval(&other.bind(Block::Vdpu2).unwrap_err()));
    assert_eq!(node.rec.borrow().calls, calls);
}
