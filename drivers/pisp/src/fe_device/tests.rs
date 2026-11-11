//! The driver against a stand-in for the block: every expected value here is written out by hand.

use std::fs::File;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, NO_BUFFER,
};
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_FRONT_END};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::allocations;

use super::{Link, Options, PispFe, PispFeNeeds, backoff_ns};
use crate::fe::FrontEnd;
use crate::fe_hw::{Finished, Hardware, Placed, Plan, Registered, Set, slot_bytes};
use crate::fe_job::{CONFIG_BYTES, STATS_BYTES};
use crate::format::formats;
use crate::uapi::{Branch, FeConfig, ImageFormatConfig, Wire, fe_enable as en};

const CONFIG: u64 = 0x1000_0000;
/// Where the stand-in puts what is registered, and how far apart.
const REGIONS: u64 = 0x8000_0000;
const REGION_SPAN: u64 = 0x0100_0000;
const MAX_REGIONS: usize = 15;
const STRIDE: u32 = 1280;
const FRAME: u32 = STRIDE * 480;
/// Bytes between configuration slots.
const SLOT: u64 = slot_bytes(CONFIG_BYTES as u32);

struct Mock {
    plans: Vec<Plan>,
    started: Vec<Start>,
    stopped: u32,
    fail_make: Option<Status>,
    fail_check: Option<Status>,
    fail_post: Option<Status>,
    fail_register: bool,
    /// Each configuration slot, as the driver last wrote it.
    configs: Vec<[u8; CONFIG_BYTES]>,
    posted: Vec<Job>,
    finished: Vec<Finished>,
    given_back: Vec<u32>,
    /// Buffers given memory on the stream now, in order; cleared when a stream is made.
    set: Vec<u32>,
    /// What the module says to the next memory offered for a buffer.
    answer: Option<(u32, Set)>,
    /// Registered regions, by their number: their size.
    regions: [Option<u64>; MAX_REGIONS],
    /// Registrations made, ever.
    registrations: u32,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            plans: Vec::new(),
            started: Vec::new(),
            stopped: 0,
            fail_make: None,
            fail_check: None,
            fail_post: None,
            fail_register: false,
            configs: vec![[0; CONFIG_BYTES]; 8],
            posted: Vec::with_capacity(64),
            finished: Vec::with_capacity(64),
            given_back: Vec::with_capacity(4096),
            set: Vec::with_capacity(4096),
            answer: None,
            regions: [None; MAX_REGIONS],
            registrations: 0,
        }
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Mock>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn registered(&self) -> usize {
        self.with(|m| m.regions.iter().flatten().count())
    }
}

impl Hardware for Hw {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn make(&mut self, plan: &Plan) -> Result<Placed> {
        self.with(|m| match m.fail_make {
            Some(status) => Err(status),
            None => {
                m.plans.push(*plan);
                m.set.clear();
                m.regions = [None; MAX_REGIONS];
                Ok(Placed { config: CONFIG })
            }
        })
    }

    fn set_buffer(&mut self, buffer: u32, _memory: BorrowedFd<'_>) -> Result<Set> {
        Ok(self.with(|m| match m.answer {
            Some((b, answer)) if b == buffer => answer,
            _ => {
                m.set.push(buffer);
                Set::Done
            }
        }))
    }

    fn go(&mut self, start: &Start) -> Result<()> {
        self.with(|m| m.started.push(*start));
        Ok(())
    }

    fn stop(&mut self) {
        self.with(|m| {
            m.stopped += 1;
            m.regions = [None; MAX_REGIONS];
        });
    }

    fn write_config(&mut self, slot: u32, config: &[u8; CONFIG_BYTES]) -> Result<()> {
        self.with(|m| {
            let to = m
                .configs
                .get_mut(slot as usize)
                .ok_or(Status::INVALID_ARGUMENT)?;
            to.copy_from_slice(config);
            Ok(())
        })
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|m| {
            if m.fail_register {
                return Err(Status::NO_MEMORY);
            }
            let region = m
                .regions
                .iter()
                .position(Option::is_none)
                .ok_or(Status::NO_MEMORY)?;
            m.regions[region] = Some(size);
            m.registrations += 1;
            Ok(Registered {
                region: region as u32,
                addr: REGIONS + region as u64 * REGION_SPAN,
                size,
            })
        })
    }

    fn unregister(&mut self, region: u32) -> Result<()> {
        self.with(|m| match m.regions.get_mut(region as usize) {
            Some(slot @ Some(_)) => {
                *slot = None;
                Ok(())
            }
            _ => Err(Status::NOT_FOUND),
        })
    }

    fn post(&mut self, job: &Job) -> Result<()> {
        self.with(|m| match m.fail_post {
            Some(status) => Err(status),
            None => {
                m.posted.push(*job);
                Ok(())
            }
        })
    }

    fn next_finished(&mut self) -> Result<Option<Finished>> {
        Ok(self.with(|m| m.finished.pop()))
    }

    fn give_back(&mut self, buffer: u32) -> Result<()> {
        self.with(|m| m.given_back.push(buffer));
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        self.with(|m| m.fail_check.map_or(Ok(()), Err))
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters {
            jobs: 9,
            dropped: 2,
            errors: 1,
        })
    }
}

fn config(extra: u32) -> FeConfig {
    let mut fe = FrontEnd::new(640, 480, crate::uapi::BayerOrder::Rggb);
    fe.set_output_format(
        Branch::Zero,
        ImageFormatConfig {
            format: formats::BAYER16,
            ..Default::default()
        },
    );
    if extra & en::output(1) != 0 {
        fe.set_output_format(
            Branch::One,
            ImageFormatConfig {
                format: formats::BAYER16,
                ..Default::default()
            },
        );
    }
    fe.enable(en::output(0) | extra, true);
    fe.prepare().unwrap()
}

fn bytes_of(cfg: &FeConfig) -> Vec<u8> {
    let mut bytes = vec![0; CONFIG_BYTES];
    cfg.write_to(&mut bytes[..]).unwrap();
    bytes
}

fn options() -> Options {
    let link = Link {
        lanes: 2,
        mbps: 800,
        vc: 0,
        data_type: 0x2c,
    };
    Options::new(link, 4).unwrap()
}

fn image() -> IspOutput {
    IspOutput {
        width: 640,
        height: 480,
        fourcc: u32::from_le_bytes(*b"RG16"),
        pad_bytes: 0,
    }
}

/// A stream of `buffers` buffers of `bytes`, each behind a handle as a dma-buf stream's are.
fn handles(buffers: u32, bytes: u64) -> MockStream {
    let memory = (0..buffers).map(|_| sealed_memfd(c"buffer", bytes).unwrap());
    MockStream::with_handles(memory.collect())
}

/// The streams a test plays the client of.
struct Rig {
    fe: PispFe,
    hw: Hw,
    config: MockStream,
    stats: MockStream,
    frames: MockStream,
    out1: MockStream,
}

impl Rig {
    /// Writes `cfg` into config buffer `buffer`, as the client does.
    fn fill(&self, buffer: u32, cfg: &FeConfig) {
        self.config.fill(buffer, &bytes_of(cfg));
    }

    fn post(&self, job: &IspJob) {
        post(&self.config, job);
    }
}

/// Every stream: four config buffers, four statistics buffers, `frames` (four output 0 buffers
/// with handles unless given), and output 1 when `output1`.
fn rig_with(frames: Option<MockStream>, output1: bool) -> Rig {
    let hw = Hw::default();
    let hold = |s: &MockStream| Box::new(s.clone()) as StreamHandle;
    let config = MockStream::with_bytes(4, CONFIG_BYTES);
    let stats = handles(4, STATS_BYTES as u64);
    let frames = frames.unwrap_or_else(|| handles(4, 4096));
    let out1 = handles(4, u64::from(FRAME));
    let mut fe = PispFe::with_hardware(
        Box::new(hw.clone()),
        PispFeNeeds {
            config: hold(&config),
            statistics: Some(hold(&stats)),
            output0: Some(hold(&frames)),
            output1: output1.then(|| hold(&out1)),
        },
        options(),
    );
    CameraIsp::set_input(
        &mut fe,
        IspInput {
            width: 640,
            height: 480,
            bit_depth: 12,
            bayer: BayerOrder::default(),
        },
    )
    .unwrap();
    let layout = CameraIsp::set_output(&mut fe, 2, image()).unwrap();
    assert_eq!((layout.stride, layout.size), (STRIDE, FRAME));
    if output1 {
        CameraIsp::set_output(&mut fe, 3, image()).unwrap();
    }
    Rig {
        fe,
        hw,
        config,
        stats,
        frames,
        out1,
    }
}

fn rig() -> Rig {
    rig_with(None, false)
}

fn job(identity: u64, config: u32, output0: u32, output1: u32) -> IspJob {
    IspJob {
        identity,
        layout: LAYOUT_FRONT_END,
        config_buffer: config,
        input_buffer: NO_BUFFER,
        outputs: [NO_BUFFER, NO_BUFFER, output0, output1],
    }
}

fn post(stream: &MockStream, job: &IspJob) {
    let mut buf = [0; 128];
    let mut w = Writer::new(&mut buf);
    job.encode(&mut w).unwrap();
    let n = w.len();
    stream.post_job(&buf[..n]);
}

fn done(buffer: u32, identity: u64) -> Finished {
    Finished {
        buffer,
        bytes: FRAME,
        sequence: identity,
        identity,
        timestamp_ns: 5,
    }
}

#[test]
fn a_job_names_its_slot_and_the_clients_buffers_and_the_first_one_starts_the_block() {
    let mut r = rig_with(None, true);
    let cfg = config(en::AWB_STATS | en::output(1));
    r.fill(1, &cfg);
    r.fill(2, &config(0));
    CameraIsp::start(&mut r.fe).unwrap();
    assert_eq!(
        r.hw.with(|m| m.plans[0]),
        Plan {
            buffers: 4,
            job_slots: 4,
            output0_bytes: FRAME,
        }
    );
    // Nothing starts without a job: the module checks the first before writing a register.
    r.fe.tick(1);
    assert!(r.hw.with(|m| m.started.is_empty()));

    r.post(&job(0xabc, 1, 3, 2));
    r.fe.tick(2);
    r.post(&job(0xabd, 2, 0, NO_BUFFER));
    r.fe.tick(3);
    let (posted, started) = r.hw.with(|m| (m.posted.clone(), m.started.clone()));
    assert_eq!(started.len(), 1, "only the first job starts the block");
    assert_eq!(
        started[0],
        Start {
            stream_fd: -1,
            lanes: 2,
            mbps: 800,
            vc: 0,
            dt: 0x2c,
            width: 640,
            height: 480,
            output_width: 640,
            output_height: 480,
            output_stride: STRIDE,
            output_size: FRAME,
            output_format: formats::BAYER16,
            flags: 0,
            reserved: [0; 2],
        }
    );
    assert_eq!(posted[0].identity, 0xabc);
    assert_eq!(posted[0].buffer, 3);
    assert_eq!(posted[0].count, 3);
    assert_eq!(
        posted[0].range[..3],
        [
            // Output 0's buffer's configuration slot, which the driver wrote.
            Range {
                addr: CONFIG + 3 * SLOT,
                len: CONFIG_BYTES as u64
            },
            // Statistics buffer 1, registered first.
            Range {
                addr: REGIONS,
                len: STATS_BYTES as u64
            },
            // Output 1 buffer 2: what the configuration makes the block write.
            Range {
                addr: REGIONS + REGION_SPAN,
                len: u64::from(FRAME)
            },
        ]
    );
    assert_eq!(
        &r.hw.with(|m| m.configs[3].to_vec())[..],
        &bytes_of(&cfg)[..],
        "the slot holds the client's configuration as it was read"
    );
    assert_eq!(posted[1].count, 1);
    assert_eq!(posted[1].range[0].addr, CONFIG);
    assert_eq!(r.hw.registered(), 2, "lent until the frame is finished");
}

#[test]
fn statistics_and_output1_are_published_with_the_jobs_identity_and_let_go_of() {
    let mut r = rig_with(None, true);
    r.fill(1, &config(en::AWB_STATS | en::output(1)));
    CameraIsp::start(&mut r.fe).unwrap();
    r.post(&job(0x77, 1, 3, 2));
    r.fe.tick(1);
    assert_eq!(r.hw.registered(), 2);
    r.hw.with(|m| m.finished.push(done(3, 0x77)));
    r.fe.tick(2);
    assert_eq!(r.hw.registered(), 0, "unregistered once the block is done");
    let frame = r.frames.take_frame().unwrap();
    assert_eq!((frame.buffer, frame.identity), (3, 0x77));
    let stats = r.stats.take_frame().unwrap();
    assert_eq!(
        (stats.buffer, stats.identity, stats.bytes),
        (1, 0x77, STATS_BYTES as u32)
    );
    let out1 = r.out1.take_frame().unwrap();
    assert_eq!((out1.buffer, out1.identity, out1.bytes), (2, 0x77, FRAME));
    assert_eq!(r.fe.counters.dropped, 0);

    // Statistics buffer 1 is the client's now: a job that needs it is dropped until it's back.
    r.post(&job(0x78, 1, 0, 3));
    r.fe.tick(3);
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);
    assert_eq!(r.fe.counters.dropped, 1);
    assert_eq!(
        r.hw.registered(),
        0,
        "what was registered for it was let go"
    );
    r.stats.return_buffer(1);
    r.post(&job(0x79, 1, 0, 3));
    r.fe.tick(4);
    assert_eq!(r.hw.with(|m| m.posted.len()), 2);
}

#[test]
fn a_job_with_no_statistics_or_second_output_names_only_its_configuration() {
    let mut r = rig();
    r.fill(0, &config(0));
    CameraIsp::start(&mut r.fe).unwrap();
    r.post(&job(1, 0, 0, NO_BUFFER));
    r.fe.tick(1);
    let posted = r.hw.with(|m| m.posted[0]);
    assert_eq!(posted.count, 1);
    assert_eq!(posted.range[1], Range::default());
    assert_eq!(r.hw.registered(), 0);
}

#[test]
fn jobs_the_driver_can_see_are_wrong_are_dropped_and_not_posted() {
    let mut r = rig();
    r.fill(0, &config(0));
    CameraIsp::start(&mut r.fe).unwrap();
    let bad = [
        IspJob {
            layout: 1,
            ..job(1, 0, 0, NO_BUFFER)
        },
        IspJob {
            input_buffer: 0,
            ..job(1, 0, 0, NO_BUFFER)
        },
        IspJob {
            outputs: [1, NO_BUFFER, 0, NO_BUFFER],
            ..job(1, 0, 0, NO_BUFFER)
        },
        job(1, 4, 0, NO_BUFFER),
        job(1, 0, 4, NO_BUFFER),
        job(1, 0, NO_BUFFER, NO_BUFFER),
        // Output 1 named when the configuration has it off.
        job(1, 0, 0, 0),
    ];
    for j in &bad {
        r.post(j);
    }
    r.fe.tick(1);
    assert!(r.hw.with(|m| m.posted.is_empty()));
    assert_eq!(r.fe.counters.dropped, bad.len() as u64);
    assert!(matches!(r.fe.health(), Health::Ok));

    // Output 1 on, with no output 1 on the device.
    r.fill(1, &config(en::output(1)));
    r.post(&job(1, 1, 0, 0));
    r.fe.tick(2);
    assert!(r.hw.with(|m| m.posted.is_empty()));
    assert_eq!(r.fe.counters.dropped, bad.len() as u64 + 1);
}

#[test]
fn a_configuration_naming_an_address_never_reaches_the_module() {
    let mut r = rig();
    CameraIsp::start(&mut r.fe).unwrap();
    type Mutate = fn(&mut FeConfig);
    let cases: [Mutate; 5] = [
        |c| c.stats_buffer.addr_lo = 16,
        |c| c.output_buffer[0].addr_hi = 1,
        |c| c.output_buffer[1].addr_lo = 0x100,
        |c| c.input_buffer.addr_lo = 0x40,
        |c| c.ch[0].output.ilines = 1,
    ];
    for (n, mutate) in cases.iter().enumerate() {
        let mut wrong = config(0);
        mutate(&mut wrong);
        r.fill(0, &wrong);
        r.post(&job(1, 0, 0, NO_BUFFER));
        r.fe.tick(n as u64 + 1);
        assert!(r.hw.with(|m| m.posted.is_empty()), "case {n}");
        assert!(
            r.hw.with(|m| m.configs.iter().all(|c| c.iter().all(|&b| b == 0))),
            "case {n}: nothing was written to a slot"
        );
    }
    assert_eq!(r.fe.counters.dropped, cases.len() as u64);
}

#[test]
fn a_buffer_the_module_wont_register_or_that_is_too_small_drops_the_job() {
    let mut r = rig_with(None, true);
    r.fill(0, &config(en::AWB_STATS));
    r.fill(1, &config(en::output(1)));
    CameraIsp::start(&mut r.fe).unwrap();
    r.hw.with(|m| m.fail_register = true);
    r.post(&job(1, 0, 0, NO_BUFFER));
    r.fe.tick(1);
    assert!(r.hw.with(|m| m.posted.is_empty()));
    assert_eq!(r.fe.counters.dropped, 1);
    r.hw.with(|m| m.fail_register = false);

    // An output 1 buffer smaller than the image the configuration describes.
    let small = handles(4, u64::from(FRAME) - 16);
    r.fe.output1 = Some(Box::new(small.clone()));
    r.fe.out1 = super::Buffers::NONE;
    r.post(&job(2, 1, 0, 1));
    r.fe.tick(2);
    assert!(r.hw.with(|m| m.posted.is_empty()));
    assert_eq!(r.fe.counters.dropped, 2);
    assert_eq!(
        (r.hw.registered(), r.hw.with(|m| m.registrations)),
        (0, 1),
        "registered, measured, and let go"
    );
}

#[test]
fn a_full_ring_drops_the_job_and_frees_what_it_had() {
    let mut r = rig();
    r.fill(0, &config(en::AWB_STATS));
    CameraIsp::start(&mut r.fe).unwrap();
    r.hw.with(|m| m.fail_post = Some(Status::BUSY));
    r.post(&job(1, 0, 2, NO_BUFFER));
    r.fe.tick(1);
    assert_eq!(r.fe.counters.dropped, 1);
    assert_eq!(r.hw.registered(), 0);
    assert!(
        matches!(r.fe.health(), Health::Ok),
        "a full ring is not the end"
    );
    r.hw.with(|m| m.fail_post = None);
    r.post(&job(2, 0, 2, NO_BUFFER));
    r.fe.tick(2);
    assert_eq!(
        r.hw.with(|m| m.posted.len()),
        1,
        "output 0's buffer and the statistics buffer are free again"
    );
}

#[test]
fn a_statistics_client_that_left_gets_nothing_and_the_next_one_gets_its_buffer() {
    let mut r = rig();
    r.fill(0, &config(en::AWB_STATS));
    CameraIsp::start(&mut r.fe).unwrap();
    r.fe.tick(1);
    r.stats.change_consumer();
    // Statistics buffer 0 was taken for the client that left: given up, and the job dropped.
    r.post(&job(1, 0, 0, NO_BUFFER));
    r.fe.tick(2);
    assert!(r.hw.with(|m| m.posted.is_empty()));
    assert_eq!(r.fe.counters.dropped, 1);
    // Taken again as the new client's.
    r.post(&job(2, 0, 0, NO_BUFFER));
    r.fe.tick(3);
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);
    // That client leaves while the block writes its buffer: the statistics reach no one.
    r.stats.change_consumer();
    r.hw.with(|m| m.finished.push(done(0, 2)));
    r.fe.tick(4);
    assert!(r.stats.take_frame().is_none());
    assert_eq!(r.frames.take_frame().map(|f| f.identity), Some(2));
    assert_eq!(r.fe.counters.dropped, 2);
    assert_eq!(r.hw.registered(), 0);
}

#[test]
fn a_refused_job_ends_the_stream_and_everything_is_made_again() {
    let mut r = rig();
    r.fill(0, &config(en::AWB_STATS));
    CameraIsp::start(&mut r.fe).unwrap();
    r.hw.with(|m| m.fail_post = Some(Status::INVALID_ARGUMENT));
    r.post(&job(1, 0, 0, NO_BUFFER));
    assert_eq!(r.fe.tick(10), Some(10 + backoff_ns(0)));
    assert!(matches!(r.fe.health(), Health::Degraded(_)));
    assert_eq!(
        r.hw.with(|m| m.stopped),
        2,
        "begin stopped once, the failure once"
    );
    r.hw.with(|m| m.fail_post = None);
    r.fe.tick(10 + backoff_ns(0));
    assert!(matches!(r.fe.health(), Health::Ok));
    assert_eq!(r.hw.with(|m| m.plans.len()), 2);
    assert!(
        !r.fe.started,
        "a new stream's first job starts the block again"
    );
    // The statistics buffer the failed job had is the driver's again.
    r.post(&job(2, 0, 0, NO_BUFFER));
    r.fe.tick(11 + backoff_ns(0));
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);
}

#[test]
fn recovery_never_stops_and_backs_off_to_eight_seconds() {
    let mut r = rig();
    CameraIsp::start(&mut r.fe).unwrap();
    r.hw.with(|m| {
        m.fail_check = Some(Status::IO);
        m.fail_make = Some(Status::NO_MEMORY);
    });
    r.fe.tick(0);
    let mut now = 1_000_000_000;
    let mut due = r.fe.tick(now).unwrap();
    assert_eq!(
        due - now,
        backoff_ns(0),
        "the check that fails, after a first look"
    );
    let mut last = 0;
    for _ in 0..100 {
        now = due;
        due = r.fe.tick(now).unwrap();
        last = due - now;
        assert!(last <= 8_000_000_000);
    }
    assert_eq!(last, 8_000_000_000);
    r.hw.with(|m| m.fail_make = None);
    r.fe.tick(due);
    assert!(matches!(r.fe.health(), Health::Ok));
}

#[test]
fn stop_ends_recovery_and_only_the_front_ends_outputs_exist() {
    let mut r = rig();
    let fe = &mut r.fe;
    assert_eq!(
        CameraIsp::set_output(fe, 0, image()),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        CameraIsp::set_output(fe, 3, image()),
        Err(Status::NOT_FOUND),
        "output 1 needs its stream"
    );
    assert_eq!(
        CameraIsp::set_output(
            fe,
            2,
            IspOutput {
                fourcc: u32::from_le_bytes(*b"NV12"),
                ..image()
            }
        ),
        Err(Status::INVALID_ARGUMENT)
    );
    CameraIsp::start(fe).unwrap();
    assert_eq!(CameraIsp::set_output(fe, 2, image()), Err(Status::BUSY));
    r.hw.with(|m| m.fail_check = Some(Status::IO));
    fe.tick(1);
    fe.tick(1 + 1_000_000_000);
    assert!(matches!(fe.health(), Health::Degraded(_)));
    CameraIsp::stop(fe).unwrap();
    assert_eq!(fe.tick(1 << 40), None);
    assert!(matches!(fe.health(), Health::Ok));
}

#[test]
fn the_streams_are_named_as_the_needs_are() {
    let mut r = rig_with(None, true);
    let mut name = [0u8; 32];
    let fe = &mut r.fe;
    let n = CameraIsp::config_stream(fe, &mut name).unwrap();
    assert_eq!(&name[..n], b"config");
    let n = CameraIsp::statistics_stream(fe, &mut name).unwrap();
    assert_eq!(&name[..n], b"statistics");
    let n = CameraIsp::output_stream(fe, 2, &mut name).unwrap();
    assert_eq!(&name[..n], b"output0");
    let n = CameraIsp::output_stream(fe, 3, &mut name).unwrap();
    assert_eq!(&name[..n], b"output1");
    assert_eq!(
        CameraIsp::output_stream(fe, 0, &mut name),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        CameraIsp::input_stream(fe, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        CameraIsp::output_stream(&mut rig().fe, 3, &mut name),
        Err(Status::NOT_FOUND)
    );
}

#[test]
fn compression_changes_the_layout_and_is_stopped_only() {
    let mut r = rig();
    let fe = &mut r.fe;
    IspPisp::set_front_end_compression(fe, 2, true).unwrap();
    assert_eq!(fe.outputs[0].unwrap().format, formats::PISP_COMP1);
    assert_eq!(fe.outputs[0].unwrap().layout.stride, 640);
    assert_eq!(
        IspPisp::set_front_end_compression(fe, 1, true),
        Err(Status::NOT_FOUND)
    );
    CameraIsp::start(fe).unwrap();
    assert_eq!(
        IspPisp::set_front_end_compression(fe, 2, false),
        Err(Status::BUSY)
    );
}

#[test]
fn a_finished_job_is_published_and_its_buffer_goes_back_when_returned() {
    let mut r = rig();
    r.fill(0, &config(0));
    CameraIsp::start(&mut r.fe).unwrap();
    assert_eq!(
        r.hw.with(|m| m.set.clone()),
        [0, 1, 2, 3],
        "every buffer of the stream was given to the module"
    );
    r.post(&job(7, 0, 2, NO_BUFFER));
    r.fe.tick(1);
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);
    r.hw.with(|m| m.finished.push(done(2, 7)));
    r.fe.tick(2);
    assert_eq!(r.fe.counters.jobs, 1);
    assert_eq!(r.frames.take_frame().unwrap().identity, 7);
    assert_eq!(
        r.hw.with(|m| m.given_back.clone()),
        [],
        "the consumer has it"
    );

    // A job on the buffer the consumer holds isn't sent.
    r.post(&job(8, 0, 2, NO_BUFFER));
    r.fe.tick(3);
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);
    assert_eq!(r.fe.counters.dropped, 1);

    r.frames.return_buffer(2);
    r.fe.tick(4);
    assert_eq!(r.hw.with(|m| m.given_back.clone()), [2]);
    assert_eq!(
        r.hw.with(|m| m.set.clone()),
        [0, 1, 2, 3, 2],
        "back to the block only after the module was shown the memory behind it again"
    );
    r.post(&job(9, 0, 2, NO_BUFFER));
    r.fe.tick(5);
    assert_eq!(r.hw.with(|m| m.posted.len()), 2);
}

#[test]
fn a_second_job_on_a_buffer_a_job_already_has_is_dropped() {
    let mut r = rig();
    r.fill(0, &config(0));
    CameraIsp::start(&mut r.fe).unwrap();
    r.post(&job(1, 0, 3, NO_BUFFER));
    r.post(&job(2, 0, 3, NO_BUFFER));
    r.fe.tick(1);
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);
    assert_eq!(r.hw.with(|m| m.posted[0].identity), 1);
    assert_eq!(r.fe.counters.dropped, 1);
}

#[test]
fn a_job_on_a_buffer_whose_memory_the_module_does_not_have_is_dropped() {
    let mut r = rig();
    r.fill(0, &config(0));
    // The module won't take buffer 1's memory yet.
    r.hw.with(|m| m.answer = Some((1, Set::Busy)));
    CameraIsp::start(&mut r.fe).unwrap();
    r.post(&job(1, 0, 1, NO_BUFFER));
    r.fe.tick(1);
    assert!(r.hw.with(|m| m.posted.is_empty()));
    assert_eq!(r.fe.counters.dropped, 1);
    r.post(&job(2, 0, 0, NO_BUFFER));
    r.fe.tick(2);
    assert_eq!(
        r.hw.with(|m| m.posted.len()),
        1,
        "a buffer it does have is fine"
    );
}

#[test]
fn a_consumer_that_leaves_gets_nothing_more_and_the_next_one_gets_its_own_memory() {
    let mut r = rig();
    r.fill(0, &config(0));
    CameraIsp::start(&mut r.fe).unwrap();
    r.post(&job(1, 0, 1, NO_BUFFER));
    r.fe.tick(1);
    assert_eq!(r.hw.with(|m| m.posted.len()), 1);

    // The consumer goes while the block has buffer 1 for that job.
    r.frames.change_consumer();
    r.hw.with(|m| {
        m.set.clear();
        m.answer = Some((1, Set::Busy));
    });
    r.fe.tick(2);
    assert_eq!(
        r.hw.with(|m| m.set.clone()),
        [0, 2, 3],
        "every hold was given up and taken again; the module has the new memory of all but 1"
    );
    // The job finishes in the old consumer's memory: not published, and only then swapped.
    r.hw.with(|m| {
        m.answer = None;
        m.finished.push(done(1, 1));
    });
    r.fe.tick(3);
    assert!(
        r.frames.take_frame().is_none(),
        "the new consumer never sees it"
    );
    assert_eq!(r.fe.counters.dropped, 1);
    assert_eq!(r.hw.with(|m| m.set.clone()), [0, 2, 3, 1]);
    assert_eq!(r.hw.with(|m| m.given_back.clone()), [1]);
    assert_eq!(
        r.hw.with(|m| m.plans.len()),
        1,
        "the block was never stopped"
    );
}

#[test]
fn a_stream_whose_buffers_have_no_handles_is_refused() {
    // Plain shared memory: there is nothing to give the block.
    let mut r = rig_with(Some(MockStream::with_bytes(4, 4096)), false);
    assert_eq!(CameraIsp::start(&mut r.fe), Err(Status::NOT_SUPPORTED));
    assert!(r.hw.with(|m| m.set.is_empty()));
    assert_eq!(r.fe.tick(1), None, "not running");
}

#[test]
fn a_statistics_stream_without_handles_is_refused_at_start() {
    let mut r = rig();
    let plain = MockStream::with_bytes(4, STATS_BYTES);
    r.fe.statistics = Some(Box::new(plain.clone()));
    assert_eq!(CameraIsp::start(&mut r.fe), Err(Status::NOT_SUPPORTED));
    assert!(r.hw.with(|m| m.set.is_empty()));
    assert_eq!(r.fe.tick(1), None, "not running");
}

#[test]
fn memory_the_module_refuses_is_a_refused_start() {
    let mut r = rig();
    r.hw.with(|m| m.answer = Some((0, Set::Refused)));
    assert_eq!(CameraIsp::start(&mut r.fe), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn without_an_output0_stream_there_is_nothing_to_start() {
    let mut r = rig();
    r.fe.output0 = None;
    assert_eq!(CameraIsp::start(&mut r.fe), Err(Status::NOT_FOUND));
    assert!(
        r.hw.with(|m| m.plans.is_empty()),
        "the block was never touched"
    );
}

#[test]
fn the_steady_state_allocates_nothing() {
    let mut r = rig_with(None, true);
    r.fill(0, &config(en::AWB_STATS | en::output(1)));
    CameraIsp::start(&mut r.fe).unwrap();
    r.fe.tick(1);
    // A job posted, sent with its statistics and output 1 lent, finished, all three published,
    // read, returned and given back to the block, every round. Only the driver is counted: the
    // stand-in client's queue of jobs allocates for each one.
    let mut now = 2;
    let mut round = |r: &mut Rig| -> u64 {
        let mut allocated = 0;
        let mut tick = |fe: &mut PispFe, at: u64| {
            let (_, n) = allocations(|| {
                fe.tick(at);
                let _ = CameraIsp::counters(fe);
                let _ = fe.health();
            });
            allocated += n;
        };
        r.post(&job(now, 0, 0, 1));
        tick(&mut r.fe, now);
        r.hw.with(|m| {
            assert_eq!(m.posted.len(), 1);
            m.posted.clear();
            m.finished.push(done(0, now));
            m.given_back.clear();
            m.set.clear();
        });
        tick(&mut r.fe, now + 1);
        assert_eq!(r.frames.take_frame().map(|f| f.identity), Some(now));
        assert_eq!(r.stats.take_frame().map(|f| f.identity), Some(now));
        assert_eq!(r.out1.take_frame().map(|f| f.identity), Some(now));
        r.frames.return_buffer(0);
        r.stats.return_buffer(0);
        r.out1.return_buffer(1);
        tick(&mut r.fe, now + 2);
        assert!(r.hw.with(|m| m.given_back == [0]));
        now += 3;
        allocated as u64
    };
    for _ in 0..16 {
        round(&mut r);
    }
    let allocated: u64 = (0..64).map(|_| round(&mut r)).sum();
    assert_eq!(
        allocated, 0,
        "jobs in, frames and statistics out, buffers back, counters and the health check"
    );
    assert_eq!(r.fe.counters.dropped, 0);
}
