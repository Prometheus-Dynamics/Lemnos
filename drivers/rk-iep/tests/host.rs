//! The driver as a host runs it: bound from a board file, its contract called through the
//! host, its jobs posted by a caller on the host's own stream and completed there. Sealed
//! memfds stand in for the stream's dma-bufs and a played kernel, which does what
//! `mpp_iep2.c` does with each message, for the service node. No device node is opened.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_contract_codec_block::{BufferRef, CodecBlock, JobStatus, MAX_BLOCK_NAME};
use lemnos_core::{Bound, Caller as CoreCaller, Health, Lifecycle, MethodId, Result};
use lemnos_driver_codec_jobs::testing::{Caller, PerBuffer, reference};
use lemnos_driver_rk_iep::{
    CONFIG_BYTES, MAX_WORK_BYTES, RESULTS_BYTES, RkIep, RkIepNeeds, SLOTS, Slot, TAG_CONFIG,
    TAG_TUNING, TUNING_BYTES, Tuning,
};
use lemnos_driver_rk_mpp::codec::NOT_THERE;
use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, BoxedDevice, Device, Message, Session};
use lemnos_host::board::parse;
use lemnos_host::stream::HeapAllocator;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_linux_sys::dma_buf;
use lemnos_sim::{SimBoard, SimClock};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// The frames: 64 by 16, NV12, 64-byte strides.
const W: u32 = 64;
const H: u32 = 16;
const LUMA: u32 = W * H;
const FRAME: u32 = LUMA * 3 / 2;
/// Each stream buffer.
const BYTES: u32 = 4096;
/// The stream buffers jobs are written to and complete on.
const WORK: u32 = 10;
const RESULT: u32 = 11;

/// The played kernel, shared with the test.
#[derive(Clone)]
struct Node(Arc<Mutex<Kernel>>);

struct Played(Arc<Mutex<Kernel>>);

impl Session for Played {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.0.lock().unwrap().call(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        let n = into.len();
        into.copy_from_slice(&self.0.lock().unwrap().areas[slot][..n]);
    }
}

impl Device for Node {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        let mut kernel = self.0.lock().unwrap();
        if let Some(errno) = kernel.fail_open.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        kernel.opens += 1;
        kernel.client = None;
        Ok(Played(Arc::clone(&self.0)))
    }

    fn confined(&mut self, _block: Block) -> io::Result<bool> {
        Ok(self.0.lock().unwrap().confined)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

/// A board with the driver named `driver` on a job stream, of dma-bufs when `dma_buf`.
fn board(driver: &str, dma_buf: bool) -> String {
    let memory = if dma_buf {
        "memory = \"dma-buf\"\n"
    } else {
        ""
    };
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"iep\"\ndriver = \"{driver}\"\n\
         config = {{ node = \"/nonexistent/mpp_service\" }}\n\
         [devices.streams.jobs]\n{memory}buffers = 12\nbuffer_bytes = {BYTES}\n\
         job_slots = 8\njob_bytes = 24\n"
    )
}

struct Rig {
    host: Host,
    clock: SimClock,
}

impl Rig {
    fn start(text: &str, dma_buf: bool) -> Self {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let board = parse(text).unwrap();
        let drivers = [entry::<RkIep>(), entry::<nothing::Nothing>()];
        let allocator: Arc<dyn lemnos_host::stream::StreamAllocator> = if dma_buf {
            Arc::new(PerBuffer)
        } else {
            Arc::new(HeapAllocator)
        };
        let host = Host::start_with_streams(
            &board,
            &drivers,
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
            allocator,
        )
        .unwrap();
        Self { host, clock }
    }

    fn ticks(&self, count: u32) {
        for _ in 0..count {
            self.clock.advance(Duration::from_millis(1));
            self.host.tick();
            self.host.run_ticks();
        }
    }
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

/// The contract's calls, carried by the host to device 0.
struct Through<'a> {
    host: &'a Host,
    args: [u8; 64],
}

impl CoreCaller for Through<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let id = MethodId {
            contract: 0,
            method,
        };
        self.host.call(0, id, &self.args[..args_len], reply)
    }
}

/// A config item: 64 by 16 NV12 in and out (Cr first out), mode `I5O2`, bottom field first.
fn config(words: [u32; 12]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

const USUAL: [u32; 12] = [W, H, 3, 0, W, W, 3, 1, W, 1, 1, 0];

/// The usual references: the five frames in buffers 0 to 4, the scratch in 5 and 6.
fn refs() -> Vec<BufferRef> {
    let frame = |y: Slot, c: Slot, buffer: u32| {
        [
            reference(slot(y), buffer, 0, 0),
            reference(slot(c), buffer, LUMA, 0),
        ]
    };
    [
        frame(Slot::CurY, Slot::CurC, 0),
        frame(Slot::NextY, Slot::NextC, 1),
        frame(Slot::PrevY, Slot::PrevC, 2),
        frame(Slot::TopY, Slot::TopC, 3),
        frame(Slot::BottomY, Slot::BottomC, 4),
    ]
    .concat()
    .into_iter()
    .chain([
        reference(slot(Slot::Md), 5, 0, 0),
        reference(slot(Slot::Mv), 6, 0, 0),
    ])
    .collect()
}

fn slot(slot: Slot) -> u32 {
    Slot::ALL.iter().position(|&s| s == slot).unwrap() as u32
}

/// The driver on the played kernel, on the stream of a device whose own driver doesn't use
/// it: the test plays the host's part in ticking it.
fn driver_on(rig: &Rig, kernel: &Arc<Mutex<Kernel>>) -> RkIep {
    let jobs = Box::new(rig.host.stream_producer(0, "jobs").unwrap());
    RkIep::on(
        RkIepNeeds {
            jobs: lemnos_core::Held::boxed(jobs),
        },
        BoxedDevice::new(Node(Arc::clone(kernel))),
    )
}

fn kernel() -> Arc<Mutex<Kernel>> {
    Arc::new(Mutex::new(Kernel::new(1 << Block::Iep2.facts().client)))
}

/// The word at byte `at` of the task the kernel last took, once it is done.
fn task_word(kernel: &Arc<Mutex<Kernel>>, at: usize) -> u32 {
    let k = kernel.lock().unwrap();
    assert_eq!(k.queued, 0);
    let slots = k.tasks.len();
    let task = &k.tasks[(k.head + slots - 1) % slots];
    assert!(task.iep2);
    task.regs[at / 4]
}

#[test]
fn a_board_file_binds_it_and_what_it_names_is_checked() {
    // The real node isn't there: the device binds (nothing is opened at bind), says what it
    // is, and a job fails with the reason in its health.
    let rig = Rig::start(&board("rk-iep", true), true);
    rig.ticks(1);
    assert_eq!(rig.host.device("iep").unwrap().health, Health::Ok);
    let mut device: Bound<dyn CodecBlock, _> = Bound::new(Through {
        host: &rig.host,
        args: [0; 64],
    });
    let mut name = [0u8; MAX_BLOCK_NAME];
    let len = device.block(&mut name).unwrap();
    assert_eq!(&name[..len], b"rk-mpp:iep2");
    let len = device.job_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"jobs");
    let info = device.info().unwrap();
    assert_eq!(
        (
            info.max_refs,
            info.max_items,
            info.max_read_back,
            info.max_work_bytes
        ),
        (SLOTS as u16, 2, RESULTS_BYTES as u32, MAX_WORK_BYTES as u32)
    );
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    rig.ticks(1);
    caller.post(1, WORK, RESULT, &refs(), &[(TAG_CONFIG, &config(USUAL))]);
    rig.ticks(1);
    let (identity, record, _) = caller.completion().unwrap();
    assert_eq!((identity, record.status), (1, JobStatus::Failed));
    assert_eq!(
        rig.host.device("iep").unwrap().health,
        Health::Degraded(NOT_THERE)
    );

    // A stream of plain memory, or an empty node: the bind is refused.
    for (text, dma_buf) in [
        (board("rk-iep", false), false),
        (
            board("rk-iep", true).replace("/nonexistent/mpp_service", ""),
            true,
        ),
    ] {
        let rig = Rig::start(&text, dma_buf);
        rig.ticks(1);
        assert_ne!(
            rig.host.device("iep").unwrap().state,
            DeviceState::Running,
            "{text}"
        );
    }
}

#[test]
fn a_deinterlace_job_runs_on_the_streams_buffers_and_its_counts_come_back() {
    let rig = Rig::start(&board("nothing", true), true);
    let kernel = kernel();
    for (i, byte) in kernel.lock().unwrap().output.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let mut driver = driver_on(&rig, &kernel);
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    let mut now = driver.tick(1).unwrap();

    let tuning = Tuning {
        md_lambda: 9,
        osd_area_num: 2,
        ..Tuning::default()
    };
    caller.post(
        7,
        WORK,
        RESULT,
        &refs(),
        &[(TAG_CONFIG, &config(USUAL)), (TAG_TUNING, &tuning.encode())],
    );
    now = driver.tick(now).unwrap();
    let (identity, record, back) = caller.completion().unwrap();
    assert_eq!((identity, record.status), (7, JobStatus::Done));
    // The block's counts, as the kernel copied them out, each word little-endian.
    assert_eq!(back.len(), RESULTS_BYTES);
    let expect: Vec<u8> = (0..RESULTS_BYTES)
        .map(|i| i as u8)
        .collect::<Vec<_>>()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|four| u32::from_ne_bytes(*four).to_le_bytes())
        .collect();
    assert_eq!(back, expect);

    // Every descriptor word the kernel translated is a duplicate the driver holds of a stream
    // buffer: the same dma-buf as the caller's, at the plane's offset.
    let translated: Vec<u32> = {
        let k = kernel.lock().unwrap();
        k.translated[..k.translated_len].to_vec()
    };
    assert_eq!(translated.len(), SLOTS);
    for (n, (y, c)) in [
        (Slot::CurY, Slot::CurC),
        (Slot::NextY, Slot::NextC),
        (Slot::PrevY, Slot::PrevC),
        (Slot::TopY, Slot::TopC),
        (Slot::BottomY, Slot::BottomC),
    ]
    .into_iter()
    .enumerate()
    {
        let fd = task_word(&kernel, y.word()).wrapping_sub(iova(0)) >> 20;
        assert_eq!(task_word(&kernel, y.word()), iova(fd), "{y:?}");
        assert_eq!(task_word(&kernel, c.word()), iova(fd) + LUMA, "{c:?}");
        assert!(translated.contains(&fd));
        let theirs = caller.handle(n as u32).as_fd().as_raw_fd() as u32;
        assert_ne!(fd, theirs, "the driver's own descriptor");
        let inode = |fd: u32| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(format!("/proc/self/fd/{fd}"))
                .unwrap()
                .ino()
        };
        assert_eq!(inode(fd), inode(theirs), "{y:?}");
    }
    // The caller's settings arrived as they were.
    assert_eq!(task_word(&kernel, 124), 9);
    assert_eq!(task_word(&kernel, 132), 2);
    assert_eq!(task_word(&kernel, 16), W / 16);
    let counters = CodecBlock::counters(&mut driver).unwrap();
    assert_eq!(
        (counters.jobs, counters.refused, counters.failed),
        (1, 0, 0)
    );
    assert_eq!(Lifecycle::health(&driver), Health::Ok);

    // The steady state: the same job again and again allocates nothing in the driver.
    let mut runs = 0;
    let mut allocated = 0;
    for identity in 8..40 {
        caller.post(
            identity,
            WORK,
            RESULT,
            &refs(),
            &[(TAG_CONFIG, &config(USUAL))],
        );
        let (next, n) = lemnos_measure::test_alloc::allocations(|| driver.tick(now).unwrap());
        now = next;
        allocated += n;
        let (_, record, _) = caller.completion().unwrap();
        assert_eq!(record.status, JobStatus::Done);
        runs += 1;
    }
    assert_eq!((runs, allocated), (32, 0), "serving jobs");
    assert_eq!(kernel.lock().unwrap().opens, 1);
}

#[test]
fn work_that_is_no_deinterlace_job_never_reaches_the_kernel() {
    let rig = Rig::start(&board("nothing", true), true);
    let kernel = kernel();
    let mut driver = driver_on(&rig, &kernel);
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    let mut now = driver.tick(1).unwrap();

    let usual = config(USUAL);
    let with = |word: usize, value: u32| {
        let mut words = USUAL;
        words[word] = value;
        config(words)
    };
    let mut no_md = refs();
    no_md.pop();
    let mut twice = refs();
    twice.push(reference(slot(Slot::Mv), 6, 0, 0));
    let mut long = refs();
    long[0].length = 1;
    let mut past = refs();
    past.push(reference(SLOTS as u32, 6, 0, 0));
    let mut cr = refs();
    cr.push(reference(slot(Slot::CurCr), 0, LUMA, 0));
    // The bottom frame's chroma one byte past the end of its stream buffer.
    let mut short = refs();
    short[9].offset = BYTES - (FRAME - LUMA) + 1;
    let mut far = refs();
    far[0].offset = BYTES;
    let mut unheld = refs();
    unheld[0].buffer = 99;
    let big_osd = Tuning {
        osd_area_num: 9,
        ..Tuning::default()
    }
    .encode();
    let tuning = Tuning::default().encode();

    type Case<'a> = (&'a str, Vec<BufferRef>, Vec<(u32, Vec<u8>)>);
    let cases: Vec<Case<'_>> = vec![
        ("no config", refs(), vec![]),
        (
            "a short config",
            refs(),
            vec![(TAG_CONFIG, usual[..CONFIG_BYTES - 4].to_vec())],
        ),
        (
            "two configs",
            refs(),
            vec![(TAG_CONFIG, usual.clone()), (TAG_CONFIG, usual.clone())],
        ),
        (
            "another tag",
            refs(),
            vec![(TAG_CONFIG, usual.clone()), (3, vec![0; 4])],
        ),
        (
            "a short tuning",
            refs(),
            vec![
                (TAG_CONFIG, usual.clone()),
                (TAG_TUNING, tuning[..TUNING_BYTES - 4].to_vec()),
            ],
        ),
        (
            "nine OSD areas",
            refs(),
            vec![(TAG_CONFIG, usual.clone()), (TAG_TUNING, big_osd.to_vec())],
        ),
        ("sampling 1", refs(), vec![(TAG_CONFIG, with(2, 1))]),
        ("layout P0", refs(), vec![(TAG_CONFIG, with(3, 2))]),
        ("a planar output", refs(), vec![(TAG_CONFIG, with(7, 3))]),
        ("mode I1O1T", refs(), vec![(TAG_CONFIG, with(9, 5))]),
        ("field order 2", refs(), vec![(TAG_CONFIG, with(10, 2))]),
        ("pulldown 3", refs(), vec![(TAG_CONFIG, with(11, 3))]),
        (
            "not whole tiles",
            refs(),
            vec![(TAG_CONFIG, with(0, W + 8))],
        ),
        ("too wide", refs(), vec![(TAG_CONFIG, with(0, 1936))]),
        (
            "a stride under a line",
            refs(),
            vec![(TAG_CONFIG, with(4, W - 4))],
        ),
        (
            "a stride past the buffers",
            refs(),
            vec![(TAG_CONFIG, with(8, 512))],
        ),
        ("no motion data", no_md, vec![(TAG_CONFIG, usual.clone())]),
        (
            "a slot named twice",
            twice,
            vec![(TAG_CONFIG, usual.clone())],
        ),
        ("a length", long, vec![(TAG_CONFIG, usual.clone())]),
        ("slot 15", past, vec![(TAG_CONFIG, usual.clone())]),
        (
            "a Cr plane, semi-planar",
            cr,
            vec![(TAG_CONFIG, usual.clone())],
        ),
        (
            "a plane a byte past its buffer",
            short,
            vec![(TAG_CONFIG, usual.clone())],
        ),
        (
            "an offset at the end",
            far,
            vec![(TAG_CONFIG, usual.clone())],
        ),
        (
            "a buffer the stream hasn't",
            unheld,
            vec![(TAG_CONFIG, usual.clone())],
        ),
    ];
    for (identity, (what, refs, items)) in cases.iter().enumerate() {
        let items: Vec<(u32, &[u8])> = items.iter().map(|(t, b)| (*t, b.as_slice())).collect();
        caller.post(identity as u64, WORK, RESULT, refs, &items);
        now = driver.tick(now).unwrap();
        let (_, record, _) = caller.completion().unwrap();
        assert_eq!(record.status, JobStatus::Refused, "{what}");
    }
    let k = kernel.lock().unwrap();
    assert_eq!((k.queued, k.polls, k.translated_len), (0, 0, 0));
}

#[test]
fn a_planar_input_names_its_cr_planes() {
    let rig = Rig::start(&board("nothing", true), true);
    let kernel = kernel();
    let mut driver = driver_on(&rig, &kernel);
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    let now = driver.tick(1).unwrap();
    // I420 in: Cb at LUMA, Cr a quarter-plane after, chroma lines W / 2.
    let mut words = USUAL;
    words[3] = 3;
    words[5] = W / 2;
    let cr = LUMA + LUMA / 4;
    let mut planar = refs();
    for (s, buffer) in [(Slot::CurCr, 0), (Slot::NextCr, 1), (Slot::PrevCr, 2)] {
        planar.push(reference(slot(s), buffer, cr, 0));
    }
    caller.post(1, WORK, RESULT, &planar, &[(TAG_CONFIG, &config(words))]);
    driver.tick(now).unwrap();
    assert_eq!(caller.completion().unwrap().1.status, JobStatus::Done);
    let cur = task_word(&kernel, Slot::CurY.word());
    assert_eq!(task_word(&kernel, Slot::CurCr.word()), cur + cr);
    // Semi-planar names none: a planar one must name all three.
    let mut missing = planar.clone();
    missing.pop();
    caller.post(2, WORK, RESULT, &missing, &[(TAG_CONFIG, &config(words))]);
    driver.tick(now + 1).unwrap();
    assert_eq!(caller.completion().unwrap().1.status, JobStatus::Refused);
}

#[test]
fn the_claim_is_given_up_when_the_caller_goes_and_retaken_for_the_next() {
    let rig = Rig::start(&board("nothing", true), true);
    let kernel = kernel();
    kernel.lock().unwrap().fail_open = Some(5);
    let mut driver = driver_on(&rig, &kernel);
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    let mut now = driver.tick(1).unwrap();
    let usual = config(USUAL);

    // The first claim fails: the job fails and health says why; the next job, after the
    // backoff, runs.
    caller.post(1, WORK, RESULT, &refs(), &[(TAG_CONFIG, &usual)]);
    now = driver.tick(now).unwrap();
    assert_eq!(caller.completion().unwrap().1.status, JobStatus::Failed);
    assert_eq!(Lifecycle::health(&driver), Health::Degraded(NOT_THERE));
    now += 250_000_000;
    caller.post(2, WORK, RESULT, &refs(), &[(TAG_CONFIG, &usual)]);
    now = driver.tick(now).unwrap();
    assert_eq!(caller.completion().unwrap().1.status, JobStatus::Done);
    assert_eq!(Lifecycle::health(&driver), Health::Ok);

    // Bytes that are no job: the caller loses the stream and the session is reset.
    caller.post_bytes(&[1, 2, 3]);
    now = driver.tick(now).unwrap();
    assert_eq!(kernel.lock().unwrap().resets, 1);

    // The next caller is served on a session of its own.
    let mut next = Caller::attach(&rig.host, 0, "jobs", 2);
    now = driver.tick(now).unwrap();
    next.post(3, WORK, RESULT, &refs(), &[(TAG_CONFIG, &usual)]);
    driver.tick(now).unwrap();
    assert_eq!(next.completion().unwrap().1.status, JobStatus::Done);
    assert_eq!(kernel.lock().unwrap().opens, 2);
}
