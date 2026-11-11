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

    fn stop(&mut self) {
        self.with(|m| {
            m.stopped += 1;
            m.regions.clear();
        });
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
        let hold = |s: &MockStream| -> StreamHandle { Box::new(s.clone()) };
        PispBeNeeds {
            config: hold(&self.config),
            input: hold(&self.input),
            output0: hold(&self.output0),
            output1: self.output1.as_ref().map(hold),
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
    let hw = Hw::default();
    let mut be = PispBe::with_hardware(Box::new(hw.clone()), streams.needs(), options);
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
    IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: 1,
        input_buffer: input,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
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
    streams.configure(&config());
    CameraIsp::start(&mut be).unwrap();
    streams.post(&job(1, 0, 0));
    streams.post(&job(2, 1, 1));
    streams.post(&job(3, 2, 2));
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
    assert!(matches!(be.health(), Health::Degraded(_)));
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
    assert!(matches!(be.health(), Health::Degraded(_)));

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
    assert!(matches!(be.health(), Health::Degraded(_)));
    assert!(now > 1_000_000_001);
}

#[test]
fn stop_ends_recovery() {
    let (mut be, hw) = setup(&Streams::new());
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| m.fail_check = Some(Status::IO));
    run_once(&mut be, 1);
    run_once(&mut be, 2_000_000_000);
    assert!(matches!(be.health(), Health::Degraded(_)));
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
