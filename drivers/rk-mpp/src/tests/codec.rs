//! The block as `codec.block/1` has it, through the job server and the mock stream (sealed
//! memfds standing in for the stream's dma-bufs), on the played kernel.

use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::rc::Rc;

use lemnos_contract_codec_block::work::Builder;
use lemnos_contract_codec_block::{
    BufferRef, COMPLETION_BYTES, Completion, DATA_OFFSET, JOB_BYTES, JobStatus,
};
use lemnos_core::{Decode, Encode, Health, Reader, Writer};
use lemnos_driver_codec_jobs::{Block as _, POLL_NS, Server};
use lemnos_hal::StreamEndpoint as _;
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf::sealed_memfd;

use super::{PlayedDevice, all_blocks};
use crate::codec::{ABSENT, MppBlock, NOT_CONFINED, NOT_THERE, TAG_READ, TAG_WRITE, block_named};
use crate::played::{Kernel, iova};
use crate::{Block, MAX_BUFFERS, MAX_REGISTERS};

const BUFFERS: u32 = 8;
const BYTES: u64 = 8192;

struct Rig {
    server: Server,
    block: MppBlock<PlayedDevice>,
    stream: MockStream,
    consumer: MockStream,
    kernel: Rc<RefCell<Kernel>>,
    now: u64,
}

fn reference(register: u32, buffer: u32, offset: u32, length: u32) -> BufferRef {
    BufferRef {
        slot: register,
        buffer,
        offset,
        length,
    }
}

/// The `TAG_WRITE` item for `values` from register `first`.
fn write(first: u32, values: &[u32]) -> Vec<u8> {
    let mut bytes = first.to_le_bytes().to_vec();
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn read(first: u32, count: u32) -> Vec<u8> {
    [first.to_le_bytes(), count.to_le_bytes()].concat()
}

fn regs() -> Vec<u32> {
    (0..32).map(|i| 0xa500_0000 | i).collect()
}

impl Rig {
    fn new(block: Block) -> Self {
        let fds = (0..BUFFERS)
            .map(|_| sealed_memfd(c"mpp-codec-test", BYTES).unwrap())
            .collect();
        let stream = MockStream::with_handles(fds);
        let kernel = Rc::new(RefCell::new(Kernel::new(all_blocks())));
        Self {
            server: Server::new(),
            block: MppBlock::new(PlayedDevice(Rc::clone(&kernel)), block),
            consumer: stream.clone(),
            stream,
            kernel,
            now: 0,
        }
    }

    /// Posts a job whose work is in buffer 0 and whose result is buffer 1, ticks, and returns
    /// how it ended with its read-back as registers.
    fn run(&mut self, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) -> (JobStatus, Vec<u32>) {
        let mut bytes = vec![0u8; 4096];
        let mut work = Builder::new(&mut bytes).unwrap();
        for r in refs {
            work.reference(*r).unwrap();
        }
        for (tag, data) in items {
            work.item(*tag, data).unwrap();
        }
        let len = work.finish();
        self.consumer.fill(0, &bytes[..len]);
        let job = lemnos_contract_codec_block::Job {
            identity: 5,
            work: 0,
            work_bytes: len as u32,
            result: 1,
        };
        let mut encoded = [0u8; JOB_BYTES];
        let mut writer = Writer::new(&mut encoded);
        job.encode(&mut writer).unwrap();
        self.consumer.post_job(&encoded);
        self.now += POLL_NS;
        self.server
            .serve(&mut self.stream, &mut self.block, self.now)
            .unwrap();
        let frame = self.consumer.take_frame().expect("a completion");
        assert_eq!((frame.identity, frame.buffer), (5, 1));
        let contents = self.consumer.contents(1);
        let record = Completion::decode(&mut Reader::new(&contents[..COMPLETION_BYTES])).unwrap();
        let back = contents[DATA_OFFSET..DATA_OFFSET + record.count as usize]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|four| u32::from_le_bytes(*four))
            .collect();
        self.consumer.return_buffer(1);
        (record.status, back)
    }

    fn refused(&mut self, what: &str, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) {
        let calls = self.kernel.borrow().calls;
        let (status, back) = self.run(refs, items);
        assert_eq!(status, JobStatus::Refused, "{what}");
        assert!(back.is_empty(), "{what}");
        // Nothing but, at most, the claim's own setup reached the kernel: no job did.
        let k = self.kernel.borrow();
        assert_eq!(k.queued, 0, "{what}");
        assert!(k.calls <= calls + 2, "{what}");
    }
}

#[test]
fn the_blocks_have_the_names_a_board_file_and_a_caller_use() {
    for block in Block::ALL {
        let kernel = Rc::new(RefCell::new(Kernel::new(0)));
        let name = MppBlock::new(PlayedDevice(kernel), block)
            .name()
            .to_string();
        assert!(name.len() <= lemnos_contract_codec_block::MAX_BLOCK_NAME);
        let short = name.strip_prefix("rk-mpp:").unwrap();
        // A parameter block is served by its own driver, never by this one.
        let expected = block.params().is_none().then_some(block);
        assert_eq!(block_named(short), expected);
    }
    assert_eq!(block_named("iep2"), None);
    assert_eq!(block_named("vdpu3"), None);
    assert_eq!(block_named("rk-mpp:vdpu2"), None);
}

#[test]
fn a_job_reaches_the_block_with_its_stream_buffers_translated_and_reads_back() {
    let mut rig = Rig::new(Block::Vepu2);
    assert!(!rig.block.claimed(), "nothing is opened before a job");
    let refs = [reference(8, 2, 0, 0), reference(9, 3, 4095, 1)];
    let (status, back) = rig.run(
        &refs,
        &[(TAG_WRITE, write(0, &regs())), (TAG_READ, read(0, 32))],
    );
    assert_eq!(status, JobStatus::Done);
    assert!(rig.block.claimed());
    assert_eq!(back.len(), 32);
    let fds: Vec<u32> = rig.kernel.borrow().translated[..2].to_vec();
    for (i, (&got, &wrote)) in back.iter().zip(&regs()).enumerate() {
        match i {
            8 => assert_eq!(got, iova(fds[0])),
            9 => assert_eq!(got, iova(fds[1]) + 4095),
            _ => assert_eq!(got, wrote, "register {i}"),
        }
    }
    // The second job names the same stream buffers: the claim already holds them.
    let (status, _) = rig.run(&refs, &[(TAG_WRITE, write(0, &regs()))]);
    assert_eq!(status, JobStatus::Done);
    let k = rig.kernel.borrow();
    assert_eq!(k.translated[2..4], fds[..], "the same two descriptors");
    assert_eq!(k.opens, 1);
}

#[test]
fn a_raw_value_in_an_address_register_never_reaches_the_kernel() {
    let mut rig = Rig::new(Block::Vepu2);
    // The caller writes a descriptor number of its own choosing where a buffer goes.
    let mut values = regs();
    values[8] = 3;
    let (status, back) = rig.run(
        &[reference(8, 2, 0, 0)],
        &[(TAG_WRITE, write(0, &values)), (TAG_READ, read(8, 1))],
    );
    assert_eq!(status, JobStatus::Done);
    let k = rig.kernel.borrow();
    assert_eq!(k.translated_len, 1);
    // The kernel translated the driver's descriptor for buffer 2, the same dma-buf. It is not
    // compared by number: the driver's descriptor is a duplicate of the stream's fd, which is the
    // lowest free number, so it can equal the caller's 3 (which is then not a leak: the driver put
    // its own descriptor there). Identity is the device and inode the descriptor names.
    let handle = rig
        .stream
        .buffer_handle(2)
        .unwrap()
        .expect("buffer 2 is held");
    assert!(
        same_file(k.translated[0], handle.fd().as_raw_fd()),
        "the kernel translated something other than buffer 2"
    );
    assert_eq!(back, [iova(k.translated[0])]);
}

/// Whether two descriptors of this process name the same file (device and inode).
fn same_file(a: u32, b: i32) -> bool {
    use std::os::unix::fs::MetadataExt;
    let at = |fd: i32| std::fs::metadata(format!("/proc/self/fd/{fd}")).unwrap();
    let (x, y) = (at(a as i32), at(b));
    (x.dev(), x.ino()) == (y.dev(), y.ino())
}

#[test]
fn work_of_the_wrong_shape_or_past_a_buffer_is_refused() {
    let mut rig = Rig::new(Block::Vepu2);
    let good = (TAG_WRITE, write(0, &regs()));
    let at = [reference(8, 2, 0, 0)];
    rig.refused("no write", &at, &[(TAG_READ, read(0, 4))]);
    rig.refused("no items", &at, &[]);
    rig.refused("two writes", &at, &[good.clone(), good.clone()]);
    rig.refused(
        "two reads",
        &at,
        &[good.clone(), (TAG_READ, read(0, 1)), (TAG_READ, read(0, 1))],
    );
    rig.refused("an unknown tag", &at, &[good.clone(), (3, vec![0; 4])]);
    rig.refused("a write with no register", &at, &[(TAG_WRITE, vec![0; 4])]);
    rig.refused(
        "a write of half a register",
        &at,
        &[(TAG_WRITE, vec![0; 10])],
    );
    rig.refused(
        "a first register past 16 bits",
        &at,
        &[(TAG_WRITE, write(0x1_0000, &[0]))],
    );
    rig.refused(
        "more registers than any block has",
        &at,
        &[(TAG_WRITE, write(0, &vec![0; MAX_REGISTERS + 1]))],
    );
    rig.refused(
        "a read of the wrong size",
        &at,
        &[good.clone(), (TAG_READ, vec![0; 4])],
    );
    rig.refused(
        "a read past 16 bits",
        &at,
        &[good.clone(), (TAG_READ, read(0, 0x1_0000))],
    );
    rig.refused(
        "a read past the block",
        &at,
        &[good.clone(), (TAG_READ, read(0, 4096))],
    );
    rig.refused(
        "a slot past 16 bits",
        &[reference(0x1_0008, 2, 0, 0)],
        std::slice::from_ref(&good),
    );
    rig.refused(
        "a slot outside the write",
        &[reference(40, 2, 0, 0)],
        std::slice::from_ref(&good),
    );
    rig.refused(
        "a slot named twice",
        &[reference(8, 2, 0, 0), reference(8, 3, 0, 0)],
        std::slice::from_ref(&good),
    );
    rig.refused("no reference", &[], std::slice::from_ref(&good));
    let size = BYTES as u32;
    rig.refused(
        "an offset at the end",
        &[reference(8, 2, size, 0)],
        std::slice::from_ref(&good),
    );
    rig.refused(
        "the largest offset",
        &[reference(8, 2, u32::MAX, 0)],
        std::slice::from_ref(&good),
    );
    rig.refused(
        "a length past the end",
        &[reference(8, 2, size - 4, 5)],
        std::slice::from_ref(&good),
    );
    rig.refused(
        "the largest offset and length",
        &[reference(8, 2, u32::MAX, u32::MAX)],
        std::slice::from_ref(&good),
    );
    // The last byte, and the whole buffer, are inside.
    let (status, _) = rig.run(&[reference(8, 2, size - 4, 4)], std::slice::from_ref(&good));
    assert_eq!(status, JobStatus::Done);
    let (status, _) = rig.run(&[reference(8, 2, 0, size)], &[good]);
    assert_eq!(status, JobStatus::Done);
    assert!(MAX_BUFFERS as u32 >= BUFFERS);
}

#[test]
fn a_stream_without_handles_has_no_buffer_to_give_the_block() {
    let stream = MockStream::with_bytes(BUFFERS, BYTES as usize);
    let mut rig = Rig::new(Block::Vepu2);
    rig.consumer = stream.clone();
    rig.stream = stream;
    rig.refused(
        "plain memory",
        &[reference(8, 2, 0, 0)],
        &[(TAG_WRITE, write(0, &regs()))],
    );
}

#[test]
fn a_consumer_that_goes_ends_the_claim_and_the_next_gets_a_session_of_its_own() {
    let mut rig = Rig::new(Block::Vepu2);
    let job = [(TAG_WRITE, write(0, &regs()))];
    let (status, _) = rig.run(&[reference(8, 2, 0, 0)], &job);
    assert_eq!(status, JobStatus::Done);
    assert_eq!(
        (rig.kernel.borrow().opens, rig.kernel.borrow().resets),
        (1, 0)
    );
    rig.consumer.change_consumer();
    let (status, _) = rig.run(&[reference(8, 2, 0, 0)], &job);
    assert_eq!(status, JobStatus::Done);
    let k = rig.kernel.borrow();
    // The first session was reset, which unmaps what it mapped, before the second opened.
    assert_eq!((k.opens, k.resets), (2, 1));
}

#[test]
fn a_block_that_cannot_be_claimed_fails_its_jobs_and_says_why() {
    let job = [(TAG_WRITE, write(0, &regs()))];
    let at = [reference(8, 2, 0, 0)];

    let mut rig = Rig::new(Block::Vepu2);
    rig.kernel.borrow_mut().hw_support = 0;
    assert_eq!(rig.run(&at, &job).0, JobStatus::Failed);
    assert_eq!(rig.block.health(), Health::Degraded(ABSENT));

    let mut rig = Rig::new(Block::Vepu2);
    rig.kernel.borrow_mut().confined = false;
    assert_eq!(rig.run(&at, &job).0, JobStatus::Failed);
    assert_eq!(rig.block.health(), Health::Degraded(NOT_CONFINED));
    assert_eq!(
        rig.kernel.borrow().queued,
        0,
        "an unconfined block is given nothing"
    );

    // A node that isn't there: the job fails, and a later one, after the backoff, runs.
    let mut rig = Rig::new(Block::Vepu2);
    rig.kernel.borrow_mut().fail_open = Some(2);
    assert_eq!(rig.run(&at, &job).0, JobStatus::Failed);
    assert_eq!(rig.block.health(), Health::Degraded(NOT_THERE));
    assert_eq!(rig.run(&at, &job).0, JobStatus::Failed, "backing off");
    rig.now += 8_000_000_000;
    assert_eq!(rig.run(&at, &job).0, JobStatus::Done);
    assert_eq!(rig.block.health(), Health::Ok);

    // A call the kernel fails: the session is given up, and the next job gets a new one.
    rig.kernel.borrow_mut().fail_send = Some(5);
    assert_eq!(rig.run(&at, &job).0, JobStatus::Failed);
    assert!(!rig.block.claimed());
    assert_eq!(rig.run(&at, &job).0, JobStatus::Done);
}

#[test]
fn running_jobs_allocates_nothing_once_the_block_is_claimed() {
    use lemnos_measure::test_alloc::allocations;
    let mut rig = Rig::new(Block::Vepu2);
    let job = [(TAG_WRITE, write(0, &regs())), (TAG_READ, read(0, 32))];
    let at = [reference(8, 2, 0, 0), reference(9, 3, 16, 0)];
    assert_eq!(rig.run(&at, &job).0, JobStatus::Done);
    // The same work again: the mock stream's own bookkeeping aside, the block and the server
    // allocate nothing. Measured around the block's part alone.
    let mut bytes = vec![0u8; 4096];
    let mut work = Builder::new(&mut bytes).unwrap();
    for r in &at {
        work.reference(*r).unwrap();
    }
    for (tag, data) in &job {
        work.item(*tag, data).unwrap();
    }
    let len = work.finish();
    let work = lemnos_contract_codec_block::work::Work::parse(&bytes[..len]).unwrap();
    let mut back = [0u8; 1024];
    let block = &mut rig.block;
    let buffers = lemnos_driver_codec_jobs::Buffers::naming(&rig.stream, 0b1100);
    let ((), allocated) = allocations(|| {
        for now in 0..100 {
            let started = block.start(&work, &buffers, &mut back, now);
            assert_eq!(started, lemnos_driver_codec_jobs::Start::Done(128));
        }
    });
    assert_eq!(allocated, 0);
}

#[test]
fn a_job_writes_each_window_as_its_own_item_and_reads_the_second_back() {
    let mut rig = Rig::new(Block::Rkvenc);
    let l2: Vec<u32> = (0..4).map(|i| 0x5a00_0000 | i).collect();
    let (status, back) = rig.run(
        &[reference(8, 0, 0, 0)],
        &[
            (TAG_WRITE, write(0, &regs())),
            (TAG_WRITE, write(0x4000, &l2)),
            (TAG_READ, read(0x4000, 4)),
        ],
    );
    assert_eq!(status, JobStatus::Done);
    assert_eq!(back, l2);
    // Nine items are one more write than a job may carry, and a read on its own is no job.
    let mut items: Vec<(u32, Vec<u8>)> = (0..9).map(|i| (TAG_WRITE, write(i, &[1]))).collect();
    items.push((TAG_READ, read(0, 1)));
    rig.refused("more writes than runs", &[reference(8, 0, 0, 0)], &items);
    rig.refused(
        "a write across the windows",
        &[reference(8, 0, 0, 0)],
        &[(TAG_WRITE, write(779, &[1, 2]))],
    );
}
