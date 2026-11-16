//! The driver against a stand-in for the block: every expected value here is written out by hand.
//! The path through the host's own streams, with real memory read and written, is in
//! `tests/be_frames.rs`.

use std::fs::File;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_BACK_END};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::allocations;

use super::{Options, PispBe, PispBeNeeds, backoff_ns};
use crate::be::BackEnd;
use crate::be_hw::{Finished, Hardware, Placed, Plan, Registered};
use crate::be_job::{KernelJob, addr_output};
use crate::format::{compute_stride_align, formats};
use crate::uapi::{
    BayerOrder as Order, BeCcmConfig, BeOutputFormatConfig, BeTilesConfig, Branch,
    ImageFormatConfig, Wire, rgb_enable,
};
use lemnos_driver_pisp_sys::CONFIG_BYTES;

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const TDN: [u64; 2] = [0x4000_0000, 0x4100_0000];
/// Where the stand-in puts what is registered: region `n` at `REGIONS + n * REGION_SPAN`.
const REGIONS: u64 = 0x8000_0000;
const REGION_SPAN: u64 = 0x0100_0000;
const INPUT_BYTES: u64 = 2560 * 800;
const OUTPUT_BYTES: u64 = 1280 * 800 + 1280 * 400;

struct Mock {
    plans: Vec<Plan>,
    stopped: u32,
    fail_start: Option<Status>,
    fail_check: Option<Status>,
    fail_post: Option<Status>,
    posted: Vec<(KernelJob, Box<[u8; CONFIG_BYTES]>)>,
    finished: Vec<Finished>,
    given_back: Vec<u32>,
    /// Registered and not let go of: size, by region.
    regions: Vec<Option<u64>>,
    /// What `stop` reports: the block could not be seen to stop.
    wedged_on_stop: bool,
    /// Set: `register_lent` refuses with this status, as the kernel refuses a file that is not a dma-buf.
    refuse_lent: Option<Status>,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            plans: Vec::new(),
            stopped: 0,
            fail_start: None,
            fail_check: None,
            fail_post: None,
            posted: Vec::with_capacity(64),
            finished: Vec::with_capacity(64),
            given_back: Vec::with_capacity(64),
            regions: Vec::with_capacity(64),
            wedged_on_stop: false,
            refuse_lent: None,
        }
    }
}

impl Mock {
    fn live(&mut self) -> usize {
        self.regions.iter().flatten().count()
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Mock>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn posted(&self) -> Vec<KernelJob> {
        self.with(|m| m.posted.iter().map(|(job, _)| *job).collect())
    }

    fn finish(&self, buffer: u32, identity: u64, bytes: u32) {
        self.with(|m| {
            m.finished.push(Finished {
                buffer,
                bytes,
                sequence: identity,
                identity,
                timestamp_ns: 13,
                end_ns: 0,
            });
        });
    }
}

impl Hardware for Hw {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn start(&mut self, plan: &Plan) -> Result<Placed> {
        self.with(|m| match m.fail_start {
            Some(status) => Err(status),
            None => {
                m.plans.push(*plan);
                m.regions.clear();
                Ok(Placed {
                    tdn: plan.tdn_bytes.map(|_| TDN),
                })
            }
        })
    }

    fn stop(&mut self) -> bool {
        self.with(|m| {
            m.stopped += 1;
            m.regions.clear();
            m.wedged_on_stop
        })
    }

    /// Stand-in: the block reads a lent dma-buf the same way it reads any region here. The real
    /// `DMA_TO_DEVICE` mapping is the module's, checked on the QEMU harness (3b).
    fn register_lent(&mut self, memory: BorrowedFd<'_>, _generation: u32) -> Result<Registered> {
        if let Some(status) = self.with(|m| m.refuse_lent) {
            return Err(status);
        }
        self.register(memory)
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|m| {
            let region = m.regions.len() as u32;
            m.regions.push(Some(size));
            Ok(Registered {
                region,
                addr: REGIONS + u64::from(region) * REGION_SPAN,
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

    fn post(&mut self, job: &KernelJob, config: &[u8; CONFIG_BYTES]) -> Result<()> {
        self.with(|m| match m.fail_post {
            Some(status) => Err(status),
            None => {
                m.posted.push((*job, Box::new(*config)));
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
}

const CSC: BeCcmConfig = BeCcmConfig {
    coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
    pad: [0; 2],
    offsets: [0, 33554432, 33554432],
};

fn config() -> BeTilesConfig {
    let mut input = ImageFormatConfig {
        width: 1280,
        height: 800,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    BackEnd::simple_bayer(
        input,
        Order::Bggr,
        4096,
        (1.5, 1.0, 1.8),
        None,
        formats::NV12,
        Some(CSC),
    )
    .prepare()
    .unwrap()
}

fn bytes_of(cfg: &BeTilesConfig) -> Vec<u8> {
    let mut bytes = vec![0; CONFIG_BYTES];
    cfg.write_to(&mut bytes[..]).unwrap();
    bytes
}

fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// A stream of `buffers` dma-bufs of `bytes` each; sealed memfds stand in.
fn handles(buffers: u32, bytes: u64) -> MockStream {
    MockStream::with_handles(
        (0..buffers)
            .map(|_| sealed_memfd(c"be-test", bytes).unwrap())
            .collect(),
    )
}

/// The client's side of every stream.
#[derive(Clone)]
struct Streams {
    config: MockStream,
    input: MockStream,
    output0: MockStream,
    output1: Option<MockStream>,
}

impl Streams {
    fn new() -> Self {
        Self {
            config: MockStream::with_bytes(4, CONFIG_BYTES),
            input: handles(4, INPUT_BYTES),
            output0: handles(4, OUTPUT_BYTES),
            output1: None,
        }
    }

    fn needs(&self) -> PispBeNeeds {
        let hold =
            |s: &MockStream| -> StreamHandle { lemnos_core::Held::boxed(Box::new(s.clone())) };
        PispBeNeeds {
            config: hold(&self.config),
            input: Some(hold(&self.input)),
            output0: hold(&self.output0),
            output1: self.output1.as_ref().map(hold),
            lend: None,
        }
    }

    /// Writes `cfg` into config buffer 1, the one [`job`] names.
    fn configure(&self, cfg: &BeTilesConfig) {
        self.config.fill(1, &bytes_of(cfg));
    }

    fn post(&self, job: &IspJob) {
        let mut buf = [0; 128];
        let mut w = Writer::new(&mut buf);
        job.encode(&mut w).unwrap();
        let n = w.len();
        self.config.post_job(&buf[..n]);
    }
}

fn setup_with(streams: &Streams, options: Options) -> (PispBe, Hw) {
    setup_needs(streams.needs(), options)
}

fn setup_needs(needs: PispBeNeeds, options: Options) -> (PispBe, Hw) {
    let hw = Hw::default();
    let mut be = PispBe::with_hardware(Box::new(hw.clone()), needs, options);
    CameraIsp::set_input(
        &mut be,
        IspInput {
            width: 1280,
            height: 800,
            bit_depth: 12,
            bayer: BayerOrder::default(),
        },
    )
    .unwrap();
    let layout = CameraIsp::set_output(
        &mut be,
        0,
        IspOutput {
            width: 1280,
            height: 800,
            fourcc: fourcc(b"NV12"),
            pad_bytes: 0,
        },
    )
    .unwrap();
    assert_eq!(layout.stride, 1280);
    assert_eq!(u64::from(layout.size), OUTPUT_BYTES);
    (be, hw)
}

fn setup(streams: &Streams) -> (PispBe, Hw) {
    setup_with(streams, Options::new(4, 4).unwrap())
}

fn job(identity: u64, input: u32, output: u32) -> IspJob {
    job_on(identity, 1, input, output)
}

fn job_on(identity: u64, config: u32, input: u32, output: u32) -> IspJob {
    IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: config,
        input_buffer: input,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
        input_lent: false,
        lent_buffer: 0,
    }
}

fn run_once(be: &mut PispBe, now: u64) -> Option<u64> {
    be.tick(now)
}

fn dropped(be: &mut PispBe) -> u64 {
    CameraIsp::counters(be).unwrap().dropped
}

#[test]
fn a_job_reaches_the_block_at_the_buffers_it_named_with_the_clients_config() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    let mut cfg = config();
    cfg.config.wbg.gain_r = 0x1234;
    streams.configure(&cfg);
    CameraIsp::start(&mut be).unwrap();
    assert_eq!(
        hw.with(|m| m.plans[0]),
        Plan {
            buffers: 4,
            job_slots: 4,
            tdn_bytes: None,
        }
    );
    streams.post(&job(0xabc, 2, 3));
    run_once(&mut be, 1);
    let posted = hw.with(|m| m.posted.clone());
    assert_eq!(posted.len(), 1);
    let (k, sent) = &posted[0];
    assert_eq!(k.identity, 0xabc);
    assert_eq!(k.buffer, 0, "the first free pool buffer counts the job");
    // The input registered first, then output 0.
    assert_eq!(k.addr[0], REGIONS);
    assert_eq!(k.addr[1], 0);
    assert_eq!(k.addr[addr_output(0, 0)], REGIONS + REGION_SPAN);
    assert_eq!(
        k.addr[addr_output(0, 1)],
        REGIONS + REGION_SPAN + 1280 * 800
    );
    assert_eq!(k.addr[addr_output(1, 0)], 0);
    assert_eq!(&sent[..], &bytes_of(&cfg)[..], "the client's configuration");
    assert_eq!(
        hw.with(|m| m.regions.clone()),
        [Some(INPUT_BYTES), Some(OUTPUT_BYTES)]
    );
    assert_eq!(dropped(&mut be), 0);
}

#[test]
fn a_finished_job_lets_go_of_its_images_and_publishes_its_output() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(7, 2, 3));
    run_once(&mut be, 1);
    hw.finish(0, 7, 4096);
    run_once(&mut be, 2);
    assert_eq!(hw.with(Mock::live), 0, "nothing still registered");
    assert_eq!(hw.with(|m| m.given_back.clone()), [0]);
    let frame = streams.output0.take_frame().expect("output 0's frame");
    assert_eq!(
        (frame.buffer, frame.identity, u64::from(frame.bytes)),
        (3, 7, OUTPUT_BYTES)
    );
    assert!(
        streams.input.take_frame().is_none(),
        "the input stays the driver's"
    );
    assert_eq!(CameraIsp::counters(&mut be).unwrap().jobs, 1);

    // The same input again, and output 3 is the client's now.
    streams.post(&job(8, 2, 3));
    run_once(&mut be, 3);
    assert_eq!(hw.posted().len(), 1);
    assert_eq!(dropped(&mut be), 1);
    streams.post(&job(9, 2, 1));
    run_once(&mut be, 4);
    assert_eq!(hw.posted().len(), 2);
}

#[test]
fn a_job_the_block_failed_publishes_nothing_and_keeps_its_buffers() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(7, 2, 3));
    run_once(&mut be, 1);
    hw.finish(0, 7, 0);
    run_once(&mut be, 2);
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(hw.with(Mock::live), 0);
    assert_eq!(CameraIsp::counters(&mut be).unwrap().errors, 1);
    streams.post(&job(8, 2, 3));
    run_once(&mut be, 3);
    assert_eq!(
        hw.posted().len(),
        2,
        "output 3 is still the driver's to name"
    );
}

#[test]
fn jobs_the_driver_can_see_are_wrong_are_dropped_and_not_posted() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();

    let bad = [
        IspJob {
            layout: 99,
            ..job(1, 0, 0)
        },
        // Config buffer 4 isn't on the stream; 40 is past what the driver tracks.
        IspJob {
            config_buffer: 4,
            ..job(1, 0, 0)
        },
        IspJob {
            config_buffer: 40,
            ..job(1, 0, 0)
        },
        job(1, 4, 0),
        job(1, NO_BUFFER, 0),
        job(1, 0, 4),
        // Output 0 is on in the configuration: it needs a buffer.
        job(1, 0, NO_BUFFER),
        // Output 1 has no stream on this board.
        IspJob {
            outputs: [0, 1, NO_BUFFER, NO_BUFFER],
            ..job(1, 0, 0)
        },
        IspJob {
            outputs: [0, NO_BUFFER, 1, NO_BUFFER],
            ..job(1, 0, 0)
        },
    ];
    for b in &bad {
        streams.post(b);
    }
    streams.config.post_job(&[1, 2, 3]);
    run_once(&mut be, 1);
    assert!(hw.posted().is_empty());
    assert_eq!(dropped(&mut be), 10);
    assert_eq!(hw.with(Mock::live), 0, "nothing left registered");
    assert!(
        matches!(be.health(), Health::Ok),
        "a caller's mistake is not a failure"
    );
}

#[test]
fn a_config_the_module_would_refuse_is_never_posted() {
    // The bus settings, and the head where older layouts kept addresses: the module supplies
    // what goes there, so a client's non-zero bytes are refused, not passed on.
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    CameraIsp::start(&mut be).unwrap();
    let mut axi = config();
    axi.config.axi.r_qos = 1;
    let mut address = config();
    address.config.pad0[8..16].copy_from_slice(&0x2000_0000u64.to_le_bytes());
    let mut hog = config();
    hog.config.pad0[104] = 1;
    for (n, cfg) in [axi, address, hog].iter().enumerate() {
        streams.configure(cfg);
        streams.post(&job(n as u64, 0, 0));
        run_once(&mut be, n as u64 + 1);
    }
    assert!(hw.posted().is_empty());
    assert_eq!(dropped(&mut be), 3);
    assert!(
        hw.with(|m| m.regions.is_empty()),
        "refused before anything was lent"
    );
}

#[test]
fn an_image_larger_than_its_buffer_is_dropped_and_let_go() {
    let streams = Streams {
        input: handles(4, INPUT_BYTES - 64),
        ..Streams::new()
    };
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(1, 0, 0));
    run_once(&mut be, 1);
    assert!(hw.posted().is_empty());
    assert_eq!(dropped(&mut be), 1);
    assert_eq!(hw.with(|m| m.regions.len()), 1, "registered, measured");
    assert_eq!(hw.with(Mock::live), 0, "and let go");
}

#[test]
fn streams_whose_buffers_have_no_handles_are_refused_at_start() {
    for streams in [
        Streams {
            input: MockStream::new(4),
            ..Streams::new()
        },
        Streams {
            output0: MockStream::new(4),
            ..Streams::new()
        },
    ] {
        let (mut be, hw) = setup(&streams);
        streams.configure(&config());
        assert_eq!(CameraIsp::start(&mut be), Err(Status::NOT_SUPPORTED));
        assert_eq!(be.tick(1), None, "not running");
        assert!(hw.posted().is_empty());
        assert_eq!(hw.with(Mock::live), 0);
    }
}

#[test]
fn a_job_needs_a_free_pool_buffer_and_room_on_the_ring() {
    let streams = Streams::new();
    let (mut be, hw) = setup_with(&streams, Options::new(2, 4).unwrap());
    // Each job names its own configuration buffer, so the ring is what limits them.
    for buffer in 1..=3 {
        streams.config.fill(buffer, &bytes_of(&config()));
    }
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job_on(1, 1, 0, 0));
    streams.post(&job_on(2, 2, 1, 1));
    streams.post(&job_on(3, 3, 2, 2));
    run_once(&mut be, 1);
    let posted = hw.posted();
    assert_eq!(posted.iter().map(|k| k.buffer).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(dropped(&mut be), 1, "two jobs on the block at most");

    // A full ring: dropped, its images let go of, and the stream goes on.
    hw.finish(0, 1, 4096);
    hw.with(|m| m.fail_post = Some(Status::BUSY));
    streams.post(&job(4, 2, 2));
    run_once(&mut be, 2);
    assert_eq!(dropped(&mut be), 2);
    assert_eq!(hw.with(Mock::live), 2, "only job 2's input and output");
    assert!(matches!(be.health(), Health::Ok));
    hw.with(|m| m.fail_post = None);
    streams.post(&job(5, 2, 2));
    run_once(&mut be, 3);
    assert_eq!(hw.posted().len(), 3);
}

#[test]
fn a_finished_buffer_no_job_had_goes_straight_back_and_reaches_no_one() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    CameraIsp::start(&mut be).unwrap();
    hw.finish(2, 11, 7);
    run_once(&mut be, 1);
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(hw.with(|m| m.given_back.clone()), [2]);
}

#[test]
fn output_1_takes_its_buffer_from_its_own_stream() {
    const RGB_BYTES: u32 = 3840 * 800;
    let streams = Streams {
        output1: Some(handles(2, u64::from(RGB_BYTES))),
        ..Streams::new()
    };
    let (mut be, hw) = setup(&streams);
    let layout = CameraIsp::set_output(
        &mut be,
        1,
        IspOutput {
            width: 1280,
            height: 800,
            fourcc: fourcc(b"RGB3"),
            pad_bytes: 0,
        },
    )
    .unwrap();
    assert_eq!(layout.size, RGB_BYTES);
    let cfg: BeTilesConfig = {
        let mut input = ImageFormatConfig {
            width: 1280,
            height: 800,
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut input, 64);
        let mut b = BackEnd::simple_bayer(
            input,
            Order::Bggr,
            4096,
            (1.5, 1.0, 1.8),
            None,
            formats::NV12,
            Some(CSC),
        );
        let global = b.config().global;
        b.set_global(
            global.bayer_enables,
            global.rgb_enables | rgb_enable::output(1),
            Order::Bggr,
        );
        b.set_output_format(
            Branch::One,
            BeOutputFormatConfig {
                image: ImageFormatConfig {
                    format: formats::RGB888,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        b.prepare().unwrap()
    };
    streams.configure(&cfg);
    CameraIsp::start(&mut be).unwrap();
    streams.post(&IspJob {
        outputs: [3, 1, NO_BUFFER, NO_BUFFER],
        ..job(5, 0, 3)
    });
    run_once(&mut be, 1);
    let posted = hw.posted();
    assert_eq!(posted.len(), 1, "dropped: {}", dropped(&mut be));
    assert_eq!(posted[0].addr[addr_output(1, 0)], REGIONS + 2 * REGION_SPAN);
    hw.finish(0, 5, 4096);
    run_once(&mut be, 2);
    let frame = streams.output1.as_ref().unwrap().take_frame().unwrap();
    assert_eq!(
        (frame.buffer, frame.identity, frame.bytes),
        (1, 5, RGB_BYTES)
    );
    assert_eq!(streams.output0.take_frame().unwrap().buffer, 3);
    assert_eq!(hw.with(Mock::live), 0);
}

#[test]
fn a_post_the_module_refuses_ends_in_recovery_and_recovery_never_gives_up() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| m.fail_post = Some(Status::IO));
    streams.post(&job(1, 0, 0));
    let next = run_once(&mut be, 1000).unwrap();
    assert_eq!(next, 1000 + 250_000_000);
    assert!(matches!(be.health(), Health::Failed(_)));
    assert_eq!(CameraIsp::counters(&mut be).unwrap().errors, 1);
    let before = hw.with(|m| m.stopped);
    assert!(before >= 1, "the stream was ended");

    // While the module can't make a stream: doubling, capped at 8 s, forever.
    hw.with(|m| m.fail_start = Some(Status::IO));
    let mut at = next;
    let mut delays = Vec::new();
    for _ in 0..12 {
        let n = run_once(&mut be, at).unwrap();
        delays.push(n - at);
        at = n;
    }
    assert_eq!(delays[0], 500_000_000);
    assert_eq!(delays[1], 1_000_000_000);
    assert_eq!(delays[2], 2_000_000_000);
    assert_eq!(delays[3], 4_000_000_000);
    assert_eq!(delays[4], 8_000_000_000);
    assert!(delays[5..].iter().all(|&d| d == 8_000_000_000));
    assert!(matches!(be.health(), Health::Failed(_)));

    // It comes back, and the buffers the failed job had are the driver's to name again.
    hw.with(|m| {
        m.fail_start = None;
        m.fail_post = None;
    });
    let before = hw.with(|m| m.plans.len());
    run_once(&mut be, at).unwrap();
    assert_eq!(hw.with(|m| m.plans.len()), before + 1);
    assert!(matches!(be.health(), Health::Ok));
    streams.post(&job(2, 0, 0));
    run_once(&mut be, at + 1);
    assert_eq!(hw.posted().len(), 1);
}

#[test]
fn a_stream_that_ended_is_found_at_the_next_check() {
    let (mut be, hw) = setup(&Streams::new());
    CameraIsp::start(&mut be).unwrap();
    let mut now = 1;
    now = run_once(&mut be, now).unwrap();
    assert!(matches!(be.health(), Health::Ok));
    hw.with(|m| m.fail_check = Some(Status::IO));
    now = run_once(&mut be, 1_000_000_001 + now).unwrap();
    assert!(matches!(be.health(), Health::Failed(_)));
    assert!(now > 1_000_000_001);
}

#[test]
fn stop_ends_recovery() {
    let (mut be, hw) = setup(&Streams::new());
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| m.fail_check = Some(Status::IO));
    run_once(&mut be, 1);
    run_once(&mut be, 2_000_000_000);
    assert!(matches!(be.health(), Health::Failed(_)));
    CameraIsp::stop(&mut be).unwrap();
    assert!(matches!(be.health(), Health::Ok));
    assert_eq!(run_once(&mut be, 3_000_000_000), None);
}

#[test]
fn temporal_denoise_alternates_its_averages_and_resets_when_told() {
    use crate::uapi::bayer_enable as b;

    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    IspPisp::enable_temporal_denoise(&mut be, false).unwrap();
    CameraIsp::start(&mut be).unwrap();
    assert_eq!(hw.with(|m| m.plans[0].tdn_bytes), Some(2560 * 800));
    let first = {
        let mut c = config();
        c.config.global.bayer_enables |= b::TDN | b::TDN_OUTPUT;
        c.config.tdn.reset = 1;
        c.config.tdn_output_format = c.config.input_format;
        c
    };
    let next = {
        let mut c = first;
        c.config.global.bayer_enables |= b::TDN_INPUT;
        c.config.tdn.reset = 0;
        c.config.tdn_input_format = c.config.input_format;
        c
    };
    // Each job finishes before the next, so its buffers are free again.
    let mut identity = 0;
    let mut run = |be: &mut PispBe, cfg: &BeTilesConfig| {
        identity += 1;
        streams.configure(cfg);
        streams.post(&job(identity, 0, 0));
        run_once(be, identity * 2);
        if let Some(k) = hw.posted().last().filter(|k| k.identity == identity) {
            hw.finish(k.buffer, identity, 4096);
        }
        run_once(be, identity * 2 + 1);
        if let Some(frame) = streams.output0.take_frame() {
            streams.output0.return_buffer(frame.buffer);
        }
    };
    // Reading an average before one exists is dropped.
    run(&mut be, &next);
    assert_eq!(hw.posted().len(), 0);
    assert_eq!(dropped(&mut be), 1);

    run(&mut be, &first);
    run(&mut be, &next);
    run(&mut be, &next);
    let posted = hw.posted();
    assert_eq!(posted.len(), 3);
    assert_eq!((posted[0].addr[3], posted[0].addr[5]), (0, TDN[0]));
    assert_eq!((posted[1].addr[3], posted[1].addr[5]), (TDN[0], TDN[1]));
    assert_eq!((posted[2].addr[3], posted[2].addr[5]), (TDN[1], TDN[0]));

    // After a reset, a config that still reads the average is dropped until one resets.
    IspPisp::reset_temporal_denoise(&mut be).unwrap();
    run(&mut be, &next);
    assert_eq!(hw.posted().len(), 3);
    run(&mut be, &first);
    assert_eq!(hw.posted().len(), 4);

    // An average larger than the driver made is refused here, not by the module.
    let mut wide = first;
    wide.config.tdn_output_format.stride *= 2;
    run(&mut be, &wide);
    assert_eq!(hw.posted().len(), 4);
}

#[test]
fn settings_are_refused_while_running_and_when_out_of_range() {
    let (mut be, _hw) = setup(&Streams::new());
    let input = IspInput {
        width: 1280,
        height: 800,
        bit_depth: 12,
        bayer: BayerOrder::default(),
    };
    let output = IspOutput {
        width: 1280,
        height: 800,
        fourcc: fourcc(b"NV12"),
        pad_bytes: 0,
    };
    assert_eq!(
        CameraIsp::set_output(&mut be, 2, output),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        CameraIsp::set_output(&mut be, 1, output),
        Err(Status::NOT_FOUND),
        "output 1 has no stream on this board"
    );
    assert_eq!(
        CameraIsp::set_output(
            &mut be,
            0,
            IspOutput {
                fourcc: fourcc(b"ZZZZ"),
                ..output
            }
        ),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        CameraIsp::set_output(&mut be, 0, IspOutput { width: 0, ..output }),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        CameraIsp::set_input(
            &mut be,
            IspInput {
                width: 70000,
                ..input
            }
        ),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        CameraIsp::set_input(
            &mut be,
            IspInput {
                bit_depth: 7,
                ..input
            }
        ),
        Err(Status::INVALID_ARGUMENT)
    );
    assert!(Options::new(1, 4).is_err());
    assert!(Options::new(33, 4).is_err());
    assert!(Options::new(4, 3).is_err());
    CameraIsp::start(&mut be).unwrap();
    assert_eq!(CameraIsp::start(&mut be), Err(Status::BUSY));
    assert_eq!(CameraIsp::set_input(&mut be, input), Err(Status::BUSY));
    assert_eq!(CameraIsp::set_output(&mut be, 0, output), Err(Status::BUSY));
    assert_eq!(CameraIsp::clear_output(&mut be, 0), Err(Status::BUSY));
    assert_eq!(
        IspPisp::enable_temporal_denoise(&mut be, true),
        Err(Status::BUSY)
    );
}

#[test]
fn it_says_what_it_is_and_refuses_what_it_is_not() {
    let (mut be, _hw) = setup(&Streams::new());
    assert_eq!(CameraIsp::info(&mut be).unwrap().max_outputs, 1);
    assert_eq!(CameraIsp::config_layout(&mut be, 0).unwrap().bytes, 16720);
    assert_eq!(CameraIsp::config_layout(&mut be, 1), Err(Status::NOT_FOUND));
    assert_eq!(IspPisp::info(&mut be).unwrap().hw_version, 0x0225_2700);
    assert_eq!(IspPisp::info(&mut be).unwrap().queue_depth, 4);
    assert_eq!(
        IspPisp::set_front_end_compression(&mut be, 2, true),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        IspPisp::set_front_end_compression(&mut be, 0, true),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        IspPisp::set_line_event(&mut be, 1),
        Err(Status::NOT_SUPPORTED)
    );
    let mut name = [0; 64];
    let n = CameraIsp::config_stream(&mut be, &mut name).unwrap();
    assert_eq!(&name[..n], b"config");
    let n = CameraIsp::input_stream(&mut be, &mut name).unwrap();
    assert_eq!(&name[..n], b"input");
    let n = CameraIsp::output_stream(&mut be, 0, &mut name).unwrap();
    assert_eq!(&name[..n], b"output0");
    assert_eq!(
        CameraIsp::output_stream(&mut be, 1, &mut name),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        CameraIsp::statistics_stream(&mut be, &mut name),
        Err(Status::NOT_SUPPORTED)
    );

    let (mut be, _hw) = setup(&Streams {
        output1: Some(MockStream::new(2)),
        ..Streams::new()
    });
    assert_eq!(CameraIsp::info(&mut be).unwrap().max_outputs, 2);
    let n = CameraIsp::output_stream(&mut be, 1, &mut name).unwrap();
    assert_eq!(&name[..n], b"output1");
}

#[test]
fn backoff_doubles_to_eight_seconds() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(6), 8_000_000_000);
    assert_eq!(backoff_ns(u32::MAX), 8_000_000_000);
}

#[test]
fn the_steady_state_allocates_nothing() {
    let streams = Streams::new();
    let (mut be, _hw) = setup(&streams);
    CameraIsp::start(&mut be).unwrap();
    run_once(&mut be, 1);
    let ((), allocated) = allocations(|| {
        for now in 2..50 {
            run_once(&mut be, now);
        }
    });
    assert_eq!(allocated, 0, "a tick with nothing to do");
}

#[test]
fn the_layouts_are_what_set_input_and_set_output_programmed() {
    let streams = Streams::new();
    let (mut be, _hw) = setup(&streams);
    let raw = CameraIsp::input_layout(&mut be)
        .unwrap()
        .expect("set by set_input");
    assert_eq!((raw.stride, u64::from(raw.size)), (2560, INPUT_BYTES));
    let out = CameraIsp::output_layout(&mut be, 0)
        .unwrap()
        .expect("set by set_output");
    assert_eq!((out.stride, u64::from(out.size)), (1280, OUTPUT_BYTES));
    // This device has output 0 only, so output 1 is past the last one.
    assert_eq!(
        CameraIsp::output_layout(&mut be, 1).err(),
        Some(Status::NOT_FOUND)
    );
    CameraIsp::clear_output(&mut be, 0).unwrap();
    assert_eq!(
        CameraIsp::output_layout(&mut be, 0).unwrap(),
        None,
        "cleared"
    );
}

#[test]
fn a_job_naming_a_config_buffer_an_accepted_job_holds_is_dropped_until_that_job_completes() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(1, 2, 3));
    streams.post(&job(2, 0, 1));
    run_once(&mut be, 1);
    run_once(&mut be, 2);
    assert_eq!(
        hw.with(|m| m.posted.len()),
        1,
        "the second names the buffer the first holds"
    );
    assert_eq!(dropped(&mut be), 1);
    hw.finish(0, 1, 4096);
    run_once(&mut be, 3);
    streams.post(&job(3, 0, 1));
    run_once(&mut be, 4);
    assert_eq!(
        hw.with(|m| m.posted.len()),
        2,
        "the buffer is free once its job completed"
    );
    assert_eq!(dropped(&mut be), 1);
}

#[test]
fn a_config_buffer_rewritten_after_its_job_completed_is_used_by_the_next_job() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    let mut first = config();
    first.config.wbg.gain_r = 0x1111;
    streams.configure(&first);
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(1, 2, 3));
    run_once(&mut be, 1);
    hw.finish(0, 1, 4096);
    run_once(&mut be, 2);
    // The consumer reads the completion and gives the output back, then rewrites config 1.
    let frame = streams.output0.take_frame().expect("job 1's frame");
    assert_eq!(frame.identity, 1);
    streams.output0.return_buffer(frame.buffer);
    let mut second = config();
    second.config.wbg.gain_r = 0x2222;
    streams.configure(&second);
    streams.post(&job(2, 0, 1));
    run_once(&mut be, 3);
    let posted = hw.with(|m| m.posted.clone());
    assert_eq!(posted.len(), 2);
    assert_eq!(&posted[0].1[..], &bytes_of(&first)[..]);
    assert_eq!(&posted[1].1[..], &bytes_of(&second)[..]);
}

#[test]
fn a_write_to_a_config_buffer_an_accepted_job_holds_does_not_change_that_job() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    let mut first = config();
    first.config.wbg.gain_r = 0x1111;
    streams.configure(&first);
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(1, 2, 3));
    run_once(&mut be, 1);
    assert_eq!(hw.with(|m| m.posted.len()), 1);
    let mut second = config();
    second.config.wbg.gain_r = 0x2222;
    streams.configure(&second);
    assert_eq!(
        &streams.config.contents(1)[..CONFIG_BYTES],
        &bytes_of(&second)[..],
        "the consumer's write landed while the job holds the buffer"
    );
    assert_eq!(
        &hw.with(|m| m.posted[0].1.clone())[..],
        &bytes_of(&first)[..],
        "the posted configuration is the one present at accept"
    );
}

#[test]
fn stop_gives_every_config_buffer_back_and_each_one_runs_its_rewrite_after_restart() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    let mut first = config();
    first.config.wbg.gain_r = 0x1111;
    for buffer in 0..4 {
        streams.config.fill(buffer, &bytes_of(&first));
    }
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(1, 2, 3));
    run_once(&mut be, 1);
    assert_eq!(hw.with(|m| m.posted.len()), 1, "job 1 is outstanding");
    CameraIsp::stop(&mut be).unwrap();
    assert!(
        streams.output0.take_frame().is_none(),
        "stop publishes nothing for the outstanding job"
    );
    assert_eq!(CameraIsp::counters(&mut be).unwrap().jobs, 0);

    let mut second = config();
    second.config.wbg.gain_r = 0x2222;
    for buffer in 0..4 {
        streams.config.fill(buffer, &bytes_of(&second));
    }
    CameraIsp::start(&mut be).unwrap();
    for c in 0..4u32 {
        streams.post(&job_on(10 + u64::from(c), c, c, c));
    }
    run_once(&mut be, 2);
    let posted = hw.with(|m| m.posted.clone());
    assert_eq!(
        posted.len(),
        5,
        "one job per config buffer after the restart"
    );
    for (_, sent) in &posted[1..] {
        assert_eq!(&sent[..], &bytes_of(&second)[..]);
    }
    assert_eq!(dropped(&mut be), 0);
}

/// A job that names a lent input (no buffer of its own) is refused with `not-lent`, and its
/// consumer is told by the job's end, on the output it posted for.
#[test]
fn a_lent_job_is_refused_not_lent_and_its_consumer_is_told() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&IspJob {
        input_lent: true,
        lent_buffer: 2,
        ..job_on(9, 1, NO_BUFFER, 0)
    });
    let now = run_once(&mut be, 1).unwrap();
    run_once(&mut be, now);
    assert_eq!(
        streams.output0.take_job_end(),
        Some((9, lemnos_wire::ring::reason::NOT_LENT, 0))
    );
    assert_eq!(dropped(&mut be), 1);
    assert_eq!(
        hw.with(|m| m.posted.len()),
        0,
        "a refused job is never posted"
    );
}

/// A wedged block keeps the buffers its jobs held, and its health says that only a board restart
/// frees them, with the front-end buffers it holds.
#[test]
fn a_wedged_block_keeps_its_buffers_and_its_health_says_so() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| m.wedged_on_stop = true);
    be.stop_block();
    match be.health() {
        Health::Failed(text) => assert_eq!(
            text,
            "the back end is wedged; 0 front-end buffers stay with it until the board restarts"
        ),
        other => panic!("a wedged block is failed, not {other:?}"),
    }
}

/// A job in flight that holds a front-end buffer as its lent input, with no image of its own.
fn lent_flight(config: u32) -> super::InFlight {
    super::InFlight {
        input: super::Lent {
            buffer: 0,
            region: 0,
            bytes: 0,
        },
        outputs: [None; super::BE_NUM_OUTPUTS],
        config,
        lent: Some(super::LentJob {
            buffer: 0,
            identity: 0,
        }),
    }
}

fn wedged_health(be: &PispBe) -> &'static str {
    match be.health() {
        Health::Failed(text) => text,
        other => panic!("a wedged block is failed, not {other:?}"),
    }
}

/// The wedge text counts the front-end buffers in flight when the block wedged: two lent jobs in
/// flight give 2, and a job that holds no front-end buffer is not counted.
#[test]
fn a_wedge_with_jobs_in_flight_counts_the_front_end_buffers_they_hold() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    be.jobs[0] = Some(lent_flight(1));
    be.jobs[1] = Some(lent_flight(1));
    be.jobs[2] = Some(super::InFlight {
        lent: None,
        ..lent_flight(1)
    });
    hw.with(|m| m.wedged_on_stop = true);
    be.stop_block();
    assert_eq!(
        wedged_health(&be),
        "the back end is wedged; 2 front-end buffers stay with it until the board restarts"
    );
}

/// The table covers 0 to 8 exactly. From 9 the text says `9 or more`, and never a count it does
/// not know.
#[test]
fn a_wedge_past_the_table_says_nine_or_more_not_a_wrong_number() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    for at in 0..12 {
        be.jobs[at] = Some(lent_flight(1));
    }
    hw.with(|m| m.wedged_on_stop = true);
    be.stop_block();
    assert_eq!(
        wedged_health(&be),
        "the back end is wedged; 9 or more front-end buffers stay with it until the board restarts"
    );
}

/// A job whose input is no buffer at all, and not lent, is refused for its configuration:
/// `bad-config`, told to its consumer.
#[test]
fn a_job_with_no_input_buffer_is_refused_bad_config_and_its_consumer_is_told() {
    let streams = Streams::new();
    let (mut be, hw) = setup(&streams);
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job_on(5, 1, NO_BUFFER, 0));
    let now = run_once(&mut be, 1).unwrap();
    run_once(&mut be, now);
    assert_eq!(
        streams.output0.take_job_end(),
        Some((5, lemnos_wire::ring::reason::BAD_CONFIG, 0))
    );
    assert_eq!(hw.with(|m| m.posted.len()), 0);
}

/// A stand-in lend source with the host's rules: a published frame is lent by its identity, a second
/// lend of a lent buffer is busy, and an unlend of a buffer not lent is refused. It is a stand-in: the
/// host's `LendView` is tested in `engine/host/tests/stream_lend.rs`, and the real dma-buf mapping in the
/// QEMU check (3b).
#[derive(Clone)]
struct LendMock(Arc<Mutex<LendState>>);

struct LendState {
    set: Arc<[std::os::fd::OwnedFd]>,
    published: Vec<Option<(u64, u32)>>,
    lent: Vec<Option<u64>>,
    unlends: u32,
}

impl LendMock {
    fn new(buffers: usize) -> Self {
        let set: Arc<[std::os::fd::OwnedFd]> = (0..buffers)
            .map(|_| sealed_memfd(c"lend-stand-in", INPUT_BYTES).unwrap())
            .collect::<Vec<_>>()
            .into();
        Self(Arc::new(Mutex::new(LendState {
            set,
            published: vec![None; buffers],
            lent: vec![None; buffers],
            unlends: 0,
        })))
    }

    fn publish(&self, buffer: usize, identity: u64, bytes: u32) {
        self.0.lock().unwrap().published[buffer] = Some((identity, bytes));
    }

    fn lent(&self, buffer: usize) -> Option<u64> {
        self.0.lock().unwrap().lent[buffer]
    }

    fn unlends(&self) -> u32 {
        self.0.lock().unwrap().unlends
    }
}

impl lemnos_hal::LendSource for LendMock {
    type Error = lemnos_hal::ErrorKind;

    fn lend(
        &mut self,
        buffer: u32,
        identity: u64,
    ) -> core::result::Result<lemnos_hal::Lend<'static>, lemnos_hal::ErrorKind> {
        let mut s = self.0.lock().unwrap();
        let at = buffer as usize;
        let Some((published, bytes)) = s.published.get(at).copied().flatten() else {
            return Ok(lemnos_hal::Lend::Busy);
        };
        if s.lent[at].is_some() {
            return Ok(lemnos_hal::Lend::Busy);
        }
        if published != identity {
            return Ok(lemnos_hal::Lend::IdentityMismatch);
        }
        let handle = lemnos_hal::blocks::BufferHandle::shared(s.set.clone(), at)
            .ok_or(lemnos_hal::ErrorKind::InvalidInput)?;
        s.lent[at] = Some(identity);
        Ok(lemnos_hal::Lend::Lent(lemnos_hal::LentBuffer {
            handle,
            bytes,
            generation: 1,
        }))
    }

    fn unlend(
        &mut self,
        buffer: u32,
        identity: u64,
    ) -> core::result::Result<(), lemnos_hal::ErrorKind> {
        let mut s = self.0.lock().unwrap();
        let at = buffer as usize;
        if s.lent.get(at).copied().flatten() != Some(identity) {
            return Err(lemnos_hal::ErrorKind::InvalidInput);
        }
        s.lent[at] = None;
        s.unlends += 1;
        Ok(())
    }
}

/// A driver with `lend` as its lend source, started, and the streams it reads.
fn setup_lent(lend: &LendMock) -> (PispBe, Hw, Streams) {
    let streams = Streams::new();
    streams.configure(&config());
    let mut needs = streams.needs();
    needs.lend = Some(lemnos_core::Held::boxed(Box::new(lend.clone())));
    let (mut be, hw) = setup_needs(needs, Options::new(4, 4).unwrap());
    CameraIsp::start(&mut be).unwrap();
    (be, hw, streams)
}

/// A job whose input is the front-end buffer `buffer` lent for `identity`.
fn lent_job(identity: u64, buffer: u32) -> IspJob {
    IspJob {
        input_lent: true,
        lent_buffer: buffer,
        ..job_on(identity, 1, NO_BUFFER, 0)
    }
}

/// A lent input is read in place, and given back when its job finishes: the output is published and
/// the front end's buffer is its own again.
#[test]
fn a_lent_input_is_read_in_place_and_given_back_when_its_job_finishes() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let (mut be, hw, streams) = setup_lent(&lend);
    streams.post(&lent_job(9, 2));
    let now = run_once(&mut be, 1).unwrap();
    run_once(&mut be, now);
    assert_eq!(hw.posted().len(), 1, "the lent job is posted");
    assert_eq!(
        lend.lent(2),
        Some(9),
        "the buffer is lent while the job runs"
    );
    hw.finish(0, 9, 4096);
    run_once(&mut be, 2);
    assert_eq!(lend.lent(2), None, "finished: given back");
    assert_eq!(lend.unlends(), 1);
    assert_eq!(hw.with(Mock::live), 0, "its region is let go");
    let frame = streams
        .output0
        .take_frame()
        .expect("the output is published");
    assert_eq!((frame.identity, u64::from(frame.bytes)), (9, OUTPUT_BYTES));
}

/// A lent buffer holding another frame is refused `identity-mismatch`, and nothing stays lent.
#[test]
fn a_lent_buffer_of_another_identity_is_refused_identity_mismatch() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let (mut be, hw, streams) = setup_lent(&lend);
    streams.post(&lent_job(8, 2));
    run_once(&mut be, 1);
    assert_eq!(
        streams.output0.take_job_end(),
        Some((8, lemnos_wire::ring::reason::IDENTITY_MISMATCH, 0))
    );
    assert_eq!(hw.posted().len(), 0, "a refused job is never posted");
    assert_eq!(lend.lent(2), None);
}

/// A buffer already lent to another block is `busy`, and the job is not posted.
#[test]
fn a_lent_buffer_already_lent_is_refused_busy() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let (mut be, hw, streams) = setup_lent(&lend);
    let mut other = lend.clone();
    lemnos_hal::LendSource::lend(&mut other, 2, 9).unwrap();
    streams.post(&lent_job(9, 2));
    run_once(&mut be, 1);
    assert_eq!(
        streams.output0.take_job_end(),
        Some((9, lemnos_wire::ring::reason::BUSY, 0))
    );
    assert_eq!(hw.posted().len(), 0);
}

/// A frame too small for the image the configuration reads is `size-or-stride`, and the lend is given
/// back at once.
#[test]
fn a_lent_frame_too_small_for_its_image_is_refused_size_or_stride_and_given_back() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, 1);
    let (mut be, hw, streams) = setup_lent(&lend);
    streams.post(&lent_job(9, 2));
    run_once(&mut be, 1);
    assert_eq!(
        streams.output0.take_job_end(),
        Some((9, lemnos_wire::ring::reason::SIZE_OR_STRIDE, 0))
    );
    assert_eq!(hw.posted().len(), 0);
    assert_eq!(lend.lent(2), None);
    assert_eq!(lend.unlends(), 1);
}

/// A stop of a block that is not wedged gives back the lends of the jobs in flight.
#[test]
fn a_stop_gives_back_the_lends_of_the_jobs_in_flight() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let (mut be, _hw, streams) = setup_lent(&lend);
    streams.post(&lent_job(9, 2));
    let now = run_once(&mut be, 1).unwrap();
    run_once(&mut be, now);
    assert_eq!(lend.lent(2), Some(9));
    be.stop_block();
    assert_eq!(lend.lent(2), None, "stopped: given back");
    assert_eq!(lend.unlends(), 1);
}

/// A wedged block keeps the lends of its jobs: the front-end buffers stay with it.
#[test]
fn a_wedged_stop_keeps_the_lends_of_its_jobs_in_flight() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let (mut be, hw, streams) = setup_lent(&lend);
    streams.post(&lent_job(9, 2));
    let now = run_once(&mut be, 1).unwrap();
    run_once(&mut be, now);
    hw.with(|m| m.wedged_on_stop = true);
    be.stop_block();
    assert_eq!(
        lend.lent(2),
        Some(9),
        "wedged: the buffer stays with the block"
    );
    assert_eq!(lend.unlends(), 0);
    assert!(wedged_health(&be).contains("1 front-end buffer"));
}

/// A board with no `input` stream: the block reads a lent front-end buffer for every job, and has no
/// input stream to read a buffer of its own from.
#[test]
fn a_block_with_no_input_stream_runs_lent_jobs_and_refuses_one_that_names_its_own_input() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let streams = Streams::new();
    streams.configure(&config());
    let mut needs = streams.needs();
    needs.input = None;
    needs.lend = Some(lemnos_core::Held::boxed(Box::new(lend.clone())));
    let (mut be, hw) = setup_needs(needs, Options::new(4, 4).unwrap());
    CameraIsp::start(&mut be).unwrap();

    // Its own input buffer 0 is refused: there is no input stream to hold it.
    streams.post(&job(7, 0, 0));
    let now = run_once(&mut be, 1).unwrap();
    run_once(&mut be, now);
    assert!(
        hw.posted().is_empty(),
        "a job naming an input buffer is not posted"
    );

    streams.post(&lent_job(9, 2));
    let now = run_once(&mut be, 3).unwrap();
    run_once(&mut be, now);
    assert_eq!(hw.posted().len(), 1, "the lent job is posted");
    assert_eq!(lend.lent(2), Some(9));
}

/// A lent buffer the block cannot register (the kernel's EINVAL for a file that is not a dma-buf) is
/// a job refused `bad-config`, as its consumer is told: nothing stays registered or lent, and the
/// block goes on to the next job.
#[test]
fn a_lent_buffer_the_block_cannot_register_is_refused_bad_config_and_nothing_stays_lent() {
    let lend = LendMock::new(4);
    lend.publish(2, 9, INPUT_BYTES as u32);
    let (mut be, hw, streams) = setup_lent(&lend);
    hw.with(|m| m.refuse_lent = Some(Status::INVALID_ARGUMENT));
    streams.post(&lent_job(9, 2));
    run_once(&mut be, 1);
    assert_eq!(
        streams.output0.take_job_end(),
        Some((9, lemnos_wire::ring::reason::BAD_CONFIG, 0))
    );
    assert_eq!(hw.posted().len(), 0);
    assert_eq!(lend.lent(2), None);
    assert_eq!(lend.unlends(), 1);
    assert!(hw.with(|m| m.regions.iter().all(|r| r.is_none())));
}
