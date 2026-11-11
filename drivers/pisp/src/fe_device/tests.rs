//! The driver against a stand-in for the block: every expected value here is written out by hand.

use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, NO_BUFFER,
};
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_FRONT_END};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, Writer};
use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::allocations;

use super::{Link, Options, PispFe, backoff_ns};
use crate::fe::FrontEnd;
use crate::fe_hw::{Finished, Hardware, Placed, Plan, Set};
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
    /// Buffers given memory on the stream now, in order; cleared when a stream is made.
    set: Vec<u32>,
    /// What the module says to the next memory offered for a buffer.
    answer: Option<(u32, Set)>,
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
            given_back: Vec::with_capacity(4096),
            set: Vec::with_capacity(4096),
            answer: None,
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
                m.set.clear();
                Ok(Placed {
                    config: CONFIG,
                    stats: STATS,
                    output1: plan.output1_bytes.map(|_| OUTPUT1),
                })
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

/// A `frames` stream of `buffers` buffers, each behind a handle as a dma-buf stream's are.
fn stream(buffers: u32) -> MockStream {
    let memory = (0..buffers).map(|_| sealed_memfd(c"frames", 4096).unwrap());
    MockStream::with_handles(memory.collect())
}

/// With no `frames` given, a stream of four buffers with handles.
fn setup(jobs: Option<&MockStream>, frames: Option<&MockStream>) -> (PispFe, Hw) {
    let hw = Hw::default();
    let hold = |s: &MockStream| Box::new(s.clone()) as lemnos_core::StreamHandle;
    let frames = frames.cloned().unwrap_or_else(|| stream(4));
    let mut fe = PispFe::with_hardware(
        Box::new(hw.clone()),
        jobs.map(hold),
        Some(hold(&frames)),
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
            output1_buffers: 4,
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
fn a_finished_job_is_published_and_its_buffer_goes_back_when_returned() {
    let frames = stream(4);
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), Some(&frames));
    hw.set_config(&config(0));
    CameraIsp::start(&mut fe).unwrap();
    assert_eq!(
        hw.with(|m| m.set.clone()),
        [0, 1, 2, 3],
        "every buffer of the stream was given to the module"
    );
    post(&jobs, &job(7, 0, 2, NO_BUFFER));
    fe.tick(1);
    assert_eq!(hw.with(|m| m.posted.len()), 1);
    hw.with(|m| m.finished.push(done(2, 7)));
    fe.tick(2);
    assert_eq!(fe.counters.jobs, 1);
    assert_eq!(frames.take_frame().unwrap().identity, 7);
    assert_eq!(hw.with(|m| m.given_back.clone()), [], "the consumer has it");

    // A job on the buffer the consumer holds isn't sent.
    post(&jobs, &job(8, 0, 2, NO_BUFFER));
    fe.tick(3);
    assert_eq!(hw.with(|m| m.posted.len()), 1);
    assert_eq!(fe.counters.dropped, 1);

    frames.return_buffer(2);
    fe.tick(4);
    assert_eq!(hw.with(|m| m.given_back.clone()), [2]);
    assert_eq!(
        hw.with(|m| m.set.clone()),
        [0, 1, 2, 3, 2],
        "back to the block only after the module was shown the memory behind it again"
    );
    post(&jobs, &job(9, 0, 2, NO_BUFFER));
    fe.tick(5);
    assert_eq!(hw.with(|m| m.posted.len()), 2);
}

#[test]
fn a_second_job_on_a_buffer_a_job_already_has_is_dropped() {
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), None);
    hw.set_config(&config(0));
    CameraIsp::start(&mut fe).unwrap();
    post(&jobs, &job(1, 0, 3, NO_BUFFER));
    post(&jobs, &job(2, 0, 3, NO_BUFFER));
    fe.tick(1);
    assert_eq!(hw.with(|m| m.posted.len()), 1);
    assert_eq!(hw.with(|m| m.posted[0].identity), 1);
    assert_eq!(fe.counters.dropped, 1);
}

#[test]
fn a_job_on_a_buffer_whose_memory_the_module_does_not_have_is_dropped() {
    let frames = stream(4);
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), Some(&frames));
    hw.set_config(&config(0));
    // The module won't take buffer 1's memory yet.
    hw.with(|m| m.answer = Some((1, Set::Busy)));
    CameraIsp::start(&mut fe).unwrap();
    post(&jobs, &job(1, 0, 1, NO_BUFFER));
    fe.tick(1);
    assert!(hw.with(|m| m.posted.is_empty()));
    assert_eq!(fe.counters.dropped, 1);
    post(&jobs, &job(2, 0, 0, NO_BUFFER));
    fe.tick(2);
    assert_eq!(
        hw.with(|m| m.posted.len()),
        1,
        "a buffer it does have is fine"
    );
}

#[test]
fn a_consumer_that_leaves_gets_nothing_more_and_the_next_one_gets_its_own_memory() {
    let frames = stream(4);
    let jobs = MockStream::new(4);
    let (mut fe, hw) = setup(Some(&jobs), Some(&frames));
    hw.set_config(&config(0));
    CameraIsp::start(&mut fe).unwrap();
    post(&jobs, &job(1, 0, 1, NO_BUFFER));
    fe.tick(1);
    assert_eq!(hw.with(|m| m.posted.len()), 1);

    // The consumer goes while the block has buffer 1 for that job.
    frames.change_consumer();
    hw.with(|m| {
        m.set.clear();
        m.answer = Some((1, Set::Busy));
    });
    fe.tick(2);
    assert_eq!(
        hw.with(|m| m.set.clone()),
        [0, 2, 3],
        "every hold was given up and taken again; the module has the new memory of all but 1"
    );
    // The job finishes in the old consumer's memory: not published, and only then swapped.
    hw.with(|m| {
        m.answer = None;
        m.finished.push(done(1, 1));
    });
    fe.tick(3);
    assert!(
        frames.take_frame().is_none(),
        "the new consumer never sees it"
    );
    assert_eq!(fe.counters.dropped, 1);
    assert_eq!(hw.with(|m| m.set.clone()), [0, 2, 3, 1]);
    assert_eq!(hw.with(|m| m.given_back.clone()), [1]);
    assert_eq!(hw.with(|m| m.plans.len()), 1, "the block was never stopped");
}

#[test]
fn a_stream_whose_buffers_have_no_handles_is_refused() {
    // Plain shared memory: there is nothing to give the block.
    let frames = MockStream::with_bytes(4, 4096);
    let (mut fe, hw) = setup(None, Some(&frames));
    assert_eq!(CameraIsp::start(&mut fe), Err(Status::NOT_SUPPORTED));
    assert!(hw.with(|m| m.set.is_empty()));
    assert_eq!(fe.tick(1), None, "not running");
}

#[test]
fn memory_the_module_refuses_is_a_refused_start() {
    let frames = stream(4);
    let (mut fe, hw) = setup(None, Some(&frames));
    hw.with(|m| m.answer = Some((0, Set::Refused)));
    assert_eq!(CameraIsp::start(&mut fe), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn without_a_frames_stream_there_is_nothing_to_start() {
    let hw = Hw::default();
    let mut fe = PispFe::with_hardware(Box::new(hw.clone()), None, None, options());
    let input = IspInput {
        width: 640,
        height: 480,
        bit_depth: 12,
        bayer: BayerOrder::default(),
    };
    CameraIsp::set_input(&mut fe, input).unwrap();
    CameraIsp::set_output(&mut fe, 2, image()).unwrap();
    assert_eq!(CameraIsp::start(&mut fe), Err(Status::NOT_FOUND));
    assert!(
        hw.with(|m| m.plans.is_empty()),
        "the block was never touched"
    );
}

#[test]
fn the_steady_state_allocates_nothing() {
    let jobs = MockStream::new(4);
    let frames = stream(4);
    let (mut fe, hw) = setup(Some(&jobs), Some(&frames));
    hw.set_config(&config(en::AWB_STATS));
    CameraIsp::start(&mut fe).unwrap();
    fe.tick(1);
    post(&jobs, &job(1, 0, 0, NO_BUFFER));
    fe.tick(2);
    // A frame finished, published, read, returned and given back to the block, every round.
    let mut now = 3;
    let mut round = |fe: &mut PispFe| {
        hw.with(|m| {
            m.finished.push(done(0, now));
            m.given_back.clear();
            m.set.clear();
        });
        fe.tick(now);
        assert_eq!(frames.take_frame().map(|f| f.identity), Some(now));
        frames.return_buffer(0);
        fe.tick(now + 1);
        assert!(hw.with(|m| m.given_back == [0]));
        let _ = CameraIsp::counters(fe);
        let _ = fe.health();
        now += 2;
    };
    for _ in 0..16 {
        round(&mut fe);
    }
    let ((), allocated) = allocations(|| {
        for _ in 0..64 {
            round(&mut fe);
        }
    });
    assert_eq!(
        allocated, 0,
        "frames out, buffers back, idle ticks, counters and the health check"
    );
}
