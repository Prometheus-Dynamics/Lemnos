//! The driver against a stand-in for the block: every expected value here is written out by hand.

use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, NO_BUFFER,
};
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_FRONT_END};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, Writer};
use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use super::{Link, Options, PispFe, backoff_ns};
use crate::fe::FrontEnd;
use crate::fe_hw::{Finished, Hardware, Placed, Plan};
use crate::fe_job::{CONFIG_BYTES, STATS_BYTES};
use crate::format::formats;
use crate::uapi::{Branch, FeConfig, ImageFormatConfig, Wire, fe_enable as en};

const CONFIG: u64 = 0x1000_0000;
const STATS: u64 = 0x2000_0000;
const OUTPUT1: u64 = 0x3000_0000;
const STRIDE: u32 = 1280;
const FRAME: u32 = STRIDE * 480;

struct Mock {
    plans: Vec<Plan>,
    started: Vec<Start>,
    stopped: u32,
    fail_make: Option<Status>,
    fail_check: Option<Status>,
    fail_post: Option<Status>,
    config: Box<[u8; CONFIG_BYTES]>,
    posted: Vec<Job>,
    finished: Vec<Finished>,
    given_back: Vec<u32>,
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

    fn set_config(&self, cfg: &FeConfig) {
        self.with(|m| {
            cfg.write_to(&mut m.config[..]).unwrap();
        });
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
                Ok(Placed {
                    config: CONFIG,
                    stats: STATS,
                    output1: plan.output1_bytes.map(|_| OUTPUT1),
                })
            }
        })
    }

    fn go(&mut self, start: &Start) -> Result<()> {
        self.with(|m| m.started.push(*start));
        Ok(())
    }

    fn stop(&mut self) {
        self.with(|m| m.stopped += 1);
    }

    fn read_config(&mut self, _slot: u32, out: &mut [u8; CONFIG_BYTES]) -> Result<()> {
        self.with(|m| out.copy_from_slice(&m.config[..]));
        Ok(())
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

fn options() -> Options {
    let link = Link {
        lanes: 2,
        mbps: 800,
        vc: 0,
        data_type: 0x2c,
    };
    Options::new(link, 4, 4, "cfg", "st", ["a", "b"]).unwrap()
}

fn image() -> IspOutput {
    IspOutput {
        width: 640,
        height: 480,
        fourcc: u32::from_le_bytes(*b"RG16"),
        pad_bytes: 0,
    }
}

fn setup(jobs: Option<&MockStream>, frames: Option<&MockStream>) -> (PispFe, Hw) {
    let hw = Hw::default();
    let hold = |s: &MockStream| Box::new(s.clone()) as lemnos_core::StreamHandle;
    let mut fe = PispFe::with_hardware(
        Box::new(hw.clone()),
        jobs.map(hold),
        frames.map(hold),
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
    (fe, hw)
}

fn job(identity: u64, slot: u32, output0: u32, output1: u32) -> IspJob {
    IspJob {
        identity,
        layout: LAYOUT_FRONT_END,
        config_buffer: slot,
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

#[test]
fn a_job_names_its_slots_and_the_first_one_starts_the_block() {
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), None);
    CameraIsp::set_output(&mut fe, 3, image()).unwrap();
    hw.set_config(&config(en::AWB_STATS | en::output(1)));
    CameraIsp::start(&mut fe).unwrap();
    assert_eq!(
        hw.with(|m| m.plans[0]),
        Plan {
            buffers: 4,
            job_slots: 4,
            output0_bytes: FRAME,
            output1_bytes: Some(FRAME),
        }
    );
    // Nothing starts without a job: the module checks the first before writing a register.
    fe.tick(1);
    assert!(hw.with(|m| m.started.is_empty()));

    post(&jobs, &job(0xabc, 1, 3, 2));
    fe.tick(2);
    post(&jobs, &job(0xabd, 2, 0, 1));
    fe.tick(3);
    let (posted, started) = hw.with(|m| (m.posted.clone(), m.started.clone()));
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
            Range {
                addr: CONFIG + CONFIG_BYTES as u64,
                len: CONFIG_BYTES as u64
            },
            Range {
                addr: STATS + STATS_BYTES as u64,
                len: STATS_BYTES as u64
            },
            Range {
                addr: OUTPUT1 + 2 * u64::from(FRAME),
                len: u64::from(FRAME)
            },
        ]
    );
    assert_eq!(posted[1].range[0].addr, CONFIG + 2 * CONFIG_BYTES as u64);
}

#[test]
fn a_job_with_no_statistics_or_second_output_names_only_its_configuration() {
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), None);
    hw.set_config(&config(0));
    CameraIsp::start(&mut fe).unwrap();
    post(&jobs, &job(1, 0, 0, NO_BUFFER));
    fe.tick(1);
    let posted = hw.with(|m| m.posted[0]);
    assert_eq!(posted.count, 1);
    assert_eq!(posted.range[1], Range::default());
}

#[test]
fn jobs_the_driver_can_see_are_wrong_are_dropped_and_not_posted() {
    let jobs = MockStream::new(16);
    let (mut fe, hw) = setup(Some(&jobs), None);
    hw.set_config(&config(0));
    CameraIsp::start(&mut fe).unwrap();
    let bad = [
        IspJob {
            layout: 1,
            ..job(1, 0, 0, NO_BUFFER)
        },
        IspJob {
            input_buffer: 0,
            ..job(1, 0, 0, NO_BUFFER)
        },
        job(1, 4, 0, NO_BUFFER),
        job(1, 0, 4, NO_BUFFER),
        job(1, 0, 0, 0),
    ];
    for j in &bad {
        post(&jobs, j);
    }
    fe.tick(1);
    assert!(hw.with(|m| m.posted.is_empty()));
    assert_eq!(fe.counters.dropped, bad.len() as u64);
    assert!(matches!(fe.health(), Health::Ok));

    let mut wrong = config(0);
    wrong.stats_buffer.addr_lo = 16;
    hw.set_config(&wrong);
    post(&jobs, &job(1, 0, 0, NO_BUFFER));
    fe.tick(2);
    assert!(
        hw.with(|m| m.posted.is_empty()),
        "an address in the configuration"
    );
    assert_eq!(fe.counters.dropped, bad.len() as u64 + 1);
}

#[test]
fn a_refused_job_ends_the_stream_and_everything_is_made_again() {
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), None);
    hw.set_config(&config(0));
    CameraIsp::start(&mut fe).unwrap();
    hw.with(|m| m.fail_post = Some(Status::INVALID_ARGUMENT));
    post(&jobs, &job(1, 0, 0, NO_BUFFER));
    assert_eq!(fe.tick(10), Some(10 + backoff_ns(0)));
    assert!(matches!(fe.health(), Health::Degraded(_)));
    assert_eq!(
        hw.with(|m| m.stopped),
        2,
        "begin stopped once, the failure once"
    );
    hw.with(|m| m.fail_post = None);
    fe.tick(10 + backoff_ns(0));
    assert!(matches!(fe.health(), Health::Ok));
    assert_eq!(hw.with(|m| m.plans.len()), 2);
    assert!(
        !fe.started,
        "a new stream's first job starts the block again"
    );
}

#[test]
fn recovery_never_stops_and_backs_off_to_eight_seconds() {
    let (mut fe, hw) = setup(None, None);
    CameraIsp::start(&mut fe).unwrap();
    hw.with(|m| {
        m.fail_check = Some(Status::IO);
        m.fail_make = Some(Status::NO_MEMORY);
    });
    fe.tick(0);
    let mut now = 1_000_000_000;
    let mut due = fe.tick(now).unwrap();
    assert_eq!(
        due - now,
        backoff_ns(0),
        "the check that fails, after a first look"
    );
    let mut last = 0;
    for _ in 0..100 {
        now = due;
        due = fe.tick(now).unwrap();
        last = due - now;
        assert!(last <= 8_000_000_000);
    }
    assert_eq!(last, 8_000_000_000);
    hw.with(|m| m.fail_make = None);
    fe.tick(due);
    assert!(matches!(fe.health(), Health::Ok));
}

#[test]
fn stop_ends_recovery_and_only_the_front_ends_outputs_exist() {
    let (mut fe, hw) = setup(None, None);
    assert_eq!(
        CameraIsp::set_output(&mut fe, 0, image()),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        CameraIsp::set_output(
            &mut fe,
            2,
            IspOutput {
                fourcc: u32::from_le_bytes(*b"NV12"),
                ..image()
            }
        ),
        Err(Status::INVALID_ARGUMENT)
    );
    CameraIsp::start(&mut fe).unwrap();
    assert_eq!(
        CameraIsp::set_output(&mut fe, 2, image()),
        Err(Status::BUSY)
    );
    hw.with(|m| m.fail_check = Some(Status::IO));
    fe.tick(1);
    fe.tick(1 + 1_000_000_000);
    assert!(matches!(fe.health(), Health::Degraded(_)));
    CameraIsp::stop(&mut fe).unwrap();
    assert_eq!(fe.tick(1 << 40), None);
    assert!(matches!(fe.health(), Health::Ok));
}

#[test]
fn compression_changes_the_layout_and_is_stopped_only() {
    let (mut fe, _hw) = setup(None, None);
    IspPisp::set_front_end_compression(&mut fe, 2, true).unwrap();
    assert_eq!(fe.outputs[0].unwrap().format, formats::PISP_COMP1);
    assert_eq!(fe.outputs[0].unwrap().layout.stride, 640);
    assert_eq!(
        IspPisp::set_front_end_compression(&mut fe, 1, true),
        Err(Status::NOT_FOUND)
    );
    CameraIsp::start(&mut fe).unwrap();
    assert_eq!(
        IspPisp::set_front_end_compression(&mut fe, 2, false),
        Err(Status::BUSY)
    );
}

#[test]
fn only_a_published_buffer_can_come_back() {
    let frames = MockStream::new(4);
    let (mut fe, hw) = setup(None, Some(&frames));
    CameraIsp::start(&mut fe).unwrap();
    hw.with(|m| {
        m.finished.push(Finished {
            buffer: 2,
            bytes: FRAME,
            sequence: 1,
            identity: 7,
            timestamp_ns: 5,
        });
    });
    fe.tick(1);
    assert_eq!(fe.counters.jobs, 1);
    assert_eq!(frames.take_frame().unwrap().identity, 7);
    frames.return_buffer(1);
    frames.return_buffer(2);
    fe.tick(2);
    assert_eq!(hw.with(|m| m.given_back.clone()), [2]);
}

#[test]
fn the_steady_state_allocates_nothing() {
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), None);
    hw.set_config(&config(en::AWB_STATS));
    CameraIsp::start(&mut fe).unwrap();
    fe.tick(1);
    post(&jobs, &job(1, 0, 0, NO_BUFFER));
    fe.tick(2);
    let ((), allocated) = allocations(|| {
        for i in 0..64 {
            fe.tick(3 + i);
            let _ = CameraIsp::counters(&mut fe);
            let _ = fe.health();
        }
    });
    assert_eq!(
        allocated, 0,
        "ticks with no work, counters and the health check"
    );
}
