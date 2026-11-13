//! The driver against a stand-in for the `vvcam-isp` device and mocked streams: every expected
//! value here is written out by hand.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, MAX_STREAM_NAME, NO_BUFFER,
};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_driver_k230isp_sys::access::{allowed, readable};
use lemnos_driver_k230isp_sys::regs::{
    AE_H_SIZE, AE_V_SIZE, CTRL_GEN_CFG_UPD, ISP_AWB_GAIN_RB, ISP_CTRL, MI_BASE, MIS_FRAME_IN,
    MIS_FRAME_OUT, WINDOW_BYTES,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamEndpoint, StreamFrame};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use crate::driver::backoff_ns;
use crate::hw::{Event, Hardware};
use crate::stats::registers;
use crate::{
    CONFIG_BYTES, JOB_TIMEOUT_NS, K230Isp, K230IspNeeds, LAYOUT_STATISTICS, LAYOUT_WRITES,
    STATISTICS_BYTES,
};

const STRIDE: u64 = 1_000_000;

/// What the device does, shared with the test.
#[derive(Default)]
struct Block {
    regs: HashMap<u32, u32>,
    writes: Vec<(u32, u32)>,
    events: VecDeque<Event>,
    opened: u32,
    closed: u32,
    /// Reads and writes the sys crate would have refused.
    refused: u32,
    fail_open: Option<Status>,
    fail_event: Option<Status>,
    /// A frame leaves while the statistics are read.
    frame_during_read: bool,
    /// A frame leaves while the update strobe is written.
    frame_during_update: bool,
}

#[derive(Clone, Default)]
struct Handle(Arc<Mutex<Block>>);

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn frame_out(&self, at: u64) {
        self.with(|b| {
            b.events.push_back(Event {
                status: MIS_FRAME_OUT,
                timestamp_ns: at,
            });
        });
    }

    fn writes(&self) -> Vec<(u32, u32)> {
        self.with(|b| b.writes.clone())
    }
}

struct Hw(Handle);

impl Hardware for Hw {
    fn open(&mut self) -> Result<()> {
        self.0.with(|b| {
            if let Some(s) = b.fail_open {
                return Err(s);
            }
            b.opened += 1;
            Ok(())
        })
    }

    fn close(&mut self) {
        self.0.with(|b| b.closed += 1);
    }

    fn read(&mut self, addr: u32) -> Result<u32> {
        self.0.with(|b| {
            if !readable(addr) {
                b.refused += 1;
                return Err(Status::INVALID_ARGUMENT);
            }
            if std::mem::take(&mut b.frame_during_read) {
                b.events.push_back(Event {
                    status: MIS_FRAME_OUT,
                    timestamp_ns: 0,
                });
            }
            Ok(b.regs.get(&addr).copied().unwrap_or(0))
        })
    }

    fn write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.0.with(|b| {
            if !allowed(addr, value) {
                b.refused += 1;
                return Err(Status::INVALID_ARGUMENT);
            }
            if addr == ISP_CTRL && std::mem::take(&mut b.frame_during_update) {
                b.events.push_back(Event {
                    status: MIS_FRAME_OUT,
                    timestamp_ns: 0,
                });
            }
            b.writes.push((addr, value));
            Ok(())
        })
    }

    fn next_event(&mut self) -> Result<Option<Event>> {
        self.0.with(|b| {
            if let Some(s) = b.fail_event.take() {
                return Err(s);
            }
            Ok(b.events.pop_front())
        })
    }
}

/// The streams, as the test holds them.
#[derive(Clone)]
struct Streams {
    config: MockStream,
    statistics: MockStream,
}

impl Streams {
    fn new() -> Self {
        Self {
            config: MockStream::with_bytes(4, CONFIG_BYTES),
            statistics: MockStream::with_bytes(4, STATISTICS_BYTES),
        }
    }

    fn needs(&self, config: StreamHandle) -> K230IspNeeds {
        K230IspNeeds {
            config,
            statistics: lemnos_core::Held::boxed(Box::new(self.statistics.clone())),
        }
    }
}

/// A consumer that scribbles over a config buffer right after the driver read it, as a caller
/// racing the driver would.
struct Racing {
    inner: MockStream,
    scribble: Vec<u8>,
    /// A buffer the consumer hands over just before the driver reads the next job.
    hands_over: Option<u32>,
}

impl StreamEndpoint for Racing {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.inner.buffer_count()
    }

    fn buffer_kind(&self) -> BufferKind {
        self.inner.buffer_kind()
    }

    fn publish(&mut self, frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        self.inner.publish(frame)
    }

    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        self.inner.next_buffer()
    }

    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        if let Some(buffer) = self.hands_over.take() {
            self.inner.return_buffer(buffer);
        }
        self.inner.next_job(out)
    }

    fn write(
        &mut self,
        buffer: u32,
        offset: usize,
        data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        self.inner.write(buffer, offset, data)
    }

    fn read(
        &mut self,
        buffer: u32,
        offset: usize,
        out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        let read = self.inner.read(buffer, offset, out);
        self.inner.fill(buffer, &self.scribble);
        read
    }

    fn buffer_handle(
        &self,
        buffer: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.inner.buffer_handle(buffer)
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
        self.inner.fault_consumer()
    }

    fn buffer_current(&self, buffer: u32) -> core::result::Result<bool, ErrorKind> {
        self.inner.buffer_current(buffer)
    }
}

fn driver_on(streams: &Streams, config: StreamHandle) -> (K230Isp, Handle) {
    let handle = Handle::default();
    handle.with(|b| {
        b.writes.reserve(1 << 16);
        b.events.reserve(64);
        b.regs.reserve(64);
    });
    let d = K230Isp::with_hardware(Box::new(Hw(handle.clone())), streams.needs(config));
    (d, handle)
}

fn driver(streams: &Streams) -> (K230Isp, Handle) {
    driver_on(
        streams,
        lemnos_core::Held::boxed(Box::new(streams.config.clone())),
    )
}

fn running(streams: &Streams) -> (K230Isp, Handle) {
    let (mut d, block) = driver(streams);
    d.start().unwrap();
    // The first tick takes every buffer the consumer has handed over.
    d.tick(0);
    (d, block)
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

fn job(identity: u64, config_buffer: u32) -> IspJob {
    IspJob {
        identity,
        layout: LAYOUT_WRITES,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs: [NO_BUFFER; 4],
    }
}

/// The statistics registers each set to its own value; returns the bytes a buffer must hold.
fn set_statistics(block: &Handle, seed: u32) -> Vec<u8> {
    let mut expect = Vec::new();
    block.with(|b| {
        for (i, addr) in registers().enumerate() {
            let value = seed.wrapping_mul(0x0101_0101) ^ (i as u32 * 0x1357);
            b.regs.insert(addr, value);
            expect.extend_from_slice(&value.to_le_bytes());
        }
    });
    expect
}

// ---- What the ISP says it takes.

#[test]
fn info_and_layouts_name_the_register_list_and_statistics() {
    let (mut d, _b) = driver(&Streams::new());
    let info = d.info().unwrap();
    assert_eq!(info.max_outputs, 0, "no frames out");
    assert_eq!(info.config_layouts, 1);
    assert_eq!(info.statistics_layout, LAYOUT_STATISTICS);
    assert_eq!(info.statistics_bytes, 172);
    let c = d.config_layout(0).unwrap();
    assert_eq!((c.layout, c.bytes), (LAYOUT_WRITES, 8196));
    assert_eq!(d.config_layout(1).unwrap_err(), Status::NOT_FOUND);
}

#[test]
fn the_frame_path_is_not_this_drivers() {
    let (mut d, _b) = driver(&Streams::new());
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = d.config_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"config");
    let n = d.statistics_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"statistics");
    assert_eq!(d.input_stream(&mut out).unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(d.output_stream(0, &mut out).unwrap_err(), Status::NOT_FOUND);
    let input = IspInput {
        width: 1280,
        height: 800,
        bit_depth: 10,
        bayer: BayerOrder::Rggb,
    };
    assert_eq!(d.set_input(input).unwrap_err(), Status::NOT_SUPPORTED);
    let image = IspOutput {
        width: 64,
        height: 32,
        fourcc: u32::from_le_bytes(*b"NV12"),
        pad_bytes: 0,
    };
    assert_eq!(d.set_output(0, image).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.clear_output(0).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(
        d.config_stream(&mut [0u8; 3]).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
}

#[test]
fn start_opens_the_device_once() {
    let (mut d, b) = driver(&Streams::new());
    d.start().unwrap();
    assert_eq!(b.with(|b| b.opened), 1);
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    d.stop().unwrap();
    d.start().unwrap();
    assert_eq!(b.with(|b| b.opened), 2);
}

#[test]
fn a_stream_with_more_buffers_than_the_driver_tracks_is_refused() {
    let streams = Streams {
        config: MockStream::with_bytes(65, 16),
        statistics: MockStream::with_bytes(4, STATISTICS_BYTES),
    };
    let (mut d, b) = driver(&streams);
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(b.with(|b| b.opened), 0);
}

#[test]
fn a_device_that_will_not_open_fails_start() {
    let (mut d, b) = driver(&Streams::new());
    b.with(|b| b.fail_open = Some(Status::NO_DEVICE));
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
    assert_eq!(d.tick(0), None, "still stopped");
}

// ---- Jobs.

#[test]
fn a_job_writes_its_list_then_the_update_and_gets_the_second_frames_statistics() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    let writes = [
        (AE_H_SIZE, 0x80),
        (AE_V_SIZE, 0x40),
        (ISP_AWB_GAIN_RB, 0x0123_0145),
    ];
    streams.config.fill(2, &list(&writes));
    // A frame that left before the job doesn't count for it.
    b.frame_out(5);
    post(&streams.config, &job(77, 2));
    d.tick(STRIDE);
    let mut expect = writes.to_vec();
    expect.push((ISP_CTRL, CTRL_GEN_CFG_UPD));
    assert_eq!(b.writes(), expect);
    // The first frame out may have started before the update.
    b.frame_out(10);
    d.tick(2 * STRIDE);
    assert!(streams.statistics.take_frame().is_none());
    // An interrupt that is not a frame leaving doesn't count.
    b.with(|b| {
        b.events.push_back(Event {
            status: MIS_FRAME_IN,
            timestamp_ns: 15,
        });
    });
    d.tick(3 * STRIDE);
    assert!(streams.statistics.take_frame().is_none());
    let stats = set_statistics(&b, 9);
    b.frame_out(20);
    d.tick(4 * STRIDE);
    let f = streams.statistics.take_frame().unwrap();
    assert_eq!(f.identity, 77);
    assert_eq!(f.bytes, 172);
    assert_eq!(f.sequence, 3, "the third frame out since start");
    assert_eq!(f.timestamp_ns, 20);
    assert_eq!(streams.statistics.contents(f.buffer), stats);
    let c = d.counters().unwrap();
    assert_eq!((c.jobs, c.dropped, c.errors), (1, 0, 0));
    assert_eq!(b.with(|b| b.refused), 0);
}

#[test]
fn frames_that_leave_while_the_update_is_written_do_not_count() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    b.with(|b| b.frame_during_update = true);
    post(&streams.config, &job(1, 0));
    d.tick(STRIDE);
    b.frame_out(1);
    d.tick(2 * STRIDE);
    assert!(streams.statistics.take_frame().is_none());
    b.frame_out(2);
    d.tick(3 * STRIDE);
    let f = streams.statistics.take_frame().unwrap();
    assert_eq!((f.identity, f.sequence, f.timestamp_ns), (1, 3, 2));
}

#[test]
fn the_driver_uses_its_copy_when_the_caller_changes_the_buffer_after_the_read() {
    let streams = Streams::new();
    let config = Racing {
        inner: streams.config.clone(),
        scribble: list(&[(MI_BASE, 1), (WINDOW_BYTES, 0)]),
        hands_over: None,
    };
    let (mut d, b) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)));
    d.start().unwrap();
    d.tick(0);
    streams.config.fill(1, &list(&[(AE_V_SIZE, 6)]));
    post(&streams.config, &job(5, 1));
    d.tick(STRIDE);
    assert_eq!(b.writes(), [(AE_V_SIZE, 6), (ISP_CTRL, CTRL_GEN_CFG_UPD)]);
    assert_eq!(b.with(|b| b.refused), 0);
}

#[test]
fn a_job_the_driver_cannot_run_is_dropped_before_anything_reaches_the_block() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    let bad_lists = [
        list(&[(AE_V_SIZE, 2), (MI_BASE, 1)]),
        list(&[(WINDOW_BYTES, 0)]),
        list(&[(u32::MAX - 3, 0)]),
        list(&[(ISP_CTRL, 1)]),
        {
            let mut l = list(&[(AE_V_SIZE, 2)]);
            l[..4].copy_from_slice(&1025u32.to_le_bytes());
            l
        },
    ];
    for (i, bad) in bad_lists.iter().enumerate() {
        streams.config.fill(0, bad);
        post(&streams.config, &job(i as u64, 0));
        d.tick(STRIDE * (i as u64 + 1));
    }
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    let mut with_input = job(10, 0);
    with_input.input_buffer = 1;
    let mut with_output = job(11, 0);
    with_output.outputs[0] = 0;
    for j in [
        IspJob {
            layout: LAYOUT_STATISTICS,
            ..job(12, 0)
        },
        job(13, 4),
        job(14, u32::MAX),
        with_input,
        with_output,
    ] {
        post(&streams.config, &j);
    }
    // Not a job at all.
    streams.config.post_job(&[1, 2, 3]);
    d.tick(STRIDE * 10);
    assert_eq!(b.writes(), [], "nothing reached the block");
    assert_eq!(
        b.with(|b| b.refused),
        0,
        "the driver's own check came first"
    );
    let c = d.counters().unwrap();
    assert_eq!((c.jobs, c.dropped, c.errors), (0, 11, 0));
}

#[test]
fn a_config_buffer_the_driver_does_not_hold_drops_the_job() {
    let streams = Streams::new();
    let (mut d, b) = driver(&streams);
    // The consumer kept buffer 3: take it before the driver can.
    let mut consumer = streams.config.clone();
    let mut taken = Vec::new();
    while let Some(buffer) = consumer.next_buffer().unwrap() {
        taken.push(buffer);
    }
    for buffer in taken.into_iter().filter(|&x| x != 3) {
        streams.config.return_buffer(buffer);
    }
    d.start().unwrap();
    d.tick(0);
    post(&streams.config, &job(1, 3));
    d.tick(STRIDE);
    assert_eq!(b.writes(), []);
    assert_eq!(d.counters().unwrap().dropped, 1);
}

#[test]
fn a_config_buffer_handed_over_and_named_while_a_tick_runs_is_held_for_that_job() {
    let streams = Streams::new();
    let good = list(&[(AE_V_SIZE, 6)]);
    // The consumer hands buffer 3 over after the tick's look at the buffers, then posts the
    // job that names it; the tick reads that job.
    let config = Racing {
        inner: streams.config.clone(),
        scribble: good.clone(),
        hands_over: Some(3),
    };
    let (mut d, b) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)));
    // The consumer kept buffer 3: take it before the driver can.
    let mut consumer = streams.config.clone();
    let mut taken = Vec::new();
    while let Some(buffer) = consumer.next_buffer().unwrap() {
        taken.push(buffer);
    }
    for buffer in taken.into_iter().filter(|&x| x != 3) {
        streams.config.return_buffer(buffer);
    }
    d.start().unwrap();
    streams.config.fill(3, &good);
    post(&streams.config, &job(1, 3));
    d.tick(0);
    assert_eq!(b.writes(), [(AE_V_SIZE, 6), (ISP_CTRL, CTRL_GEN_CFG_UPD)]);
    assert_eq!(d.counters().unwrap().dropped, 0);
}

#[test]
fn one_job_is_with_the_block_at_a_time() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    streams.config.fill(1, &list(&[(AE_V_SIZE, 4)]));
    post(&streams.config, &job(1, 0));
    post(&streams.config, &job(2, 1));
    d.tick(STRIDE);
    assert_eq!(b.writes(), [(AE_V_SIZE, 2), (ISP_CTRL, CTRL_GEN_CFG_UPD)]);
    b.frame_out(1);
    b.frame_out(2);
    d.tick(STRIDE * 2);
    assert_eq!(streams.statistics.take_frame().unwrap().identity, 1);
    assert_eq!(
        b.writes(),
        [
            (AE_V_SIZE, 2),
            (ISP_CTRL, CTRL_GEN_CFG_UPD),
            (AE_V_SIZE, 4),
            (ISP_CTRL, CTRL_GEN_CFG_UPD)
        ],
        "the second goes once the first is done"
    );
}

#[test]
fn a_frame_that_passed_between_looks_is_an_error() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    post(&streams.config, &job(1, 0));
    d.tick(STRIDE);
    for t in 1..=3 {
        b.frame_out(t);
    }
    d.tick(STRIDE * 2);
    assert!(streams.statistics.take_frame().is_none());
    assert_eq!(d.counters().unwrap().errors, 1);
}

#[test]
fn statistics_torn_by_a_frame_leaving_are_not_delivered() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    post(&streams.config, &job(1, 0));
    d.tick(STRIDE);
    b.frame_out(1);
    b.frame_out(2);
    b.with(|b| b.frame_during_read = true);
    d.tick(STRIDE * 2);
    assert!(streams.statistics.take_frame().is_none());
    assert_eq!(d.counters().unwrap().errors, 1);
}

#[test]
fn a_job_with_no_frames_running_times_out_and_the_next_goes() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    post(&streams.config, &job(1, 0));
    post(&streams.config, &job(2, 0));
    d.tick(STRIDE);
    d.tick(STRIDE + JOB_TIMEOUT_NS);
    assert_eq!(d.counters().unwrap().errors, 0, "not yet");
    d.tick(STRIDE + JOB_TIMEOUT_NS + 1);
    assert_eq!(d.counters().unwrap().errors, 1);
    assert_eq!(b.writes().len(), 4, "the second job went");
}

#[test]
fn with_no_statistics_buffer_free_the_job_is_an_error() {
    let streams = Streams {
        config: MockStream::with_bytes(2, CONFIG_BYTES),
        statistics: MockStream::with_bytes(1, STATISTICS_BYTES),
    };
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    for (i, t) in [(1u64, 1u64), (2, 10)] {
        post(&streams.config, &job(i, 0));
        d.tick(STRIDE * t);
        b.frame_out(t);
        b.frame_out(t + 1);
        d.tick(STRIDE * (t + 1));
    }
    // The consumer never gave the one buffer back.
    assert_eq!(streams.statistics.take_frame().unwrap().identity, 1);
    assert!(streams.statistics.take_frame().is_none());
    let c = d.counters().unwrap();
    assert_eq!((c.jobs, c.errors), (1, 1));
}

#[test]
fn layouts_are_refused_because_this_driver_has_no_input_and_no_outputs() {
    let (mut d, _b) = driver(&Streams::new());
    assert_eq!(d.input_layout().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(d.output_layout(0).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.output_layout(3).unwrap_err(), Status::NOT_FOUND);
    d.start().unwrap();
    assert_eq!(d.input_layout().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(d.output_layout(0).unwrap_err(), Status::NOT_FOUND);
}

#[test]
fn a_job_naming_a_config_buffer_the_block_holds_waits_on_the_ring() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    post(&streams.config, &job(1, 0));
    // Posted while job 1 holds buffer 0: the consumer writes nothing to it.
    post(&streams.config, &job(2, 0));
    d.tick(STRIDE);
    assert_eq!(b.writes(), [(AE_V_SIZE, 2), (ISP_CTRL, CTRL_GEN_CFG_UPD)]);
    d.tick(STRIDE * 2);
    d.tick(STRIDE * 3);
    // Job 2 is still on the ring: not taken, not dropped.
    assert_eq!(b.writes().len(), 2);
    assert_eq!(d.counters().unwrap().dropped, 0);
    b.frame_out(1);
    b.frame_out(2);
    d.tick(STRIDE * 4);
    assert_eq!(streams.statistics.take_frame().unwrap().identity, 1);
    // Job 2 went once job 1 was done, reading buffer 0 as it was.
    assert_eq!(
        b.writes(),
        [
            (AE_V_SIZE, 2),
            (ISP_CTRL, CTRL_GEN_CFG_UPD),
            (AE_V_SIZE, 2),
            (ISP_CTRL, CTRL_GEN_CFG_UPD)
        ]
    );
    assert_eq!(d.counters().unwrap().dropped, 0);
}

#[test]
fn a_config_buffer_rewritten_after_its_job_completes_is_read_by_the_next_job() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    post(&streams.config, &job(1, 0));
    d.tick(STRIDE);
    b.frame_out(1);
    b.frame_out(2);
    d.tick(STRIDE * 2);
    assert_eq!(streams.statistics.take_frame().unwrap().identity, 1);
    // Job 1 is complete: the consumer may write buffer 0 again and name it.
    streams.config.fill(0, &list(&[(AE_V_SIZE, 9)]));
    post(&streams.config, &job(2, 0));
    d.tick(STRIDE * 3);
    assert_eq!(
        b.writes(),
        [
            (AE_V_SIZE, 2),
            (ISP_CTRL, CTRL_GEN_CFG_UPD),
            (AE_V_SIZE, 9),
            (ISP_CTRL, CTRL_GEN_CFG_UPD)
        ]
    );
}

// ---- Failures.

#[test]
fn a_failed_request_reopens_the_device_with_capped_backoff_and_never_gives_up() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    streams.config.fill(0, &list(&[(AE_V_SIZE, 2)]));
    post(&streams.config, &job(1, 0));
    d.tick(1);
    b.with(|b| {
        b.fail_event = Some(Status::IO);
        b.fail_open = Some(Status::IO);
    });
    assert_eq!(d.tick(2), Some(2 + backoff_ns(0)));
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.counters().unwrap().errors, 1, "the job with the block");
    let mut now = 2 + backoff_ns(0);
    let mut delays = Vec::new();
    for _ in 0..8 {
        let next = d.tick(now).unwrap();
        delays.push(next - now);
        now = next;
    }
    assert_eq!(
        delays,
        vec![
            500_000_000,
            1_000_000_000,
            2_000_000_000,
            4_000_000_000,
            8_000_000_000,
            8_000_000_000,
            8_000_000_000,
            8_000_000_000
        ]
    );
    // The device comes back.
    b.with(|b| b.fail_open = None);
    assert_eq!(d.tick(now), Some(now + 1_000_000));
    assert_eq!(d.health(), Health::Ok);
    assert_eq!(b.with(|b| b.opened), 2);
    post(&streams.config, &job(2, 0));
    d.tick(now + 1);
    assert_eq!(b.writes().len(), 4);
    assert_eq!(d.counters().unwrap().dropped, 0);
}

#[test]
fn stop_ends_recovery() {
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    b.with(|b| {
        b.fail_event = Some(Status::IO);
        b.fail_open = Some(Status::IO);
    });
    d.tick(1);
    assert!(matches!(d.health(), Health::Degraded(_)));
    d.stop().unwrap();
    assert_eq!(d.tick(10_000_000_000), None);
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(u32::MAX), 8_000_000_000);
}

#[test]
fn counters_start_again_at_each_start() {
    let streams = Streams::new();
    let (mut d, _b) = running(&streams);
    streams.config.post_job(&[1]);
    d.tick(1);
    assert_eq!(d.counters().unwrap().dropped, 1);
    d.stop().unwrap();
    d.start().unwrap();
    assert_eq!(d.counters().unwrap(), Counters::default());
}

// ---- Allocation.

#[test]
fn the_steady_state_allocates_nothing() {
    const JOBS: u64 = 2000;
    const WARM: u64 = 8;
    let streams = Streams::new();
    let (mut d, b) = running(&streams);
    let writes: Vec<_> = (0..64).map(|i| (AE_V_SIZE, i * 2)).collect();
    for buffer in 0..4 {
        streams.config.fill(buffer, &list(&writes));
    }
    set_statistics(&b, 3);
    // Everything the stand-ins and the mocked streams hold is made before measuring.
    for i in 0..JOBS {
        post(&streams.config, &job(i, (i % 4) as u32));
    }
    let mut now = STRIDE;
    let mut one = |d: &mut K230Isp, i: u64| {
        d.tick(now);
        b.frame_out(2 * i);
        b.frame_out(2 * i + 1);
        d.tick(now + 1);
        now += 2;
        let f = streams.statistics.take_frame().unwrap();
        streams.statistics.return_buffer(f.buffer);
        b.with(|b| b.writes.clear());
    };
    for i in 0..WARM {
        one(&mut d, i);
    }
    let ((), allocated) = allocations(|| {
        for i in WARM..JOBS {
            one(&mut d, i);
        }
    });
    assert_eq!(
        allocated, 0,
        "job intake, the config read and check, 65 writes, events, 43 reads, write, publish"
    );
    let c = d.counters().unwrap();
    assert_eq!((c.jobs, c.dropped, c.errors), (JOBS, 0, 0));
}
