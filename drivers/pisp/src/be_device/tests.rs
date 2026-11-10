//! The driver against a stand-in for the block: every expected value here is written out by hand.

use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_BACK_END};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, Writer};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use super::{Options, PispBe, backoff_ns};
use crate::be::BackEnd;
use crate::be_hw::{Finished, Hardware, Placed, Plan, Registered};
use crate::be_job::{KernelJob, addr_output};
use crate::format::{compute_stride_align, formats};
use crate::uapi::{BayerOrder as Order, BeCcmConfig, BeTilesConfig, ImageFormatConfig, Wire};
use lemnos_driver_pisp_sys::CONFIG_BYTES;

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const POOL: u64 = 0x1000_0000;
const INPUT: u64 = 0x2000_0000;
const OUTPUT1: u64 = 0x3000_0000;
const TDN: [u64; 2] = [0x4000_0000, 0x4100_0000];

struct Mock {
    plans: Vec<Plan>,
    stopped: u32,
    fail_start: Option<Status>,
    fail_check: Option<Status>,
    fail_post: Option<Status>,
    config: Box<[u8; CONFIG_BYTES]>,
    posted: Vec<KernelJob>,
    finished: Vec<Finished>,
    given_back: Vec<u32>,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            plans: Vec::new(),
            stopped: 0,
            fail_start: None,
            fail_check: None,
            fail_post: None,
            config: Box::new([0; CONFIG_BYTES]),
            posted: Vec::with_capacity(64),
            finished: Vec::with_capacity(64),
            given_back: Vec::with_capacity(64),
        }
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Mock>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn set_config(&self, cfg: &BeTilesConfig) {
        self.with(|m| {
            cfg.write_to(&mut m.config[..]).unwrap();
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
                Ok(Placed {
                    pool: POOL,
                    pool_stride: 0x10_0000,
                    input: INPUT,
                    output1: plan.output1_bytes.map(|_| OUTPUT1),
                    tdn: plan.tdn_bytes.map(|_| TDN),
                })
            }
        })
    }

    fn stop(&mut self) {
        self.with(|m| m.stopped += 1);
    }

    fn read_config(&mut self, _slot: u32, out: &mut [u8; CONFIG_BYTES]) -> Result<()> {
        self.with(|m| out.copy_from_slice(&m.config[..]));
        Ok(())
    }

    fn register(&mut self, _memory: std::os::fd::BorrowedFd<'_>) -> Result<Registered> {
        Err(Status::NOT_SUPPORTED)
    }

    fn unregister(&mut self, _region: u32) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    fn post(&mut self, job: &KernelJob) -> Result<()> {
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

fn options() -> Options {
    Options::new(4, 4, "cfg", ["a", "b"]).unwrap()
}

fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

fn setup(jobs: Option<&MockStream>, frames: Option<&MockStream>) -> (PispBe, Hw) {
    let hw = Hw::default();
    let hold = |s: &MockStream| Box::new(s.clone()) as lemnos_core::StreamHandle;
    let mut be = PispBe::with_hardware(
        Box::new(hw.clone()),
        jobs.map(hold),
        frames.map(hold),
        options(),
    );
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
    assert_eq!(layout.size, 1280 * 800 + 1280 * 400);
    (be, hw)
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

fn post(stream: &MockStream, job: &IspJob) {
    let mut buf = [0; 128];
    let mut w = Writer::new(&mut buf);
    job.encode(&mut w).unwrap();
    let n = w.len();
    stream.post_job(&buf[..n]);
}

fn run_once(be: &mut PispBe, now: u64) -> Option<u64> {
    be.tick(now)
}

#[test]
fn a_job_reaches_the_block_at_the_buffers_it_named() {
    let jobs = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), None);
    hw.set_config(&config());
    CameraIsp::start(&mut be).unwrap();
    assert_eq!(
        hw.with(|m| m.plans[0]),
        Plan {
            buffers: 4,
            job_slots: 4,
            input_bytes: 2560 * 800,
            output0_bytes: 1280 * 800 + 1280 * 400,
            output1_bytes: None,
            tdn_bytes: None,
        }
    );
    post(&jobs, &job(0xabc, 2, 3));
    run_once(&mut be, 1);
    let posted = hw.with(|m| m.posted.clone());
    assert_eq!(posted.len(), 1);
    let k = posted[0];
    assert_eq!(k.identity, 0xabc);
    assert_eq!(k.buffer, 3);
    assert_eq!(k.config, 1);
    assert_eq!(k.addr[0], INPUT + 2 * 2560 * 800);
    assert_eq!(k.addr[addr_output(0, 0)], POOL + 3 * 0x10_0000);
    assert_eq!(k.addr[addr_output(0, 1)], POOL + 3 * 0x10_0000 + 1280 * 800);
    assert_eq!(k.addr[addr_output(1, 0)], 0);
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 0);
}

#[test]
fn jobs_the_driver_can_see_are_wrong_are_dropped_and_not_posted() {
    let jobs = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), None);
    hw.set_config(&config());
    CameraIsp::start(&mut be).unwrap();

    let bad = [
        IspJob {
            layout: 99,
            ..job(1, 0, 0)
        },
        IspJob {
            config_buffer: 4,
            ..job(1, 0, 0)
        },
        job(1, 4, 0),
        job(1, 0, 4),
        job(1, NO_BUFFER, 0),
        job(1, 0, NO_BUFFER),
        IspJob {
            outputs: [0, 4, NO_BUFFER, NO_BUFFER],
            ..job(1, 0, 0)
        },
        IspJob {
            outputs: [0, NO_BUFFER, 1, NO_BUFFER],
            ..job(1, 0, 0)
        },
    ];
    for b in &bad {
        post(&jobs, b);
    }
    jobs.post_job(&[1, 2, 3]);
    run_once(&mut be, 1);
    assert!(hw.with(|m| m.posted.is_empty()));
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 9);
    assert!(
        matches!(be.health(), Health::Ok),
        "a caller's mistake is not a failure"
    );
}

#[test]
fn a_config_the_module_would_refuse_is_never_posted() {
    let jobs = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), None);
    let mut cfg = config();
    cfg.config.axi.r_qos = 1;
    hw.set_config(&cfg);
    CameraIsp::start(&mut be).unwrap();
    post(&jobs, &job(1, 0, 0));
    run_once(&mut be, 1);
    assert!(hw.with(|m| m.posted.is_empty()));
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
}

#[test]
fn a_finished_buffer_no_job_was_lent_goes_straight_back_and_reaches_no_one() {
    // What the block wrote into the pool is nobody's: the consumer never sees it. The path
    // through a lent buffer is in `tests/be_frames.rs`, against the host's own stream.
    let frames = MockStream::new(4);
    let (mut be, hw) = setup(None, Some(&frames));
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| {
        m.finished.push(Finished {
            buffer: 2,
            bytes: 7,
            sequence: 5,
            identity: 11,
            timestamp_ns: 13,
        });
    });
    run_once(&mut be, 1);
    assert!(frames.take_frame().is_none());
    assert_eq!(hw.with(|m| m.given_back.clone()), [2]);
}

#[test]
fn with_buffers_that_have_no_handles_a_job_is_dropped_before_the_block() {
    let jobs = MockStream::new(4);
    let frames = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), Some(&frames));
    hw.set_config(&config());
    CameraIsp::start(&mut be).unwrap();
    post(&jobs, &job(1, 0, 2));
    run_once(&mut be, 1);
    assert!(hw.with(|m| m.posted.is_empty()));
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
}

#[test]
fn with_no_frames_stream_a_finished_buffer_goes_straight_back() {
    let (mut be, hw) = setup(None, None);
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| {
        m.finished.push(Finished {
            buffer: 1,
            bytes: 0,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
        });
    });
    run_once(&mut be, 1);
    assert_eq!(hw.with(|m| m.given_back.clone()), [1]);
}

#[test]
fn a_post_the_module_refuses_ends_in_recovery_and_recovery_never_gives_up() {
    let jobs = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), None);
    hw.set_config(&config());
    CameraIsp::start(&mut be).unwrap();
    hw.with(|m| m.fail_post = Some(Status::IO));
    post(&jobs, &job(1, 0, 0));
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

    // It comes back.
    hw.with(|m| m.fail_start = None);
    let before = hw.with(|m| m.plans.len());
    run_once(&mut be, at).unwrap();
    assert_eq!(hw.with(|m| m.plans.len()), before + 1);
    assert!(matches!(be.health(), Health::Ok));
}

#[test]
fn a_stream_that_ended_is_found_at_the_next_check() {
    let (mut be, hw) = setup(None, None);
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
    let (mut be, hw) = setup(None, None);
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

    let jobs = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), None);
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
    // Reading an average before one exists is dropped.
    hw.set_config(&next);
    post(&jobs, &job(1, 0, 0));
    run_once(&mut be, 1);
    assert_eq!(hw.with(|m| m.posted.len()), 0);
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);

    hw.set_config(&first);
    post(&jobs, &job(2, 0, 0));
    run_once(&mut be, 2);
    hw.set_config(&next);
    post(&jobs, &job(3, 0, 0));
    run_once(&mut be, 3);
    post(&jobs, &job(4, 0, 0));
    run_once(&mut be, 4);
    let posted = hw.with(|m| m.posted.clone());
    assert_eq!(posted.len(), 3);
    assert_eq!((posted[0].addr[3], posted[0].addr[5]), (0, TDN[0]));
    assert_eq!((posted[1].addr[3], posted[1].addr[5]), (TDN[0], TDN[1]));
    assert_eq!((posted[2].addr[3], posted[2].addr[5]), (TDN[1], TDN[0]));

    // After a reset, a config that still reads the average is dropped until one resets.
    IspPisp::reset_temporal_denoise(&mut be).unwrap();
    post(&jobs, &job(5, 0, 0));
    run_once(&mut be, 5);
    assert_eq!(hw.with(|m| m.posted.len()), 3);
    hw.set_config(&first);
    post(&jobs, &job(6, 0, 0));
    run_once(&mut be, 6);
    assert_eq!(hw.with(|m| m.posted.len()), 4);
}

#[test]
fn settings_are_refused_while_running_and_when_out_of_range() {
    let (mut be, _hw) = setup(None, None);
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
    let (mut be, _hw) = setup(None, None);
    assert_eq!(CameraIsp::info(&mut be).unwrap().max_outputs, 2);
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
    let n = CameraIsp::output_stream(&mut be, 1, &mut name).unwrap();
    assert_eq!(&name[..n], b"b");
    assert_eq!(
        CameraIsp::output_stream(&mut be, 2, &mut name),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        CameraIsp::statistics_stream(&mut be, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
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
    let jobs = MockStream::new(4);
    let (mut be, hw) = setup(Some(&jobs), None);
    hw.set_config(&config());
    CameraIsp::start(&mut be).unwrap();
    run_once(&mut be, 1);
    let ((), allocated) = allocations(|| {
        for now in 2..50 {
            run_once(&mut be, now);
        }
    });
    assert_eq!(allocated, 0, "a tick with nothing to do");
}
