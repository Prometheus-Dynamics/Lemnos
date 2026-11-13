//! The block as `codec.block/1`, through a real host's job stream of sealed memfds, on a node
//! that plays the `mvx` driver back. No device node is opened.

use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;
use std::time::Duration;

use lemnos_contract_codec_block::{BufferRef, CodecBlock, JobStatus, MAX_BLOCK_NAME};
use lemnos_core::{Bound, Caller as CoreCaller, Health, Lifecycle, MethodId};
use lemnos_driver_codec_jobs::testing::{Caller, PerBuffer, reference};
use lemnos_driver_m2m_decode::Event;
use lemnos_host::board::parse;
use lemnos_host::stream::MemfdAllocator;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_linux_sys::v4l2::{BUF_FLAG_LAST, EVENT_SOURCE_CHANGE, SRC_CH_RESOLUTION};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::{SimBoard, SimClock};

use crate::codec::{
    CLOSED, NOT_THERE, TAG_ADOPT, TAG_CONTROL, TAG_DESTINATION, TAG_DRAIN, TAG_EVENTS, TAG_PRIVATE,
    TAG_SETUP, TAG_SOURCE, TAG_STOP,
};
use crate::tests::{H264, Mock, MockDevice, NV12M};
use crate::uapi::{self, control};
use crate::{K230Vpu, K230VpuNeeds, VpuBlock};

/// Each stream buffer.
const BYTES: u32 = 8192;
/// The buffers a job that completes at once is written to and completes on.
const WORK: u32 = 14;
const RESULT: u32 = 15;
/// Jobs in flight at once are written to and complete on buffers 8 to 15, a pair each.
const PAIRS: u32 = 4;

fn board(driver: &str, dma_buf: bool) -> String {
    let memory = if dma_buf {
        "memory = \"dma-buf\"\n"
    } else {
        ""
    };
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"vpu\"\ndriver = \"{driver}\"\n\
         config = {{ device = \"/nonexistent/video-codec\" }}\n\
         [devices.streams.jobs]\n{memory}buffers = 16\nbuffer_bytes = {BYTES}\n\
         job_slots = 16\njob_bytes = 24\n"
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

struct Rig {
    host: Host,
    clock: SimClock,
}

impl Rig {
    fn start(text: &str, dma_buf: bool) -> Self {
        Self::try_start(text, dma_buf).unwrap()
    }

    /// The host started on `text`, or the reason it refused the board file.
    fn try_start(text: &str, dma_buf: bool) -> Result<Self, String> {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let allocator: Arc<dyn lemnos_host::stream::StreamAllocator> = if dma_buf {
            Arc::new(PerBuffer)
        } else {
            Arc::new(MemfdAllocator)
        };
        let host = Host::start_with_streams(
            &parse(text).unwrap(),
            &[entry::<K230Vpu>(), entry::<nothing::Nothing>()],
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
            allocator,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self { host, clock })
    }

    fn ticks(&self, count: u32) {
        for _ in 0..count {
            self.clock.advance(Duration::from_millis(1));
            self.host.tick();
            self.host.run_ticks();
        }
    }
}

/// The contract's calls, carried by the host to device 0.
struct Through<'a> {
    host: &'a Host,
    args: [u8; 64],
}

impl CoreCaller for Through<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(
        &mut self,
        method: u16,
        args_len: usize,
        reply: &mut [u8],
    ) -> lemnos_core::Result<usize> {
        let id = MethodId {
            contract: 0,
            method,
        };
        self.host.call(0, id, &self.args[..args_len], reply)
    }
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn le(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|four| u32::from_le_bytes(*four))
        .collect()
}

/// A decoder: H.264 in 4096-byte source planes, NV12 in two planes of 2048 and 1024 out.
fn setup(destinations: u32) -> Vec<u8> {
    words(&[
        0,
        H264,
        64,
        32,
        1,
        4096,
        0,
        0,
        NV12M,
        64,
        32,
        2,
        2048,
        1024,
        0,
        2,
        destinations,
    ])
}

/// The driver on the played node, on the stream of a device whose own driver doesn't use it:
/// the test plays the host's part in ticking it.
fn driver_on(rig: &Rig, mock: &Mock) -> K230Vpu {
    let jobs = Box::new(rig.host.stream_producer(0, "jobs").unwrap());
    K230Vpu::on(
        K230VpuNeeds {
            jobs: lemnos_core::Held::boxed(jobs),
        },
        Box::new(VpuBlock::new(MockDevice {
            mock: mock.clone(),
            opens: 0,
            fail_opens: 0,
            size: None,
        })),
    )
}

struct Session {
    _rig: Rig,
    driver: K230Vpu,
    caller: Caller,
    mock: Mock,
    now: u64,
    /// The job using each pair of buffers, until its completion is taken.
    pairs: [Option<u64>; PAIRS as usize],
}

impl Session {
    fn new(destinations: u32) -> Self {
        let rig = Rig::start(&board("nothing", true), true);
        let mock = Mock::default();
        let mut driver = driver_on(&rig, &mock);
        let caller = Caller::attach(&rig.host, 0, "jobs", 1);
        let now = driver.tick(1).unwrap();
        let mut s = Self {
            _rig: rig,
            driver,
            caller,
            mock,
            now,
            pairs: [None; PAIRS as usize],
        };
        let (status, _) = s.run(1, &[], &[(TAG_SETUP, &setup(destinations))]);
        assert_eq!(status, JobStatus::Done);
        s
    }

    fn post(&mut self, identity: u64, refs: &[BufferRef], items: &[(u32, &[u8])]) {
        let free = self
            .pairs
            .iter()
            .position(Option::is_none)
            .expect("a free pair");
        self.pairs[free] = Some(identity);
        let pair = 8 + 2 * free as u32;
        self.caller.post(identity, pair, pair + 1, refs, items);
    }

    fn tick(&mut self) {
        self.now = self.driver.tick(self.now).unwrap();
    }

    /// The next completion, if one is there after a tick.
    fn completion(&mut self) -> Option<(u64, JobStatus, Vec<u8>)> {
        self.tick();
        self.take()
    }

    /// The next completion already published.
    fn take(&mut self) -> Option<(u64, JobStatus, Vec<u8>)> {
        let (identity, record, back) = self.caller.completion()?;
        for pair in &mut self.pairs {
            if *pair == Some(identity) {
                *pair = None;
            }
        }
        Some((identity, record.status, back))
    }

    /// Posts a job that completes at once, and its outcome.
    fn run(
        &mut self,
        identity: u64,
        refs: &[BufferRef],
        items: &[(u32, &[u8])],
    ) -> (JobStatus, Vec<u8>) {
        self.post(identity, refs, items);
        let (id, status, back) = self.completion().expect("a completion");
        assert_eq!(id, identity);
        (status, back)
    }
}

fn source(buffer: u32, bytes: u32, stamp: u64) -> (Vec<BufferRef>, Vec<u8>) {
    (
        vec![reference(0, buffer, 0, bytes)],
        stamp.to_le_bytes().to_vec(),
    )
}

fn destination(luma: u32, chroma: u32) -> Vec<BufferRef> {
    vec![reference(0, luma, 0, 0), reference(1, chroma, 0, 0)]
}

#[test]
fn a_board_file_binds_it_and_what_it_names_is_checked() {
    // The node isn't there: the device binds (nothing is opened at bind), says what it is,
    // and a setup fails with the reason in its health.
    let rig = Rig::start(&board("k230-vpu", true), true);
    rig.ticks(1);
    assert_eq!(rig.host.device("vpu").unwrap().health, Health::Ok);
    let mut device: Bound<dyn CodecBlock, _> = Bound::new(Through {
        host: &rig.host,
        args: [0; 64],
    });
    let mut name = [0u8; MAX_BLOCK_NAME];
    let len = device.block(&mut name).unwrap();
    assert_eq!(&name[..len], b"mvx");
    let len = device.job_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"jobs");
    let info = device.info().unwrap();
    assert_eq!((info.max_refs, info.max_buffers), (3, 32));
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    rig.ticks(1);
    caller.post(1, WORK, RESULT, &[], &[(TAG_SETUP, &setup(2))]);
    rig.ticks(1);
    let (identity, record, _) = caller.completion().unwrap();
    assert_eq!((identity, record.status), (1, JobStatus::Failed));
    assert_eq!(
        rig.host.device("vpu").unwrap().health,
        Health::Degraded(NOT_THERE)
    );

    // A stream of plain memory: the bind is refused.
    let rig = Rig::start(&board("k230-vpu", false), false);
    rig.ticks(1);
    assert_ne!(rig.host.device("vpu").unwrap().state, DeviceState::Running);

    // No device: the board starts, and the device is listed failed by name, with the key and the
    // reason. The device is never made.
    let text = board("k230-vpu", true).replace("device = \"/nonexistent/video-codec\"", "");
    let rig = Rig::try_start(&text, true).unwrap();
    let state = rig.host.device("vpu").unwrap().state;
    assert!(
        matches!(&state, DeviceState::Backoff { reason, .. } if reason.contains("setting \"device\" is required")),
        "{state:?}"
    );
}

#[test]
fn sources_and_destinations_complete_on_the_blocks_schedule_with_the_streams_buffers() {
    let mut s = Session::new(2);
    // The setup read back what the kernel kept, and the slots it granted.
    let (_, back) = s.run(2, &[], &[(TAG_SETUP, &setup(2))]);
    let kept = le(&back);
    assert_eq!(kept.len(), 22);
    assert_eq!(&kept[..5], &[H264, 64, 32, 1, 4096]);
    assert_eq!(&kept[10..15], &[NV12M, 64, 32, 2, 2048]);
    assert_eq!(&kept[20..], &[2, 2]);

    // Two destinations, then a source: all three pending.
    s.post(10, &destination(1, 2), &[(TAG_DESTINATION, &[])]);
    s.post(11, &destination(3, 4), &[(TAG_DESTINATION, &[])]);
    let (refs, stamp) = source(0, 100, 5_000);
    s.post(12, &refs, &[(TAG_SOURCE, &stamp)]);
    let early = s.completion();
    assert!(early.is_none(), "{early:?}");

    // What the kernel was handed: the driver's own descriptors of the stream's buffers, at
    // their real size, the payload, no data offset.
    let inode = |fd: i32| {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(format!("/proc/self/fd/{fd}"))
            .unwrap()
            .ino()
    };
    s.mock.with(|d| {
        let q = d.queued[0][0];
        let (fd, length, used, offset) = q.planes[0];
        assert_eq!((length, used, offset), (BYTES, 100, 0));
        let theirs = s.caller.handle(0).as_fd().as_raw_fd();
        assert_ne!(fd, theirs);
        assert_eq!(inode(fd), inode(theirs));
        assert_eq!(d.queued[1].len(), 2);
        assert_eq!(d.queued[1][0].planes[0].1, BYTES);
    });

    // The block fills the first destination before it gives the source back: the jobs
    // complete in that order, not the order they were posted.
    s.mock.finish(1, BUF_FLAG_LAST, [2000, 1000, 0]);
    let (identity, status, back) = s.completion().expect("a destination");
    assert_eq!((identity, status), (10, JobStatus::Done));
    let back = le(&back);
    assert_eq!(back[4..], [2, 2000, 1000, 0]);
    assert_eq!(back[0], BUF_FLAG_LAST);
    s.mock.finish(0, 0, [0; 3]);
    let (identity, status, back) = s.completion().expect("the source");
    assert_eq!((identity, status, back.len()), (12, JobStatus::Done, 16));

    // A byte count the kernel reports past the buffer comes back flagged, with no bytes.
    s.mock.finish(1, 0, [BYTES + 1, 0, 0]);
    let (identity, _, back) = s.completion().expect("a destination");
    assert_eq!(identity, 11);
    let back = le(&back);
    assert_ne!(back[0] & lemnos_linux_sys::v4l2::BUF_FLAG_ERROR, 0);
    assert_eq!(back[5..], [0, 0, 0]);

    // A drain is done at once.
    assert_eq!(s.run(13, &[], &[(TAG_DRAIN, &[])]).0, JobStatus::Done);
    s.mock.with(|d| assert_eq!(d.stops[0], 1));
}

#[test]
fn work_that_is_no_job_of_the_session_never_reaches_the_kernel() {
    let mut s = Session::new(2);
    let (refs, stamp) = source(0, 100, 0);
    let mut twice = destination(1, 2);
    twice[1].slot = 0;
    let mut third = destination(1, 2);
    third.push(reference(2, 3, 0, 0));
    let mut gap = destination(1, 2);
    gap[1].slot = 2;
    let mut offset = refs.clone();
    offset[0].offset = 4;
    let mut empty = refs.clone();
    empty[0].length = 0;
    let mut long = refs.clone();
    long[0].length = BYTES + 1;
    let mut with_length = destination(1, 2);
    with_length[0].length = 1;
    let mut unheld = refs.clone();
    unheld[0].buffer = 40;
    let control = words(&[control::SECURE_VIDEO, 1]);
    let colour = words(&[uapi::VIDIOC_S_MVX_DSL_MODE, 0]);
    let mut short_private = words(&[uapi::VIDIOC_S_MVX_RATE_CONTROL]);
    short_private.extend([0u8; 3]);
    type Case<'a> = (&'a str, Vec<BufferRef>, Vec<(u32, Vec<u8>)>);
    let cases: Vec<Case<'_>> = vec![
        ("nothing", vec![], vec![]),
        ("another tag", vec![], vec![(10, vec![])]),
        (
            "two kinds",
            refs.clone(),
            vec![(TAG_SOURCE, stamp.clone()), (TAG_DRAIN, vec![])],
        ),
        (
            "a short stamp",
            refs.clone(),
            vec![(TAG_SOURCE, stamp[..4].to_vec())],
        ),
        (
            "a drain with references",
            refs.clone(),
            vec![(TAG_DRAIN, vec![])],
        ),
        (
            "a source without planes",
            vec![],
            vec![(TAG_SOURCE, stamp.clone())],
        ),
        (
            "a plane named twice",
            twice,
            vec![(TAG_DESTINATION, vec![])],
        ),
        (
            "a plane past the format",
            third,
            vec![(TAG_DESTINATION, vec![])],
        ),
        ("a gap in the planes", gap, vec![(TAG_DESTINATION, vec![])]),
        ("an offset", offset, vec![(TAG_SOURCE, stamp.clone())]),
        ("an empty payload", empty, vec![(TAG_SOURCE, stamp.clone())]),
        (
            "a payload past the buffer",
            long,
            vec![(TAG_SOURCE, stamp.clone())],
        ),
        (
            "a destination with a length",
            with_length,
            vec![(TAG_DESTINATION, vec![])],
        ),
        (
            "a buffer past the claim's",
            unheld,
            vec![(TAG_SOURCE, stamp.clone())],
        ),
        (
            "a control with a source",
            refs.clone(),
            vec![(TAG_SOURCE, stamp.clone()), (TAG_CONTROL, words(&[1, 1]))],
        ),
        (
            "a refused control",
            vec![],
            vec![(TAG_CONTROL, control.clone())],
        ),
        (
            "an unsound private request",
            vec![],
            vec![(TAG_PRIVATE, colour)],
        ),
        (
            "a private request of the wrong size",
            vec![],
            vec![(TAG_PRIVATE, short_private)],
        ),
        (
            "an adopt of no buffers",
            vec![],
            vec![(TAG_ADOPT, words(&[0]))],
        ),
        (
            "a setup with a refused control",
            vec![],
            vec![(TAG_SETUP, setup(2)), (TAG_CONTROL, control)],
        ),
        (
            "a setup of no planes",
            vec![],
            vec![(TAG_SETUP, {
                let mut b = setup(2);
                b[16..20].fill(0);
                b
            })],
        ),
        (
            "a setup that is no direction",
            vec![],
            vec![(TAG_SETUP, {
                let mut b = setup(2);
                b[0] = 2;
                b
            })],
        ),
    ];
    for (identity, (what, refs, items)) in cases.iter().enumerate() {
        let items: Vec<(u32, &[u8])> = items.iter().map(|(t, b)| (*t, b.as_slice())).collect();
        let (status, _) = s.run(100 + identity as u64, refs, &items);
        assert_eq!(status, JobStatus::Refused, "{what}");
    }
    s.mock.with(|d| {
        assert!(d.queued[0].is_empty() && d.queued[1].is_empty());
        assert!(d.controls.is_empty());
    });
    // The session is the first setup's, still open.
    assert_eq!(Lifecycle::health(&s.driver), Health::Ok);

    // A buffer the block holds can't be handed to it again.
    s.post(1, &destination(1, 2), &[(TAG_DESTINATION, &[])]);
    assert!(s.completion().is_none());
    let (status, _) = s.run(2, &destination(1, 3), &[(TAG_DESTINATION, &[])]);
    assert_eq!(status, JobStatus::Refused);
    // A plane smaller than the format's: a picture plane of 2048 bytes in a 1024-byte setup.
    let mut small = setup(2);
    small[48..52].copy_from_slice(&(BYTES + 1).to_le_bytes());
    assert_eq!(s.run(3, &[], &[(TAG_SETUP, &small)]).0, JobStatus::Done);
    s.post(4, &destination(5, 6), &[(TAG_DESTINATION, &[])]);
    // The first destination went with the first session, and is reported first.
    assert_eq!(
        s.completion().map(|c| (c.0, c.1)),
        Some((1, JobStatus::Failed))
    );
    assert_eq!(s.take().map(|c| (c.0, c.1)), Some((4, JobStatus::Refused)));
}

#[test]
fn before_a_setup_there_is_no_session_to_run_on() {
    let rig = Rig::start(&board("nothing", true), true);
    let mock = Mock::default();
    let mut driver = driver_on(&rig, &mock);
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    let mut now = driver.tick(1).unwrap();
    let (refs, stamp) = source(0, 1, 0);
    for (refs, item) in [
        (refs, (TAG_SOURCE, stamp.as_slice())),
        (vec![], (TAG_DRAIN, &[][..])),
        (vec![], (TAG_EVENTS, &[][..])),
    ] {
        caller.post(1, WORK, RESULT, &refs, &[item]);
        now = driver.tick(now).unwrap();
        assert_eq!(caller.completion().unwrap().1.status, JobStatus::Refused);
    }
    mock.with(|d| assert!(d.formats.iter().all(Option::is_none)));
}

#[test]
fn controls_and_the_sound_private_requests_go_to_the_session() {
    let mut s = Session::new(2);
    let (status, back) = s.run(
        2,
        &[],
        &[
            (TAG_CONTROL, &words(&[0x0099_0a01, 7])),
            (TAG_CONTROL, &words(&[0x0099_0a02, (-3i32).cast_unsigned()])),
        ],
    );
    assert_eq!(
        (status, le(&back)),
        (JobStatus::Done, vec![7, (-3i32).cast_unsigned()])
    );
    s.mock.with(|d| {
        assert_eq!(d.controls, [(0x0099_0a01, 7), (0x0099_0a02, -3)]);
        d.bad_control = Some(0x0099_0a03);
    });
    let (status, _) = s.run(3, &[], &[(TAG_CONTROL, &words(&[0x0099_0a03, 1]))]);
    assert_eq!(status, JobStatus::Failed);

    // An allowed request, at its size, reaches the call: the played node's descriptor is a
    // pipe, which answers that it has no such request.
    let mut rate = words(&[uapi::VIDIOC_S_MVX_RATE_CONTROL]);
    rate.resize(4 + size_of::<uapi::RateControl>(), 0);
    assert_eq!(s.run(4, &[], &[(TAG_PRIVATE, &rate)]).0, JobStatus::Failed);
    // Every request issued fits the read-back.
    for (request, size) in [
        (uapi::VIDIOC_G_MVX_COLORDESC, size_of::<uapi::ColorDesc>()),
        (
            uapi::VIDIOC_S_MVX_ROI_REGIONS,
            size_of::<uapi::RoiRegions>(),
        ),
        (uapi::VIDIOC_S_MVX_QP_EPR, size_of::<uapi::ParamQp>()),
        (
            uapi::VIDIOC_S_MVX_RATE_CONTROL,
            size_of::<uapi::RateControl>(),
        ),
        (
            uapi::VIDIOC_S_MVX_LONG_TERM_REF,
            size_of::<uapi::LongTermRef>(),
        ),
        (uapi::VIDIOC_S_MVX_STATS_MODE, size_of::<uapi::EncStats>()),
        (uapi::VIDIOC_S_MVX_HUFF_TABLE, size_of::<uapi::HuffTable>()),
        (
            uapi::VIDIOC_S_MVX_COLOR_CONV_COEF,
            size_of::<uapi::ColorConvCoef>(),
        ),
        (
            uapi::VIDIOC_S_MVX_RGB2YUV_COLOR_CONV_COEF,
            size_of::<uapi::Rgb2YuvColorConvCoef>(),
        ),
    ] {
        assert!(lemnos_driver_k230_vpu_sys::takes(request, &vec![0; size]));
        assert!(size <= crate::codec::MAX_PRIVATE_BYTES, "{request:#x}");
    }
}

#[test]
fn a_decoder_learns_the_streams_format_and_adopts_destinations_for_it() {
    let mut s = Session::new(0);
    // No destinations yet: one is refused.
    let (status, _) = s.run(2, &destination(1, 2), &[(TAG_DESTINATION, &[])]);
    assert_eq!(status, JobStatus::Refused);
    let (refs, stamp) = source(0, 10, 0);
    s.post(3, &refs, &[(TAG_SOURCE, &stamp)]);
    assert!(s.completion().is_none());
    s.mock.with(|d| {
        d.events.push_back(Event {
            event_type: EVENT_SOURCE_CHANGE,
            id: 0,
            pending: 0,
            sequence: 0,
            changes: SRC_CH_RESOLUTION,
            timestamp: Duration::ZERO,
        });
    });
    let (status, back) = s.run(4, &[], &[(TAG_EVENTS, &[])]);
    assert_eq!(status, JobStatus::Done);
    let back = le(&back);
    assert_eq!((back[0], back[1], back[11]), (SRC_CH_RESOLUTION, NV12M, 0));
    let (status, back) = s.run(5, &[], &[(TAG_ADOPT, &words(&[3]))]);
    assert_eq!((status, le(&back)[11]), (JobStatus::Done, 3));
    s.post(6, &destination(1, 2), &[(TAG_DESTINATION, &[])]);
    assert!(s.completion().is_none());
    s.mock.finish(1, 0, [10, 5, 0]);
    let (identity, status, _) = s.completion().unwrap();
    assert_eq!((identity, status), (6, JobStatus::Done));

    // A stop gives back what the block held, as failed.
    let (refs, stamp) = source(7, 10, 0);
    s.post(7, &refs, &[(TAG_SOURCE, &stamp)]);
    assert!(s.completion().is_none());
    s.post(8, &[], &[(TAG_STOP, &[])]);
    let mut seen = Vec::new();
    while let Some((identity, status, _)) = s.completion() {
        seen.push((identity, status));
    }
    seen.sort_unstable_by_key(|(identity, _)| *identity);
    assert_eq!(
        seen,
        [
            (3, JobStatus::Failed),
            (7, JobStatus::Failed),
            (8, JobStatus::Done)
        ]
    );
}

#[test]
fn a_session_that_fails_is_closed_and_its_jobs_fail_and_a_consumer_that_goes_closes_it() {
    let mut s = Session::new(2);
    let (refs, stamp) = source(0, 10, 0);
    s.post(2, &refs, &[(TAG_SOURCE, &stamp)]);
    assert!(s.completion().is_none());
    s.mock.with(|d| d.refuse_dequeue = true);
    let (identity, status, _) = s.completion().expect("failed");
    assert_eq!((identity, status), (2, JobStatus::Failed));
    assert_eq!(Lifecycle::health(&s.driver), Health::Degraded(CLOSED));
    // The node was let go: only the test and the block's device hold the played driver now.
    assert_eq!(Arc::strong_count(&s.mock.0), 2);
    let (status, _) = s.run(3, &refs, &[(TAG_SOURCE, &stamp)]);
    assert_eq!(status, JobStatus::Refused);

    // Set up again; then the consumer goes, and the session with it.
    s.mock.with(|d| d.refuse_dequeue = false);
    assert_eq!(s.run(4, &[], &[(TAG_SETUP, &setup(2))]).0, JobStatus::Done);
    assert_eq!(Lifecycle::health(&s.driver), Health::Ok);
    s.post(5, &refs, &[(TAG_SOURCE, &stamp)]);
    s.tick();
    assert_eq!(Arc::strong_count(&s.mock.0), 3);
    s.caller.post_bytes(&[1, 2, 3]);
    s.tick();
    assert_eq!(Arc::strong_count(&s.mock.0), 2);
}

#[test]
fn serving_sources_and_destinations_allocates_nothing_once_set_up() {
    let mut s = Session::new(2);
    let (refs, stamp) = source(0, 100, 0);
    let dest = destination(1, 2);
    // Warm up: the first job of each kind takes its buffers into the claim.
    s.post(2, &dest, &[(TAG_DESTINATION, &[])]);
    s.post(3, &refs, &[(TAG_SOURCE, &stamp)]);
    s.tick();
    s.mock.finish(0, 0, [0; 3]);
    s.mock.finish(1, 0, [1, 1, 0]);
    assert!(s.completion().is_some() && s.take().is_some());
    let mut allocated = 0;
    for identity in 0..40 {
        s.post(10 + 2 * identity, &dest, &[(TAG_DESTINATION, &[])]);
        s.post(11 + 2 * identity, &refs, &[(TAG_SOURCE, &stamp)]);
        let ((), n) = allocations(|| s.tick());
        allocated += n;
        s.mock.finish(0, 0, [0; 3]);
        s.mock.finish(1, 0, [1, 1, 0]);
        let ((), n) = allocations(|| s.tick());
        allocated += n;
        assert_eq!(s.take().unwrap().1, JobStatus::Done);
        assert_eq!(s.take().unwrap().1, JobStatus::Done);
    }
    assert_eq!(allocated, 0, "serving jobs");
}
