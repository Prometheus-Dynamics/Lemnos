//! The frames driver against a pair of sessions (configuration and frames) that records what it
//! was asked and answers as the module would, and mocked streams whose output buffers are memfds behind handles. No node is
//! opened; every expected value is written out by hand.

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, MAX_STREAM_NAME, NO_BUFFER, OutputLayout,
};
use lemnos_core::{Encode, Health, Lifecycle, Status, Writer};
use lemnos_driver_k230isp_sys::regs::{AE_H_SIZE, AE_V_SIZE, ISP_AWB_GAIN_RB, ISP_CTRL};
use lemnos_driver_regjob_sys::{Addr, Job, STATUS_UNLATCHED, Write};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use crate::driver::backoff_ns;
use crate::frames::{
    FRAME_TIMEOUT_MS, K230IspFrames, K230IspFramesNeeds, MAX_CALLER_WRITES, Module, SETUP_WRITES,
};
use crate::stats::registers;
use crate::{CONFIG_BYTES, LAYOUT_STATISTICS, LAYOUT_WRITES, STATISTICS_BYTES};

const STRIDE: u64 = 1_000_000;
const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
const LUMA: u32 = WIDTH * HEIGHT;
/// NV12: half the luma.
const CHROMA: u32 = LUMA / 2;
const NV12: u32 = u32::from_le_bytes(*b"NV12");
const NV16: u32 = u32::from_le_bytes(*b"NV16");
/// The output buffers.
const OUTPUTS: u32 = 4;
/// What a frame's end reports: the frame ended, nothing wrapped.
const FRAME_END: u32 = 1;
const RGGB_10_BITS: u32 = 1 << 12;
/// The frames role's bits of `isp_ctrl`: enabled, Bayer RGB, the input formatter on.
const CTRL_BASE: u32 = 1 | 3 << 1 | 1 << 4;
const GEN_CFG_UPD: u32 = 1 << 10;
const GAMMA_OUT: u32 = 1 << 11;

/// A job as the session was given it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ran {
    writes: Vec<Write>,
    addrs: Vec<Addr>,
    reads: Vec<u32>,
    start: u32,
    timeout_ms: u32,
}

#[derive(Default)]
struct Log {
    opens: u32,
    closes: u32,
    fail_open: bool,
    /// The memory each import named, as (device, inode).
    imports: Vec<(u64, u64)>,
    releases: Vec<u32>,
    next_index: u32,
    /// Kept while set: the steady-state check runs with it off, so the log allocates nothing.
    record: bool,
    jobs: Vec<Ran>,
    /// Each configuration written, in order.
    configs: Vec<Vec<Write>>,
    /// What the module answers the next configuration with, when not a write.
    configure_answer: Option<i32>,
    starts: u32,
    /// What the module answers the next start with, when not a start.
    start_answer: Option<i32>,
    /// What the module answers collects with, in order; a job runs while it is empty.
    ends: VecDeque<io::Result<u32>>,
    /// The value each statistics register reads as: this plus its index.
    seed: u32,
    /// A job started and not yet collected or ended by a close.
    running: bool,
}

#[derive(Clone, Default)]
struct Played(Arc<Mutex<Log>>);

impl Played {
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap()
    }

    fn end(&self, end: io::Result<u32>) {
        self.log().ends.push_back(end);
    }
}

struct Session(Played);

impl Module for Session {
    fn open(&mut self) -> io::Result<()> {
        let mut log = self.0.log();
        if log.fail_open {
            return Err(io::Error::from_raw_os_error(libc::ENODEV));
        }
        log.opens += 1;
        // Each session numbers its buffers from 0.
        log.next_index = 0;
        Ok(())
    }

    fn close(&mut self) {
        let mut log = self.0.log();
        log.closes += 1;
        log.running = false;
    }

    fn configure(&mut self, writes: &[Write]) -> io::Result<()> {
        let mut log = self.0.log();
        if log.record {
            log.configs.push(writes.to_vec());
        }
        match log.configure_answer.take() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(()),
        }
    }

    fn import(&mut self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        let memory = lemnos_linux_sys::dma_buf::identity(buffer)?;
        assert!(buffer.as_raw_fd() >= 0);
        let mut log = self.0.log();
        log.imports.push(memory);
        log.next_index += 1;
        Ok(log.next_index - 1)
    }

    fn release(&mut self, index: u32) -> io::Result<()> {
        self.0.log().releases.push(index);
        Ok(())
    }

    fn start(&mut self, job: &Job<'_>) -> io::Result<()> {
        let mut log = self.0.log();
        assert!(!log.running, "a job started over a running one");
        log.starts += 1;
        if log.record {
            log.jobs.push(Ran {
                writes: job.writes.to_vec(),
                addrs: job.addrs.to_vec(),
                reads: job.reads.to_vec(),
                start: job.start,
                timeout_ms: job.timeout_ms,
            });
        }
        if let Some(errno) = log.start_answer.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        log.running = true;
        Ok(())
    }

    fn collect(&mut self, values: &mut [u32]) -> io::Result<Option<u32>> {
        let mut log = self.0.log();
        assert!(log.running, "a collect with no job started");
        let Some(end) = log.ends.pop_front() else {
            return Ok(None);
        };
        log.running = false;
        let status = end?;
        for (i, value) in values.iter_mut().enumerate() {
            *value = log.seed.wrapping_add(i as u32);
        }
        Ok(Some(status))
    }
}

fn buffer(len: u64) -> OwnedFd {
    lemnos_linux_sys::dma_buf::sealed_memfd(c"k230isp-frames-test", len).unwrap()
}

struct Rig {
    d: K230IspFrames,
    config: MockStream,
    statistics: MockStream,
    output: MockStream,
    played: Played,
    now: u64,
}

impl Rig {
    /// A driver, not started.
    fn new() -> Self {
        let config = MockStream::with_bytes(4, CONFIG_BYTES);
        let statistics = MockStream::with_bytes(4, STATISTICS_BYTES);
        let output = MockStream::with_handles(
            (0..OUTPUTS)
                .map(|_| buffer(u64::from(LUMA + CHROMA)))
                .collect(),
        );
        let played = Played::default();
        played.log().record = true;
        let needs = K230IspFramesNeeds {
            config: lemnos_core::Held::boxed(Box::new(config.clone())),
            statistics: lemnos_core::Held::boxed(Box::new(statistics.clone())),
            output0: lemnos_core::Held::boxed(Box::new(output.clone())),
        };
        let d = K230IspFrames::with_module(Box::new(Session(played.clone())), needs);
        Self {
            d,
            config,
            statistics,
            output,
            played,
            now: 0,
        }
    }

    fn configure(&mut self) {
        self.d.set_input(input()).unwrap();
        let layout = self.d.set_output(0, image(NV12)).unwrap();
        assert_eq!((layout.stride, layout.size), (WIDTH, LUMA + CHROMA));
    }

    /// Configured, started, and the streams' buffers taken.
    fn running() -> Self {
        let mut rig = Self::new();
        rig.configure();
        rig.d.start().unwrap();
        rig.tick();
        rig
    }

    fn tick(&mut self) -> Option<u64> {
        self.now += STRIDE;
        self.d.tick(self.now)
    }

    /// Posts a job of `identity` writing `writes` from config buffer `config` into output
    /// `out`.
    fn post(&self, identity: u64, config: u32, writes: &[(u32, u32)], out: u32) {
        self.config.fill(config, &list(writes));
        post(&self.config, &job(identity, config, out));
    }

    fn jobs(&self) -> Vec<Ran> {
        self.played.log().jobs.clone()
    }

    fn counters(&mut self) -> (u64, u64, u64) {
        let c = self.d.counters().unwrap();
        (c.jobs, c.dropped, c.errors)
    }
}

fn input() -> IspInput {
    IspInput {
        width: WIDTH,
        height: HEIGHT,
        bit_depth: 10,
        bayer: BayerOrder::Rggb,
    }
}

fn image(fourcc: u32) -> IspOutput {
    IspOutput {
        width: WIDTH,
        height: HEIGHT,
        fourcc,
        pad_bytes: 0,
    }
}

/// A configuration holding `writes`.
fn list(writes: &[(u32, u32)]) -> Vec<u8> {
    let mut out = vec![0u8; CONFIG_BYTES];
    out[..4].copy_from_slice(&(writes.len() as u32).to_le_bytes());
    for (i, (addr, value)) in writes.iter().enumerate() {
        out[4 + 8 * i..8 + 8 * i].copy_from_slice(&addr.to_le_bytes());
        out[8 + 8 * i..12 + 8 * i].copy_from_slice(&value.to_le_bytes());
    }
    out
}

fn post(stream: &MockStream, job: &IspJob) {
    let mut buf = [0; 128];
    let mut w = Writer::new(&mut buf);
    job.encode(&mut w).unwrap();
    let n = w.len();
    stream.post_job(&buf[..n]);
}

fn job(identity: u64, config_buffer: u32, out: u32) -> IspJob {
    IspJob {
        identity,
        layout: LAYOUT_WRITES,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs: [out, NO_BUFFER, NO_BUFFER, NO_BUFFER],
    }
}

fn w(offset: u32, value: u32) -> Write {
    Write { offset, value }
}

/// The driver's own writes for the rig's 64 by 32 RGGB 10-bit input, NV12 out.
fn setup() -> Vec<Write> {
    vec![
        w(0x404, RGGB_10_BITS),
        w(0x408, 0),
        w(0x40c, 0),
        w(0x410, WIDTH),
        w(0x414, HEIGHT),
        w(0x418, 0),
        w(0x594, 0),
        w(0x598, 0),
        w(0x59c, WIDTH),
        w(0x5a0, HEIGHT),
        w(0x1314, 0),
        w(0x131c, 0x0200_0100),
        w(0x1330, WIDTH),
        w(0x1334, WIDTH),
        w(0x1338, HEIGHT),
        w(0x133c, LUMA),
        w(ISP_CTRL, CTRL_BASE | GEN_CFG_UPD),
    ]
}

fn addrs(index: u32) -> Vec<Addr> {
    vec![
        Addr {
            reg: 0x1324,
            buffer: index,
            offset: 0,
            bytes: u64::from(LUMA),
        },
        Addr {
            reg: 0x1340,
            buffer: index,
            offset: u64::from(LUMA),
            bytes: u64::from(CHROMA),
        },
        Addr {
            reg: 0x134c,
            buffer: index,
            offset: u64::from(LUMA),
            bytes: u64::from(CHROMA),
        },
        Addr {
            reg: 0x1394,
            buffer: index,
            offset: 0,
            bytes: u64::from(LUMA),
        },
    ]
}

/// The statistics bytes the session's reads give with `seed`.
fn statistics(seed: u32) -> Vec<u8> {
    (0..registers().count() as u32)
        .flat_map(|i| seed.wrapping_add(i).to_le_bytes())
        .collect()
}

// ---- What the ISP says it takes.

#[test]
fn info_names_one_output_and_the_streams() {
    let mut rig = Rig::new();
    let info = rig.d.info().unwrap();
    assert_eq!(info.max_outputs, 1);
    assert_eq!((info.max_width, info.max_height), (4096, 4096));
    assert_eq!(info.statistics_layout, LAYOUT_STATISTICS);
    assert_eq!(info.statistics_bytes, 172);
    let c = rig.d.config_layout(0).unwrap();
    assert_eq!((c.layout, c.bytes), (LAYOUT_WRITES, 8196));
    assert_eq!(rig.d.config_layout(1).unwrap_err(), Status::NOT_FOUND);
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = rig.d.config_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"config");
    let n = rig.d.statistics_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"statistics");
    let n = rig.d.output_stream(0, &mut out).unwrap();
    assert_eq!(&out[..n], b"output0");
    assert_eq!(
        rig.d.output_stream(1, &mut out).unwrap_err(),
        Status::NOT_FOUND
    );
    assert_eq!(
        rig.d.input_stream(&mut out).unwrap_err(),
        Status::NOT_SUPPORTED
    );
    // Every write the layout holds goes to the module in one request.
    assert_eq!(MAX_CALLER_WRITES, crate::MAX_WRITES);
    assert_eq!(SETUP_WRITES, 17);
}

#[test]
fn input_and_output_take_only_what_the_main_path_writes() {
    let mut rig = Rig::new();
    let bad_inputs = [
        IspInput {
            width: 0,
            ..input()
        },
        IspInput {
            height: 0,
            ..input()
        },
        IspInput {
            width: 4097,
            ..input()
        },
        IspInput {
            height: 4097,
            ..input()
        },
        IspInput {
            bit_depth: 14,
            ..input()
        },
        IspInput {
            bit_depth: 0,
            ..input()
        },
        IspInput {
            bayer: BayerOrder::Mono,
            ..input()
        },
    ];
    for bad in bad_inputs {
        assert_eq!(
            rig.d.set_input(bad).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{bad:?}"
        );
    }
    let bad_outputs = [
        IspOutput {
            width: 0,
            ..image(NV12)
        },
        IspOutput {
            width: 72,
            ..image(NV12)
        },
        IspOutput {
            height: 31,
            ..image(NV12)
        },
        IspOutput {
            width: 4112,
            ..image(NV12)
        },
        IspOutput {
            height: 4098,
            ..image(NV12)
        },
        IspOutput {
            pad_bytes: u32::MAX,
            ..image(NV12)
        },
        image(u32::from_le_bytes(*b"YUYV")),
    ];
    for bad in bad_outputs {
        assert_eq!(
            rig.d.set_output(0, bad).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{bad:?}"
        );
    }
    assert_eq!(
        rig.d.set_output(1, image(NV12)).unwrap_err(),
        Status::NOT_FOUND
    );
    let layout = rig.d.set_output(0, image(NV16)).unwrap();
    assert_eq!((layout.stride, layout.size), (WIDTH, 2 * LUMA));
    let padded = rig
        .d
        .set_output(
            0,
            IspOutput {
                pad_bytes: 100,
                ..image(NV12)
            },
        )
        .unwrap();
    assert_eq!(padded.size, LUMA + CHROMA + 100);
    // Nothing to start without an input, or with sizes that differ: the path doesn't scale.
    assert_eq!(rig.d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    rig.d
        .set_input(IspInput {
            width: 128,
            ..input()
        })
        .unwrap();
    assert_eq!(rig.d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    rig.d.set_input(input()).unwrap();
    rig.d.clear_output(0).unwrap();
    assert_eq!(rig.d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(rig.played.log().opens, 0);
    rig.configure();
    rig.d.start().unwrap();
    assert_eq!(rig.played.log().opens, 1);
    // Nothing changes while running.
    assert_eq!(rig.d.set_input(input()).unwrap_err(), Status::BUSY);
    assert_eq!(rig.d.set_output(0, image(NV12)).unwrap_err(), Status::BUSY);
    assert_eq!(rig.d.clear_output(0).unwrap_err(), Status::BUSY);
    assert_eq!(rig.d.start().unwrap_err(), Status::BUSY);
    rig.d.stop().unwrap();
    assert_eq!(
        rig.played.log().closes,
        2,
        "start closes first, stop closes"
    );
}

#[test]
fn an_output_stream_without_handles_is_refused() {
    let config = MockStream::with_bytes(4, CONFIG_BYTES);
    let statistics = MockStream::with_bytes(4, STATISTICS_BYTES);
    let output = MockStream::with_bytes(4, (LUMA + CHROMA) as usize);
    let played = Played::default();
    let needs = K230IspFramesNeeds {
        config: lemnos_core::Held::boxed(Box::new(config)),
        statistics: lemnos_core::Held::boxed(Box::new(statistics)),
        output0: lemnos_core::Held::boxed(Box::new(output)),
    };
    let mut d = K230IspFrames::with_module(Box::new(Session(played.clone())), needs);
    d.set_input(input()).unwrap();
    d.set_output(0, image(NV12)).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(played.log().opens, 0);
}

#[test]
fn a_session_that_will_not_open_fails_start() {
    let mut rig = Rig::new();
    rig.configure();
    rig.played.log().fail_open = true;
    assert_eq!(rig.d.start().unwrap_err(), Status::IO);
    assert_eq!(rig.tick(), None, "still stopped");
}

// ---- Jobs.

#[test]
fn a_job_goes_to_the_module_as_the_callers_writes_then_the_drivers_and_four_ranges() {
    let mut rig = Rig::running();
    let caller = [
        (AE_H_SIZE, 0x80),
        (AE_V_SIZE, 0x40),
        (ISP_AWB_GAIN_RB, 0x0123_0145),
        (ISP_CTRL, GAMMA_OUT | GEN_CFG_UPD),
    ];
    rig.post(7, 2, &caller, 1);
    assert!(rig.tick().is_some());
    // The caller's writes through the configuration session, as they are: the module keeps the
    // frames role's bits of `isp_ctrl` under the caller's.
    assert_eq!(
        rig.played.log().configs,
        vec![vec![
            w(AE_H_SIZE, 0x80),
            w(AE_V_SIZE, 0x40),
            w(ISP_AWB_GAIN_RB, 0x0123_0145),
            w(ISP_CTRL, GAMMA_OUT | GEN_CFG_UPD),
        ]]
    );
    let reads: Vec<u32> = registers().collect();
    assert_eq!(reads.len(), 43);
    assert_eq!(
        rig.jobs(),
        vec![Ran {
            writes: setup(),
            addrs: addrs(0),
            reads,
            start: 1,
            timeout_ms: FRAME_TIMEOUT_MS,
        }]
    );
    assert_eq!(rig.played.log().imports.len(), 1);
    rig.played.end(Ok(FRAME_END));
    rig.post(8, 3, &[(AE_H_SIZE, 1)], 2);
    rig.tick();
    let second = rig.jobs().pop().unwrap();
    assert_eq!(second.writes, setup());
    assert_eq!(second.addrs, addrs(1));
    assert_eq!(
        rig.played.log().configs.last().unwrap(),
        &vec![w(AE_H_SIZE, 1)]
    );
}

#[test]
fn a_configuration_the_module_refuses_drops_the_job_before_its_frame() {
    let mut rig = Rig::running();
    rig.played.log().configure_answer = Some(libc::EPERM);
    rig.post(1, 0, &[(AE_H_SIZE, 1)], 0);
    rig.tick();
    assert_eq!(rig.played.log().starts, 0);
    assert_eq!(rig.counters(), (0, 1, 0));
    assert_eq!(rig.d.health(), Health::Ok);
    // A configuration session the module ended is a failed session.
    rig.played.log().configure_answer = Some(libc::ENODEV);
    rig.post(2, 1, &[(AE_H_SIZE, 1)], 1);
    rig.tick();
    assert!(matches!(rig.d.health(), Health::Failed(_)));
}

#[test]
fn nv16_writes_a_chroma_plane_as_large_as_the_luma() {
    let mut rig = Rig::new();
    let output = MockStream::with_handles(vec![buffer(u64::from(2 * LUMA))]);
    rig.output = output.clone();
    let needs = K230IspFramesNeeds {
        config: lemnos_core::Held::boxed(Box::new(rig.config.clone())),
        statistics: lemnos_core::Held::boxed(Box::new(rig.statistics.clone())),
        output0: lemnos_core::Held::boxed(Box::new(output)),
    };
    rig.d = K230IspFrames::with_module(Box::new(Session(rig.played.clone())), needs);
    rig.d
        .set_input(IspInput {
            bit_depth: 8,
            bayer: BayerOrder::Bggr,
            ..input()
        })
        .unwrap();
    rig.d.set_output(0, image(NV16)).unwrap();
    rig.d.start().unwrap();
    rig.tick();
    rig.post(1, 0, &[], 0);
    rig.tick();
    let ran = rig.jobs().pop().unwrap();
    // BGGR is pattern 3; 8 bits is input selection 3.
    assert_eq!(ran.writes[0], w(0x404, 3 << 3 | 3 << 12));
    assert_eq!(ran.writes[10], w(0x1314, 1 << 2));
    assert_eq!(ran.addrs[1].offset, u64::from(LUMA));
    assert_eq!(ran.addrs[1].bytes, u64::from(LUMA));
    assert_eq!(ran.addrs[2].bytes, u64::from(LUMA));
}

#[test]
fn a_frame_comes_out_when_the_next_job_takes_the_path() {
    let mut rig = Rig::running();
    rig.played.log().seed = 0x100;
    rig.post(70, 0, &[(AE_H_SIZE, 1)], 0);
    rig.tick();
    // Still running: nothing out.
    rig.tick();
    assert!(rig.output.take_frame().is_none());
    // The path took job 70's buffer: it is writing it, so still nothing out.
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    assert!(rig.output.take_frame().is_none());
    assert!(rig.statistics.take_frame().is_none());
    // Job 71 takes the path at the end of the frame in job 70's buffer.
    rig.post(71, 1, &[(AE_H_SIZE, 2)], 1);
    rig.tick();
    assert_eq!(rig.played.log().starts, 2);
    rig.played.log().seed = 0x200;
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    let image = rig.output.take_frame().unwrap();
    assert_eq!(
        (image.buffer, image.bytes, image.identity),
        (0, LUMA + CHROMA, 70)
    );
    assert_eq!((image.sequence, image.timestamp_ns), (0, rig.now));
    let stats = rig.statistics.take_frame().unwrap();
    assert_eq!((stats.bytes, stats.identity, stats.sequence), (172, 70, 0));
    assert_eq!(rig.statistics.contents(stats.buffer), statistics(0x200));
    assert_eq!(rig.counters(), (1, 0, 0));
    // A job naming the buffer the path writes can't go.
    rig.post(72, 2, &[], 1);
    rig.tick();
    assert_eq!(rig.counters(), (1, 1, 0));
    // The buffer handed back is taken again.
    rig.output.return_buffer(0);
    rig.post(73, 3, &[], 0);
    rig.tick();
    assert_eq!(rig.played.log().starts, 3);
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    let image = rig.output.take_frame().unwrap();
    assert_eq!((image.buffer, image.identity, image.sequence), (1, 71, 1));
    // The same memory is imported once.
    assert_eq!(rig.played.log().imports.len(), 2);
}

#[test]
fn input_and_output_layouts_are_what_is_set_and_none_before_or_after() {
    let mut rig = Rig::new();
    assert_eq!(rig.d.input_layout().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(rig.d.output_layout(0).unwrap(), None, "before set_output");
    assert_eq!(rig.d.output_layout(1).unwrap_err(), Status::NOT_FOUND);
    rig.d.set_input(input()).unwrap();
    assert_eq!(rig.d.input_layout().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(
        rig.d.output_layout(0).unwrap(),
        None,
        "input is not an output"
    );
    let nv12 = OutputLayout {
        stride: WIDTH,
        size: LUMA + CHROMA,
    };
    let answered = rig.d.set_output(0, image(NV12)).unwrap();
    assert_eq!(answered, nv12);
    assert_eq!(rig.d.output_layout(0).unwrap(), Some(nv12));
    // The padding the output asked for is in the size, not the stride.
    let padded = OutputLayout {
        stride: WIDTH,
        size: 2 * LUMA + 100,
    };
    let answered = rig
        .d
        .set_output(
            0,
            IspOutput {
                pad_bytes: 100,
                ..image(NV16)
            },
        )
        .unwrap();
    assert_eq!(answered, padded);
    assert_eq!(rig.d.output_layout(0).unwrap(), Some(padded));
    // While running it is the same.
    rig.d.clear_output(0).unwrap();
    assert_eq!(rig.d.output_layout(0).unwrap(), None, "after clear_output");
    rig.configure();
    rig.d.start().unwrap();
    assert_eq!(rig.d.output_layout(0).unwrap(), Some(nv12));
    // The planes of a job's image fit in the size the layout gives.
    rig.post(1, 0, &[], 0);
    rig.tick();
    let ran = rig.jobs().pop().unwrap();
    let end = ran.addrs.iter().map(|a| a.offset + a.bytes).max().unwrap();
    assert!(end <= u64::from(nv12.size), "{end}");
    assert_eq!(rig.d.output_layout(1).unwrap_err(), Status::NOT_FOUND);
    rig.d.stop().unwrap();
    // A refused output changes nothing: the layout set before stays.
    let refused = IspOutput {
        width: 0,
        ..image(NV12)
    };
    assert_eq!(
        rig.d.set_output(0, refused).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    assert_eq!(rig.d.output_layout(0).unwrap(), Some(nv12));
}

#[test]
fn a_config_buffer_a_job_in_flight_holds_drops_a_job_that_names_it() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[(AE_H_SIZE, 1)], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    // Job 2 names buffer 0, which job 1 still holds (its image is not out). The consumer writes
    // nothing to buffer 0 here.
    post(&rig.config, &job(2, 0, 1));
    rig.tick();
    assert_eq!(rig.counters(), (0, 1, 0), "job 2 refused");
    assert_eq!(
        rig.played.log().starts,
        1,
        "nothing of job 2 went to the module"
    );
    assert!(rig.output.take_frame().is_none());
    // Job 3 takes the path at job 1's frame end: job 1's image comes out with its identity.
    rig.post(3, 1, &[], 1);
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    rig.tick();
    let image = rig.output.take_frame().unwrap();
    assert_eq!((image.buffer, image.identity), (0, 1));
    assert_eq!(rig.counters(), (1, 1, 0));
    assert_eq!(rig.played.log().starts, 2);
    assert_eq!(rig.played.log().configs.len(), 2, "jobs 1 and 3 only");
}

#[test]
fn a_config_buffer_rewritten_after_its_job_completes_is_read_by_the_next_job() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[(AE_H_SIZE, 1)], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(2, 1, &[(AE_H_SIZE, 2)], 1);
    rig.tick();
    // Job 2's frame end completes job 1, whose image comes out.
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    assert_eq!(rig.output.take_frame().unwrap().identity, 1);
    // Job 1 is complete: buffer 0 may be written again and named.
    rig.post(3, 0, &[(AE_H_SIZE, 3)], 2);
    rig.tick();
    assert_eq!(rig.played.log().starts, 3);
    assert_eq!(
        rig.played.log().configs.last().unwrap(),
        &vec![w(AE_H_SIZE, 3)]
    );
}

#[test]
fn a_write_to_a_config_buffer_an_accepted_job_holds_does_not_change_that_job() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[(AE_H_SIZE, 1)], 0);
    rig.tick();
    // The consumer writes buffer 0 while job 1 holds it: job 1 was copied at accept and runs with that.
    rig.config.fill(0, &list(&[(AE_H_SIZE, 9)]));
    rig.post(2, 1, &[(AE_H_SIZE, 2)], 1);
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    assert_eq!(rig.output.take_frame().unwrap().identity, 1);
    assert_eq!(
        rig.played.log().configs,
        vec![vec![w(AE_H_SIZE, 1)], vec![w(AE_H_SIZE, 2)]],
        "job 1 programmed from the bytes it was accepted with"
    );
}

#[test]
fn a_stop_gives_every_config_buffer_back_and_a_restart_runs_each_with_its_new_contents() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[(AE_H_SIZE, 1)], 0);
    rig.tick();
    // Job 1 is with the module when stop comes: it ends as an error and publishes nothing.
    rig.d.stop().unwrap();
    assert_eq!(rig.counters(), (0, 0, 1));
    assert!(rig.output.take_frame().is_none());
    // Every config buffer is the consumer's again: each is rewritten with contents no job ran.
    for buffer in 0..4 {
        rig.config.fill(buffer, &list(&[(AE_H_SIZE, 10 + buffer)]));
    }
    rig.d.start().unwrap();
    rig.tick();
    // Each buffer is named in turn; a job goes once the one before it has been collected.
    for (identity, buffer) in [(2, 0), (3, 1), (4, 2), (5, 3)] {
        post(&rig.config, &job(identity, buffer, buffer));
        rig.tick();
        rig.played.end(Ok(FRAME_END));
        rig.tick();
    }
    assert_eq!(
        rig.played.log().configs[1..],
        [
            vec![w(AE_H_SIZE, 10)],
            vec![w(AE_H_SIZE, 11)],
            vec![w(AE_H_SIZE, 12)],
            vec![w(AE_H_SIZE, 13)],
        ]
    );
    // Jobs 2 to 4 completed in turn; job 5 is still with the path.
    let done: Vec<u64> = std::iter::from_fn(|| rig.output.take_frame())
        .map(|frame| frame.identity)
        .collect();
    assert_eq!(done, [2, 3, 4]);
    assert_eq!(rig.counters(), (3, 0, 0));
}

#[test]
fn a_frame_that_wrapped_comes_out_empty() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(2, 1, &[], 1);
    rig.tick();
    // The Cb plane reached its size.
    rig.played.end(Ok(FRAME_END | 0x200));
    rig.tick();
    let image = rig.output.take_frame().unwrap();
    assert_eq!((image.buffer, image.bytes, image.identity), (0, 0, 1));
    assert!(rig.statistics.take_frame().is_none());
    assert_eq!(rig.counters(), (0, 0, 1));
}

/// A named job and the writes its configuration holds.
type Post = (&'static str, IspJob, Vec<(u32, u32)>);

#[test]
fn work_of_the_wrong_shape_never_reaches_the_module() {
    let mut rig = Rig::running();
    let posts: Vec<Post> = vec![
        (
            "another layout",
            IspJob {
                layout: 9,
                ..job(1, 0, 0)
            },
            vec![],
        ),
        ("a config buffer past the stream", job(1, 4, 0), vec![]),
        (
            "an input buffer",
            IspJob {
                input_buffer: 0,
                ..job(1, 0, 0)
            },
            vec![],
        ),
        (
            "a second output",
            IspJob {
                outputs: [0, 1, NO_BUFFER, NO_BUFFER],
                ..job(1, 0, 0)
            },
            vec![],
        ),
        ("an output past the stream", job(1, 0, OUTPUTS), vec![]),
        ("no output", job(1, 0, NO_BUFFER), vec![]),
        ("an address register", job(1, 0, 0), vec![(0x1324, 0x1000)]),
        (
            "a register not in the table",
            job(1, 0, 0),
            vec![(0x9000, 0)],
        ),
    ];
    let count = posts.len() as u64;
    for (what, job, writes) in posts {
        rig.config.fill(0, &list(&writes));
        post(&rig.config, &job);
        rig.tick();
        assert_eq!(rig.played.log().starts, 0, "{what}");
    }
    // Not decodable at all.
    rig.config.post_job(&[1, 2, 3]);
    rig.tick();
    assert_eq!(rig.counters(), (0, count + 1, 0));
    assert!(rig.played.log().imports.is_empty());
}

#[test]
fn a_refused_job_is_dropped_and_the_session_stays() {
    let mut rig = Rig::running();
    for errno in [
        libc::EPERM,
        libc::EINVAL,
        libc::ENOENT,
        libc::EFAULT,
        libc::ENOSPC,
    ] {
        rig.played.log().start_answer = Some(errno);
        rig.post(1, 0, &[], 0);
        rig.config.return_buffer(0);
        rig.tick();
    }
    assert_eq!(rig.counters(), (0, 5, 0));
    assert_eq!(rig.played.log().closes, 1, "only start's");
    assert_eq!(rig.d.health(), Health::Ok);
    rig.post(2, 1, &[], 0);
    rig.tick();
    assert!(rig.played.log().running);
}

#[test]
fn a_frame_that_never_comes_fails_both_jobs_and_the_next_starts_the_path() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(2, 1, &[], 1);
    rig.tick();
    // The module reset the block when the frame was late.
    rig.played
        .end(Err(io::Error::from_raw_os_error(libc::ETIMEDOUT)));
    rig.tick();
    assert_eq!(rig.counters(), (0, 0, 2));
    assert_eq!(rig.played.log().closes, 1, "the session stays");
    assert_eq!(rig.d.health(), Health::Ok);
    // Both buffers are the driver's again: neither job is writing.
    rig.post(3, 2, &[], 0);
    rig.tick();
    assert_eq!(rig.played.log().starts, 3);
    rig.played.end(Ok(FRAME_END));
    rig.post(4, 3, &[], 1);
    rig.tick();
    assert_eq!(rig.played.log().starts, 4);
    assert!(
        rig.output.take_frame().is_none(),
        "no frame came from the reset"
    );
}

#[test]
fn a_block_that_did_not_take_the_buffers_is_reset_and_opened_again() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(2, 1, &[], 1);
    rig.tick();
    rig.played.end(Ok(FRAME_END | STATUS_UNLATCHED));
    let at = rig.now + STRIDE;
    assert_eq!(rig.tick(), Some(at + backoff_ns(0)));
    assert!(matches!(rig.d.health(), Health::Failed(_)));
    assert_eq!(rig.counters(), (0, 0, 2));
    assert_eq!(rig.played.log().closes, 2);
    assert!(
        rig.output.take_frame().is_none(),
        "no buffer goes out a block may write"
    );
    // Not before the delay.
    rig.tick();
    assert_eq!(rig.played.log().opens, 1);
    // A failed open doubles it.
    rig.played.log().fail_open = true;
    rig.now = at + backoff_ns(0);
    assert_eq!(rig.d.tick(rig.now), Some(rig.now + backoff_ns(1)));
    rig.played.log().fail_open = false;
    rig.now += backoff_ns(1);
    rig.d.tick(rig.now);
    assert_eq!(rig.played.log().opens, 2);
    assert_eq!(rig.d.health(), Health::Ok);
    // A new session imports again.
    rig.post(3, 2, &[], 0);
    rig.tick();
    assert_eq!(rig.played.log().imports.len(), 3);
    assert_eq!(rig.jobs().pop().unwrap().addrs, addrs(0));
}

#[test]
fn the_delay_between_attempts_is_capped_at_8_s() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(40), 8_000_000_000);
}

#[test]
fn a_session_the_module_ends_is_opened_again() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.played
        .end(Err(io::Error::from_raw_os_error(libc::ENODEV)));
    rig.tick();
    assert!(matches!(rig.d.health(), Health::Failed(_)));
    assert_eq!(rig.counters(), (0, 0, 1));
    // A start the module fails without refusing it does the same.
    rig.now += backoff_ns(0);
    rig.d.tick(rig.now);
    assert_eq!(rig.d.health(), Health::Ok);
    rig.played.log().start_answer = Some(libc::EIO);
    rig.post(2, 1, &[], 1);
    rig.tick();
    assert!(matches!(rig.d.health(), Health::Failed(_)));
}

#[test]
fn an_output_consumer_that_goes_takes_its_buffers_from_the_block() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(2, 1, &[], 1);
    rig.tick();
    rig.output.change_consumer();
    rig.tick();
    // The session closed (the module resets the block) before either buffer was given up.
    let log = rig.played.log();
    assert_eq!((log.closes, log.opens), (2, 2));
    drop(log);
    assert_eq!(rig.counters(), (0, 0, 2));
    // Given up with nothing in them; they reach no one and come back free.
    assert!(rig.output.take_frame().is_none());
    assert_eq!(rig.d.health(), Health::Ok);
    // A buffer held from the old consumer is given up when a job names it; one that came back
    // is the new consumer's, imported into the new session.
    rig.post(3, 2, &[], 2);
    rig.post(4, 3, &[], 0);
    rig.tick();
    assert_eq!(rig.counters(), (0, 1, 2));
    assert_eq!(rig.played.log().starts, 3);
    assert_eq!(rig.played.log().imports.len(), 3);
    assert_eq!(rig.jobs().pop().unwrap().addrs, addrs(0));
}

#[test]
fn a_buffer_named_again_is_not_imported_again() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(2, 1, &[], 1);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(3, 2, &[], 2);
    rig.tick();
    assert_eq!(rig.output.take_frame().unwrap().buffer, 0);
    rig.output.return_buffer(0);
    rig.post(4, 3, &[], 0);
    rig.played.end(Ok(FRAME_END));
    rig.tick();
    assert_eq!(
        rig.played.log().imports.len(),
        3,
        "buffer 0 again: no import"
    );
    assert!(rig.played.log().releases.is_empty());
}

#[test]
fn stop_closes_the_session() {
    let mut rig = Rig::running();
    rig.post(1, 0, &[], 0);
    rig.tick();
    rig.d.stop().unwrap();
    assert_eq!(rig.played.log().closes, 2);
    assert_eq!(rig.tick(), None);
    assert_eq!(rig.counters(), (0, 0, 1));
}

#[test]
fn the_steady_state_allocates_nothing() {
    let mut rig = Rig::running();
    rig.played.log().ends.reserve(16);
    rig.post(0, 0, &[(AE_H_SIZE, 0)], 0);
    rig.tick();
    rig.played.end(Ok(FRAME_END));
    rig.post(1, 1, &[(AE_H_SIZE, 1)], 1);
    rig.tick();
    rig.played.log().record = false;
    for n in 2..12u32 {
        let buffer = n % OUTPUTS;
        rig.post(u64::from(n), buffer, &[(AE_H_SIZE, n)], buffer);
        rig.played.end(Ok(FRAME_END));
        let at = rig.now + STRIDE;
        let (_, count) = allocations(|| rig.d.tick(at));
        rig.now = at;
        // The mocked streams' queues grow on the first frames out.
        if n >= 4 {
            assert_eq!(count, 0, "job {n}");
        }
        let image = rig.output.take_frame().unwrap();
        assert_eq!(image.identity, u64::from(n - 2));
        rig.output.return_buffer(image.buffer);
        let stats = rig.statistics.take_frame().unwrap();
        rig.statistics.return_buffer(stats.buffer);
    }
    assert_eq!(rig.counters(), (10, 0, 0));
    assert_eq!(rig.played.log().imports.len(), OUTPUTS as usize);
}

/// The refusal `check_config` gives for `entries`, or `None` when it accepts them.
fn frames_refusal<'a>(
    entries: &'a [(&'a str, lemnos_core::ConfigValue<'a>)],
) -> Option<lemnos_core::ConfigError<'a>> {
    <K230IspFrames as lemnos_core::Lifecycle>::check_config(&lemnos_core::Config::new(entries))
        .err()
}

#[test]
fn check_config_of_the_frames_driver_needs_a_block_name() {
    use lemnos_core::ConfigValue::Str;
    assert_eq!(frames_refusal(&[("block", Str("k230_isp"))]), None);
    assert_eq!(frames_refusal(&[]).unwrap().key, "block");
    assert_eq!(frames_refusal(&[("block", Str(""))]).unwrap().key, "block");
    let typo = [("block", Str("k230_isp")), ("nod", Str("/x"))];
    assert_eq!(frames_refusal(&typo).unwrap().key, "nod");
}
