//! The stateless block through the job server, on the played-back node, with sealed memfds
//! standing in for the stream's dma-bufs; and the driver as a host runs it.

use std::sync::Arc;

use lemnos_contract_codec_block::work::{Builder, Work};
use lemnos_contract_codec_block::{
    BufferRef, COMPLETION_BYTES, CodecBlock, Completion, DATA_OFFSET, JOB_BYTES, Job, JobStatus,
};
use lemnos_core::{Decode, Encode, Health, Lifecycle, Reader, Writer};
use lemnos_driver_codec_jobs::testing::{Caller, PerBuffer, reference};
use lemnos_driver_codec_jobs::{Block, Buffers, POLL_NS, Server, Start};
use lemnos_hal::mock::MockStream;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::v4l2::fourcc;
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;
use lemnos_wire::ring::STATE_FAULT;

use crate::mock::{Call, Mock, MockNode};
use crate::stateless::{
    FIRST_CONTROL, M2mCodec, M2mCodecNeeds, MAX_CONTROLS, MAX_PAYLOAD_BYTES, PICTURE_BUFFERS,
    REOPENING, SETUP_BYTES, SLOT_PICTURE, SLOT_SOURCE, Settings, StatelessBlock, TAG_SETUP,
    TAG_STAMP,
};

const BUFFERS: u32 = 8;
const BYTES: u64 = 8192;
/// Two controls a decoder might take: a compound one and an integer one.
const COMPOUND: u32 = FIRST_CONTROL + 0x1000;
const INTEGER: u32 = FIRST_CONTROL + 0x1001;

type Opener = Box<dyn FnMut() -> std::io::Result<MockNode> + Send>;

fn block_on(mock: &Mock) -> StatelessBlock<MockNode, Opener> {
    let mock = mock.clone();
    StatelessBlock::new(Box::new(move || mock.open()), Settings::default())
}

fn setup(coded: &[u8; 4], width: u32, height: u32, picture: &[u8; 4], planes: u32) -> Vec<u8> {
    [fourcc(coded), width, height, fourcc(picture), planes]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect()
}

fn usual_setup() -> Vec<u8> {
    setup(b"S265", 64, 64, b"NV12", 1)
}

fn source(buffer: u32, offset: u32, length: u32) -> BufferRef {
    reference(SLOT_SOURCE, buffer, offset, length)
}

fn picture(plane: u32, buffer: u32) -> BufferRef {
    reference(SLOT_PICTURE + plane, buffer, 0, 0)
}

struct Rig {
    server: Server,
    block: StatelessBlock<MockNode, Opener>,
    stream: MockStream,
    consumer: MockStream,
    mock: Mock,
    now: u64,
}

impl Rig {
    fn with(multiplanar: bool, bytes: u64) -> Self {
        let fds = (0..BUFFERS)
            .map(|_| sealed_memfd(c"stateless-test", bytes).unwrap())
            .collect();
        let stream = MockStream::with_handles(fds);
        let mock = Mock::new(multiplanar);
        Self {
            server: Server::new(),
            block: block_on(&mock),
            consumer: stream.clone(),
            stream,
            mock,
            now: 0,
        }
    }

    fn new() -> Self {
        Self::with(true, BYTES)
    }

    /// Posts a job whose work is in buffer 0 and whose result is buffer 1, ticks, and returns
    /// how it ended with its read-back as words.
    fn run(&mut self, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) -> (JobStatus, Vec<u32>) {
        let mut bytes = vec![0u8; self.consumer.contents(0).len()];
        let mut work = Builder::new(&mut bytes).unwrap();
        for r in refs {
            work.reference(*r).unwrap();
        }
        for (tag, data) in items {
            work.item(*tag, data).unwrap();
        }
        let len = work.finish();
        self.consumer.fill(0, &bytes[..len]);
        let job = Job {
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
        let contents = self.consumer.contents(1);
        let record = Completion::decode(&mut Reader::new(&contents[..COMPLETION_BYTES])).unwrap();
        let back = contents[DATA_OFFSET..DATA_OFFSET + record.count as usize]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|four| u32::from_le_bytes(*four))
            .collect();
        assert_eq!(frame.identity, 5);
        self.consumer.return_buffer(1);
        (record.status, back)
    }

    fn usual(&mut self) -> (JobStatus, Vec<u32>) {
        self.run(
            &[source(2, 0, 100), picture(0, 3)],
            &[(TAG_SETUP, usual_setup())],
        )
    }

    fn refused(&mut self, what: &str, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) {
        let requests = self
            .mock
            .with(|d| d.calls().filter(|c| *c == Call::QueueRequest).count());
        let (status, back) = self.run(refs, items);
        assert_eq!(status, JobStatus::Refused, "{what}");
        assert!(back.is_empty(), "{what}");
        let after = self
            .mock
            .with(|d| d.calls().filter(|c| *c == Call::QueueRequest).count());
        assert_eq!(after, requests, "{what}: no request reached the device");
    }
}

#[test]
fn a_job_becomes_one_request_on_the_callers_buffers_and_brings_its_picture_back() {
    let mut rig = Rig::new();
    assert!(!rig.block.is_open(), "nothing is opened before a job");
    let items = [
        (TAG_SETUP, usual_setup()),
        (TAG_STAMP, 123_456_789u64.to_le_bytes().to_vec()),
        (COMPOUND, vec![7; 40]),
        (INTEGER, 2i32.to_le_bytes().to_vec()),
    ];
    let (status, back) = rig.run(&[source(2, 16, 100), picture(0, 3)], &items);
    assert_eq!(status, JobStatus::Done);
    // One plane, and the bytes the device said it wrote to it.
    assert_eq!(back, [4096]);
    rig.mock.with(|d| {
        // The controls went as the caller built them: a payload by its length, an integer
        // by value.
        assert_eq!(
            d.controls[..d.control_count],
            [(COMPOUND, 40), (INTEGER, 0)]
        );
        // The source: the stream buffer's dma-buf, its real size, the bytes from the offset.
        assert_eq!(d.last_queued[0], Some((0, true, BYTES as u32, 116, 16)));
        // The picture: the capture buffer with the stream buffer's number.
        assert_eq!(d.last_queued[1], Some((3, true, BYTES as u32, 0, 0)));
        assert_eq!(d.calls().filter(|c| *c == Call::QueueRequest).count(), 1);
        assert_eq!(d.opens, 1);
    });
    // The next job runs on the same session.
    assert_eq!(rig.usual().0, JobStatus::Done);
    assert_eq!(rig.mock.with(|d| d.opens), 1);
    assert_eq!(rig.block.health(), Health::Ok);
}

#[test]
fn work_of_the_wrong_shape_is_refused_before_the_device() {
    let mut rig = Rig::new();
    let good = (TAG_SETUP, usual_setup());
    let refs = [source(2, 0, 100), picture(0, 3)];
    rig.refused("no setup", &refs, &[]);
    rig.refused("two setups", &refs, &[good.clone(), good.clone()]);
    rig.refused(
        "a short setup",
        &refs,
        &[(TAG_SETUP, vec![0; SETUP_BYTES - 1])],
    );
    rig.refused(
        "no width",
        &refs,
        &[(TAG_SETUP, setup(b"S265", 0, 64, b"NV12", 1))],
    );
    rig.refused(
        "no planes",
        &refs,
        &[(TAG_SETUP, setup(b"S265", 64, 64, b"NV12", 0))],
    );
    rig.refused(
        "more planes than a format has",
        &refs,
        &[(TAG_SETUP, setup(b"S265", 64, 64, b"NV12", 9))],
    );
    rig.refused(
        "a short stamp",
        &refs,
        &[good.clone(), (TAG_STAMP, vec![0; 4])],
    );
    rig.refused(
        "two stamps",
        &refs,
        &[
            good.clone(),
            (TAG_STAMP, vec![0; 8]),
            (TAG_STAMP, vec![0; 8]),
        ],
    );
    rig.refused(
        "a tag that is nothing",
        &refs,
        &[good.clone(), (3, vec![0; 4])],
    );
    rig.refused(
        "a tag below the controls",
        &refs,
        &[good.clone(), (FIRST_CONTROL - 1, vec![0; 4])],
    );
    let mut many = vec![good.clone()];
    many.extend((0..=MAX_CONTROLS as u32).map(|i| (FIRST_CONTROL + i, vec![0; 4])));
    rig.refused("more controls than a job carries", &refs, &many);
    let mut large = Rig::with(true, 2 * BYTES);
    large.refused(
        "more payload than a job carries",
        &refs,
        &[
            good.clone(),
            (COMPOUND, vec![0; MAX_PAYLOAD_BYTES - 8]),
            (COMPOUND + 1, vec![0; 9]),
        ],
    );
    assert_eq!(
        large
            .run(
                &refs,
                &[good.clone(), (COMPOUND, vec![0; MAX_PAYLOAD_BYTES])]
            )
            .0,
        JobStatus::Done,
        "all the payload a job carries"
    );

    let items = std::slice::from_ref(&good);
    rig.refused("no source", &[picture(0, 3)], items);
    rig.refused("no picture", &[source(2, 0, 100)], items);
    rig.refused(
        "two sources",
        &[source(2, 0, 100), source(4, 0, 100), picture(0, 3)],
        items,
    );
    rig.refused(
        "a plane the format doesn't have",
        &[source(2, 0, 100), picture(0, 3), picture(1, 4)],
        items,
    );
    rig.refused(
        "a slot that is nothing",
        &[source(2, 0, 100), picture(0, 3), reference(99, 4, 0, 0)],
        items,
    );
    rig.refused(
        "the largest slot",
        &[source(2, 0, 100), reference(u32::MAX, 3, 0, 0)],
        items,
    );
    rig.refused(
        "a picture from an offset",
        &[source(2, 0, 100), reference(SLOT_PICTURE, 3, 4, 0)],
        items,
    );
    rig.refused(
        "a picture with a length",
        &[source(2, 0, 100), reference(SLOT_PICTURE, 3, 0, 4)],
        items,
    );
    assert!(!rig.block.is_open(), "none of it opened the device");
    assert_eq!(large.mock.with(|d| d.opens), 1);
}

#[test]
fn a_source_or_a_picture_past_its_buffer_is_refused() {
    let mut rig = Rig::new();
    let items = [(TAG_SETUP, usual_setup())];
    let size = BYTES as u32;
    rig.refused("an empty source", &[source(2, 0, 0), picture(0, 3)], &items);
    rig.refused(
        "a source a byte too long",
        &[source(2, 0, size + 1), picture(0, 3)],
        &items,
    );
    rig.refused(
        "a source past the end",
        &[source(2, size - 4, 5), picture(0, 3)],
        &items,
    );
    rig.refused(
        "the largest source",
        &[source(2, u32::MAX, u32::MAX), picture(0, 3)],
        &items,
    );
    // The whole buffer, and its last byte, are inside.
    assert_eq!(
        rig.run(&[source(2, 0, size), picture(0, 3)], &items).0,
        JobStatus::Done
    );
    assert_eq!(
        rig.run(&[source(2, size - 1, 1), picture(0, 3)], &items).0,
        JobStatus::Done
    );
    assert_eq!(
        rig.mock.with(|d| d.last_queued[0]),
        Some((0, true, size, size, size - 1))
    );

    // A picture buffer smaller than the plane the device writes: 4096 bytes here.
    let mut small = Rig::with(true, 4095);
    small.refused(
        "a picture buffer too small",
        &[source(2, 0, 100), picture(0, 3)],
        &items,
    );
    let mut exact = Rig::with(true, 4096);
    assert_eq!(exact.usual().0, JobStatus::Done);

    // A picture in a stream buffer past the buffers the device granted.
    let mut rig = Rig::new();
    rig.mock.with(|d| d.faults.grant = Some(3));
    rig.refused(
        "a picture past the device's buffers",
        &[source(2, 0, 100), picture(0, 3)],
        &items,
    );
    assert_eq!(
        rig.run(&[source(3, 0, 100), picture(0, 2)], &items).0,
        JobStatus::Done
    );
    const { assert!(PICTURE_BUFFERS >= BUFFERS) };
}

#[test]
fn a_device_with_a_single_planar_source_has_nowhere_to_say_an_offset() {
    let mut rig = Rig::with(false, BYTES);
    let items = [(TAG_SETUP, usual_setup())];
    rig.refused("an offset", &[source(2, 16, 100), picture(0, 3)], &items);
    assert_eq!(rig.usual().0, JobStatus::Done);
}

#[test]
fn a_device_that_keeps_other_formats_is_not_the_one_the_work_was_made_for() {
    let mut rig = Rig::new();
    rig.mock.with(|d| d.capture_fourcc = Some(fourcc(b"YU12")));
    rig.refused(
        "another picture format",
        &[source(2, 0, 100), picture(0, 3)],
        &[(TAG_SETUP, usual_setup())],
    );
    assert_eq!(
        rig.block.health(),
        Health::Ok,
        "the device did nothing wrong"
    );
}

#[test]
fn another_setup_makes_the_queues_again_and_a_consumer_that_goes_closes_the_device() {
    let mut rig = Rig::new();
    assert_eq!(rig.usual().0, JobStatus::Done);
    let other = [(TAG_SETUP, setup(b"S265", 128, 64, b"NV12", 1))];
    assert_eq!(
        rig.run(&[source(2, 0, 100), picture(0, 3)], &other).0,
        JobStatus::Done
    );
    assert_eq!(rig.mock.with(|d| d.opens), 2);
    rig.consumer.change_consumer();
    assert_eq!(
        rig.run(&[source(2, 0, 100), picture(0, 3)], &other).0,
        JobStatus::Done
    );
    assert_eq!(
        rig.mock.with(|d| d.opens),
        3,
        "the new consumer's buffers, on a new session"
    );
}

#[test]
fn a_device_that_fails_is_closed_and_opened_again_after_a_backoff() {
    let mut rig = Rig::new();
    // A picture flagged in error fails the job; the device did its part and stays open.
    rig.mock.with(|d| d.faults.frame_error = true);
    assert_eq!(rig.usual().0, JobStatus::Failed);
    assert!(rig.block.is_open());
    assert_eq!(rig.usual().0, JobStatus::Done);

    // A request that never completes: the session is closed.
    rig.mock.with(|d| d.faults.hang = true);
    assert_eq!(rig.usual().0, JobStatus::Failed);
    assert!(!rig.block.is_open());
    assert_eq!(rig.block.health(), Health::Degraded(REOPENING));
    rig.mock.with(|d| d.faults.hang = false);
    // Until the backoff has passed nothing is opened.
    let opens = rig.mock.with(|d| d.opens);
    assert_eq!(rig.usual().0, JobStatus::Failed);
    assert_eq!(rig.mock.with(|d| d.opens), opens);
    rig.now += 250_000_000;
    assert_eq!(rig.usual().0, JobStatus::Done);
    assert_eq!(rig.block.health(), Health::Ok);

    // A node that isn't there fails every job that tries it, with the wait doubling, and the
    // first job after it is back and the wait has passed runs.
    rig.consumer.change_consumer();
    rig.mock.with(|d| d.faults.open = true);
    for _ in 0..8 {
        assert_eq!(rig.usual().0, JobStatus::Failed);
        rig.now += 8_000_000_000;
    }
    assert_eq!(rig.block.health(), Health::Degraded(REOPENING));
    rig.mock.with(|d| d.faults.open = false);
    assert_eq!(rig.usual().0, JobStatus::Done, "it never gives up");
}

#[test]
fn decoding_allocates_nothing_once_the_session_is_open() {
    let mut rig = Rig::new();
    assert_eq!(rig.usual().0, JobStatus::Done);
    let mut bytes = vec![0u8; 1024];
    let mut work = Builder::new(&mut bytes).unwrap();
    work.reference(source(2, 0, 100)).unwrap();
    work.reference(picture(0, 3)).unwrap();
    work.item(TAG_SETUP, &usual_setup()).unwrap();
    work.item(COMPOUND, &[1; 64]).unwrap();
    work.item(INTEGER, &[0; 4]).unwrap();
    let len = work.finish();
    let work = Work::parse(&bytes[..len]).unwrap();
    let mut back = [0u8; 64];
    let block = &mut rig.block;
    let buffers = Buffers::naming(&rig.stream, 0b1100);
    let ((), allocated) = allocations(|| {
        for now in 0..100 {
            assert_eq!(block.start(&work, &buffers, &mut back, now), Start::Done(4));
        }
    });
    assert_eq!(allocated, 0);
}

/// A board with `driver` on a job stream of dma-bufs.
fn board(driver: &str, memory: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"decoder\"\ndriver = \"{driver}\"\n\
         config = {{ device = \"/nonexistent/video-decoder\" }}\n\
         [devices.streams.jobs]\n{memory}buffers = 8\nbuffer_bytes = 8192\n\
         job_slots = 8\njob_bytes = 20\n"
    )
}

mod nothing {
    use lemnos_core::{Config, Lifecycle, Result, driver};

    /// A driver that leaves its device's stream to the test.
    #[driver("nothing", provides = [])]
    pub struct Nothing {}

    impl Lifecycle for Nothing {
        type Needs = NothingNeeds;

        fn start(_: NothingNeeds, _: &Config<'_>) -> Result<Self> {
            Ok(Self {})
        }
    }
}

fn host(text: &str) -> Host {
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(text).unwrap(),
        &[entry::<M2mCodec>(), entry::<nothing::Nothing>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        Arc::new(PerBuffer),
    )
    .unwrap()
}

#[test]
fn a_board_file_binds_the_driver_and_a_plain_stream_or_no_device_is_refused() {
    let dma = "memory = \"dma-buf\"\n";
    let up = host(&board("v4l2-stateless", dma));
    up.tick();
    let device = up.device("decoder").unwrap();
    assert_eq!(
        (device.state, device.health),
        (lemnos_host::DeviceState::Running, Health::Ok)
    );
    assert_eq!(up.manifest(0).unwrap().provides.len(), 1);
    for text in [
        board("v4l2-stateless", ""),
        board("v4l2-stateless", dma).replace("device = \"/nonexistent/video-decoder\"", ""),
        board("v4l2-stateless", dma).replace(
            "device = \"/nonexistent/video-decoder\"",
            "device = \"/x\", timeout_ms = 0",
        ),
    ] {
        let host = host(&text);
        host.tick();
        assert_ne!(
            host.device("decoder").unwrap().state,
            lemnos_host::DeviceState::Running,
            "{text}"
        );
    }
}

#[test]
fn a_caller_decodes_through_a_real_host_on_the_streams_own_buffers() {
    let host = host(&board("nothing", "memory = \"dma-buf\"\n"));
    let mock = Mock::new(true);
    let jobs = Box::new(host.stream_producer(0, "jobs").unwrap());
    let mut driver = M2mCodec::on(M2mCodecNeeds { jobs }, Box::new(block_on(&mock)));
    let mut name = [0u8; 48];
    let len = CodecBlock::block(&mut driver, &mut name).unwrap();
    assert_eq!(&name[..len], b"v4l2-stateless");

    let mut caller = Caller::attach(&host, 0, "jobs", 1);
    let mut now = driver.tick(1).unwrap();
    // The bitstream goes in buffer 2, the picture comes in buffer 3, the work is in buffer 0
    // and the result in buffer 1.
    caller.fill(2, 0, &[0xb5; 100]);
    let setup = usual_setup();
    let stamp = 40_000_000u64.to_le_bytes();
    let items: [(u32, &[u8]); 3] = [
        (TAG_SETUP, &setup),
        (TAG_STAMP, &stamp),
        (COMPOUND, &[9; 24]),
    ];
    caller.post(7, 0, 1, &[source(2, 0, 100), picture(0, 3)], &items);
    now = driver.tick(now).unwrap();
    let (identity, record, back) = caller.completion().unwrap();
    assert_eq!((identity, record.status), (7, JobStatus::Done));
    assert_eq!(back, 4096u32.to_le_bytes());
    mock.with(|d| {
        assert_eq!(d.last_queued[0], Some((0, true, 8192, 100, 0)));
        assert_eq!(d.last_queued[1], Some((3, true, 8192, 0, 0)));
        assert_eq!(d.controls[..d.control_count], [(COMPOUND, 24)]);
    });

    // A source past its buffer is refused, and bytes that are no job cost the caller its
    // stream and the device its session.
    caller.post(8, 0, 1, &[source(2, 8000, 193), picture(0, 3)], &items);
    now = driver.tick(now).unwrap();
    assert_eq!(caller.completion().unwrap().1.status, JobStatus::Refused);
    caller.post_bytes(&[0; 3]);
    now = driver.tick(now).unwrap();
    assert_eq!(caller.state(), STATE_FAULT);

    let mut next = Caller::attach(&host, 0, "jobs", 2);
    now = driver.tick(now).unwrap();
    next.post(9, 0, 1, &[source(2, 0, 50), picture(0, 4)], &items);
    driver.tick(now).unwrap();
    assert_eq!(next.completion().unwrap().1.status, JobStatus::Done);
    assert_eq!(mock.with(|d| d.opens), 2, "a session of its own");
    let counters = CodecBlock::counters(&mut driver).unwrap();
    assert_eq!(
        (counters.jobs, counters.refused, counters.failed),
        (2, 2, 0)
    );
    assert_eq!(Lifecycle::health(&driver), Health::Ok);
}
