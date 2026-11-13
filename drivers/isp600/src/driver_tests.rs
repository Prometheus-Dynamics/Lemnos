//! The driver against a stand-in for the vendor kernel's nodes and mocked streams: every expected
//! value here is written out by hand.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, MAX_STREAM_NAME, NO_BUFFER,
};
use lemnos_contract_isp_isp600::{
    IspIsp600, LAYOUT_LOAD, LAYOUT_STATISTICS, LOAD_RGB_TABLE, Region as WireRegion,
    STATISTICS_HISTOGRAM_0,
};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamEndpoint, StreamFrame};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;
use lemnos_v4l2::FourCc;

use crate::driver::backoff_ns;
use crate::hw::{Frame, Granted, Hardware, Statistics, Wanted};
use crate::layout::{self, ADDRESS_WORDS, EXTENT_WORDS, LOAD_BYTES, STATISTICS_BYTES};
use crate::{DEFAULT_SETTLE, Isp600, Isp600Needs, Options};

/// 64x32 NV12: lines of 64 bytes, chroma after luma.
const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
const IMAGE: usize = 64 * 32 * 3 / 2;
const NV12: u32 = u32::from_le_bytes(*b"NV12");
/// Settle used by most tests, to keep them short.
const SETTLE: u8 = 1;

/// What the kernel does, shared with the test.
#[derive(Default)]
struct Kernel {
    opened: Vec<Wanted>,
    streamed: Vec<u32>,
    stopped: u32,
    /// How the capture node answers the format; `None` answers as asked.
    answer: Option<Granted>,
    fail_open: Option<Status>,
    fail_dequeue: Option<Status>,
    fail_load: Option<Status>,
    /// Statistics the kernel finished: frame number and bytes.
    statistics: VecDeque<(u32, Vec<u8>)>,
    /// Images the capture node filled.
    images: VecDeque<(Frame, Vec<u8>)>,
    requeued: Vec<u32>,
    /// Every load image handed in, whole, unless `count_only`.
    loads: Vec<Vec<u8>>,
    /// Counts loads and keeps the last in `last`, which allocates nothing once sized.
    count_only: bool,
    load_count: u32,
    last: Vec<u8>,
}

#[derive(Clone, Default)]
struct Handle(Arc<Mutex<Kernel>>);

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut Kernel) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// The kernel finishes a frame: its statistics and its image, numbered `n`.
    fn frame(&self, n: u32, stats: &[u8], image: &[u8]) {
        self.with(|k| {
            k.statistics.push_back((n, stats.to_vec()));
            let frame = Frame {
                index: n % 4,
                sequence: n,
                timestamp_ns: u64::from(n) * 1000,
                bad: false,
                planes: 1,
            };
            k.images.push_back((frame, image.to_vec()));
        });
    }

    fn loads(&self) -> Vec<Vec<u8>> {
        self.with(|k| k.loads.clone())
    }
}

/// The kernel, and the bytes of the image dequeued last, held here so they can be lent.
struct Hw {
    kernel: Arc<Mutex<Kernel>>,
    current: Vec<u8>,
}

impl Hardware for Hw {
    fn open(&mut self, wanted: &Wanted) -> Result<Granted> {
        let mut k = self.kernel.lock().unwrap();
        if let Some(status) = k.fail_open {
            return Err(status);
        }
        k.opened.push(*wanted);
        Ok(k.answer.unwrap_or(Granted {
            fourcc: wanted.fourcc,
            width: wanted.width,
            height: wanted.height,
            stride: wanted.stride,
            size: wanted.size,
        }))
    }

    fn stream_on(&mut self, buffers: u32) -> Result<()> {
        self.kernel.lock().unwrap().streamed.push(buffers);
        Ok(())
    }

    fn stop(&mut self) {
        self.kernel.lock().unwrap().stopped += 1;
    }

    fn load(&mut self, image: layout::Checked<'_>) -> Result<()> {
        let mut k = self.kernel.lock().unwrap();
        if let Some(status) = k.fail_load.take() {
            return Err(status);
        }
        k.load_count += 1;
        if k.count_only {
            k.last.clear();
            k.last.extend_from_slice(image.bytes());
        } else {
            k.loads.push(image.bytes().to_vec());
        }
        Ok(())
    }

    fn statistics(&mut self, into: &mut [u8; STATISTICS_BYTES]) -> Result<Option<Statistics>> {
        let mut k = self.kernel.lock().unwrap();
        let Some((frame_number, data)) = k.statistics.pop_front() else {
            return Ok(None);
        };
        into[..data.len()].copy_from_slice(&data);
        Ok(Some(Statistics {
            frame_number,
            bytes: data.len(),
        }))
    }

    fn dequeue(&mut self) -> Result<Option<Frame>> {
        let mut k = self.kernel.lock().unwrap();
        if let Some(status) = k.fail_dequeue.take() {
            return Err(status);
        }
        let Some((frame, data)) = k.images.pop_front() else {
            return Ok(None);
        };
        // Into memory made before, as the kernel's mappings are.
        self.current.clear();
        self.current.extend_from_slice(&data);
        Ok(Some(frame))
    }

    fn payload(&self, _frame: &Frame, plane: usize) -> Result<&[u8]> {
        if plane != 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(&self.current)
    }

    fn requeue(&mut self, index: u32) -> Result<()> {
        self.kernel.lock().unwrap().requeued.push(index);
        Ok(())
    }
}

/// The streams, as the test holds them.
#[derive(Clone)]
struct Streams {
    config: MockStream,
    statistics: MockStream,
    output0: MockStream,
}

impl Streams {
    fn new() -> Self {
        Self {
            config: MockStream::with_bytes(4, LOAD_BYTES),
            statistics: MockStream::with_bytes(4, STATISTICS_BYTES),
            output0: MockStream::with_bytes(3, IMAGE),
        }
    }

    fn needs(&self, config: StreamHandle) -> Isp600Needs {
        Isp600Needs {
            config,
            statistics: lemnos_core::Held::boxed(Box::new(self.statistics.clone())),
            output0: lemnos_core::Held::boxed(Box::new(self.output0.clone())),
        }
    }
}

/// Runs before each read of the config ring while set: the consumer acting between the driver's
/// looks.
type Hook = Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>;

/// A consumer that scribbles over a config buffer right after the driver read it, as a caller
/// racing the driver would.
struct Racing {
    inner: MockStream,
    scribble: Vec<u8>,
    hook: Hook,
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
        if let Some(hook) = self.hook.lock().unwrap().as_mut() {
            hook();
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

fn driver_on(streams: &Streams, config: StreamHandle, settle: u8) -> (Isp600, Handle) {
    let handle = Handle::default();
    let hw = Hw {
        kernel: Arc::clone(&handle.0),
        current: Vec::with_capacity(IMAGE * 2),
    };
    let options = Options::new(0, 4, settle).unwrap();
    (
        Isp600::with_hardware(Box::new(hw), streams.needs(config), options),
        handle,
    )
}

fn driver(streams: &Streams) -> (Isp600, Handle) {
    driver_on(
        streams,
        lemnos_core::Held::boxed(Box::new(streams.config.clone())),
        SETTLE,
    )
}

fn input() -> IspInput {
    IspInput {
        width: 1920,
        height: 1080,
        bit_depth: 10,
        bayer: BayerOrder::Rggb,
    }
}

fn nv12() -> IspOutput {
    IspOutput {
        width: WIDTH,
        height: HEIGHT,
        fourcc: NV12,
        pad_bytes: 0,
    }
}

fn configured(d: &mut Isp600) {
    d.set_input(input()).unwrap();
    d.set_output(0, nv12()).unwrap();
}

fn running(streams: &Streams) -> (Isp600, Handle) {
    let (mut d, kernel) = driver(streams);
    configured(&mut d);
    d.start().unwrap();
    (d, kernel)
}

fn post(stream: &MockStream, job: &IspJob) {
    let mut buf = [0; 128];
    let mut w = Writer::new(&mut buf);
    job.encode(&mut w).unwrap();
    let n = w.len();
    stream.post_job(&buf[..n]);
}

fn job(identity: u64, config_buffer: u32, output0: u32) -> IspJob {
    IspJob {
        identity,
        layout: LAYOUT_LOAD,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs: [output0, NO_BUFFER, NO_BUFFER, NO_BUFFER],
    }
}

/// A load image with every byte set but the address and extent words.
fn image(seed: u8) -> Vec<u8> {
    let mut image: Vec<u8> = (0..LOAD_BYTES)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed) | 1)
        .collect();
    for &w in ADDRESS_WORDS.iter().chain(&EXTENT_WORDS) {
        image[w..w + 4].fill(0);
    }
    image
}

fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

// ---- What the ISP says it takes.

#[test]
fn info_and_layouts_are_the_blocks() {
    let (mut d, _k) = driver(&Streams::new());
    let info = CameraIsp::info(&mut d).unwrap();
    assert_eq!(info.max_outputs, 1);
    assert_eq!((info.max_width, info.max_height), (4800, 4800));
    assert_eq!(info.config_layouts, 1);
    assert_eq!(info.statistics_layout, LAYOUT_STATISTICS);
    assert_eq!(info.statistics_bytes, 40192);
    let c = d.config_layout(0).unwrap();
    assert_eq!((c.layout, c.bytes), (LAYOUT_LOAD, 30464));
    assert_eq!(d.config_layout(1).unwrap_err(), Status::NOT_FOUND);

    let own = IspIsp600::info(&mut d).unwrap();
    assert_eq!(own.layout_version, 1);
    assert_eq!((own.load_bytes, own.statistics_bytes), (30464, 40192));
    assert_eq!((own.load_regions, own.statistics_regions), (5, 15));
    assert_eq!(own.settle_frames, SETTLE);
    assert_eq!(
        d.load_region(LOAD_RGB_TABLE).unwrap(),
        WireRegion {
            offset: 0x4e00,
            bytes: 0x1200
        }
    );
    assert_eq!(d.load_region(5).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(
        d.statistics_region(STATISTICS_HISTOGRAM_0).unwrap(),
        WireRegion {
            offset: 0x5a80,
            bytes: 0x400
        }
    );
    assert_eq!(d.statistics_region(15).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.statistics_region(u8::MAX).unwrap_err(), Status::NOT_FOUND);
}

#[test]
fn streams_are_named_after_the_needs() {
    let (mut d, _k) = driver(&Streams::new());
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = d.config_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"config");
    let n = d.statistics_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"statistics");
    let n = d.output_stream(0, &mut out).unwrap();
    assert_eq!(&out[..n], b"output0");
    assert_eq!(d.output_stream(1, &mut out).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.input_stream(&mut out).unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(
        d.config_stream(&mut [0u8; 3]).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
}

#[test]
fn an_input_the_isp_cannot_take_is_refused() {
    let (mut d, _k) = driver(&Streams::new());
    for bad in [
        IspInput {
            bit_depth: 9,
            ..input()
        },
        IspInput {
            bayer: BayerOrder::Mono,
            ..input()
        },
        IspInput {
            width: 31,
            ..input()
        },
        IspInput {
            height: 4801,
            ..input()
        },
        IspInput {
            width: u32::MAX,
            ..input()
        },
    ] {
        assert_eq!(d.set_input(bad).unwrap_err(), Status::INVALID_ARGUMENT);
    }
    d.set_input(input()).unwrap();
}

#[test]
fn an_output_is_laid_out_by_its_format() {
    let (mut d, _k) = driver(&Streams::new());
    let layout = d.set_output(0, nv12()).unwrap();
    assert_eq!((layout.stride, layout.size), (64, 3072));
    let nv16 = IspOutput {
        fourcc: u32::from_le_bytes(*b"NV16"),
        pad_bytes: 100,
        ..nv12()
    };
    let layout = d.set_output(0, nv16).unwrap();
    assert_eq!((layout.stride, layout.size), (64, 4196));
    for bad in [
        IspOutput {
            fourcc: u32::from_le_bytes(*b"RG10"),
            ..nv12()
        },
        IspOutput {
            width: 63,
            ..nv12()
        },
        IspOutput {
            height: 4802,
            ..nv12()
        },
        IspOutput {
            pad_bytes: u32::MAX,
            ..nv12()
        },
    ] {
        assert_eq!(d.set_output(0, bad).unwrap_err(), Status::INVALID_ARGUMENT);
    }
    assert_eq!(d.set_output(1, nv12()).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.clear_output(1).unwrap_err(), Status::NOT_FOUND);
    d.clear_output(0).unwrap();
}

// ---- Starting.

#[test]
fn start_needs_an_input_and_an_output() {
    let (mut d, k) = driver(&Streams::new());
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.set_input(input()).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.set_output(0, nv12()).unwrap();
    d.start().unwrap();
    k.with(|k| {
        assert_eq!(
            k.opened,
            vec![Wanted {
                input: 0,
                fourcc: FourCc::new(b"NV12"),
                width: 64,
                height: 32,
                stride: 64,
                size: 3072,
            }]
        );
        assert_eq!(k.streamed, vec![4]);
    });
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    assert_eq!(d.set_input(input()).unwrap_err(), Status::BUSY);
    assert_eq!(d.set_output(0, nv12()).unwrap_err(), Status::BUSY);
    assert_eq!(d.clear_output(0).unwrap_err(), Status::BUSY);
    d.stop().unwrap();
    d.set_input(input()).unwrap();
}

#[test]
fn a_capture_node_that_lays_the_image_out_otherwise_is_refused() {
    let (mut d, k) = driver(&Streams::new());
    configured(&mut d);
    k.with(|k| {
        k.answer = Some(Granted {
            fourcc: FourCc::new(b"NV12"),
            width: 64,
            height: 32,
            stride: 128,
            size: 6144,
        });
    });
    assert_eq!(d.start().unwrap_err(), Status::NOT_SUPPORTED);
    k.with(|k| assert!(k.streamed.is_empty()));
}

#[test]
fn a_stream_with_more_buffers_than_the_driver_tracks_is_refused() {
    let streams = Streams {
        output0: MockStream::with_bytes(65, 0),
        ..Streams::new()
    };
    let (mut d, _k) = driver(&streams);
    configured(&mut d);
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
}

// ---- Jobs.

#[test]
fn a_job_carries_its_image_in_and_its_statistics_and_image_out() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    let load = image(1);
    streams.config.fill(2, &load);
    post(&streams.config, &job(77, 2, 1));
    d.tick(1);
    assert_eq!(k.loads(), vec![load]);

    // One frame finishes on the earlier parameters and goes by; the next is the job's.
    let stats = pattern(2, STATISTICS_BYTES);
    let picture = pattern(3, IMAGE);
    k.frame(4, &pattern(9, STATISTICS_BYTES), &pattern(9, IMAGE));
    d.tick(2);
    assert!(streams.statistics.take_frame().is_none(), "frame 4 went by");
    assert!(streams.output0.take_frame().is_none());
    k.frame(5, &stats, &picture);
    k.frame(6, &pattern(8, STATISTICS_BYTES), &pattern(8, IMAGE));
    d.tick(3);

    let s = streams.statistics.take_frame().unwrap();
    assert_eq!((s.identity, s.sequence), (77, 5));
    assert_eq!(s.bytes as usize, STATISTICS_BYTES);
    assert_eq!(streams.statistics.contents(s.buffer), stats);
    assert!(
        streams.statistics.take_frame().is_none(),
        "frame 6 had no job"
    );
    let o = streams.output0.take_frame().unwrap();
    assert_eq!((o.buffer, o.identity, o.sequence), (1, 77, 5));
    assert_eq!((o.bytes as usize, o.timestamp_ns), (IMAGE, 5000));
    assert_eq!(streams.output0.contents(1), picture);
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            jobs: 1,
            dropped: 0,
            errors: 0
        }
    );
    // Every image went back to the node.
    k.with(|k| assert_eq!(k.requeued, vec![0, 1, 2]));
}

#[test]
fn the_default_settle_lets_three_frames_go_by() {
    let streams = Streams::new();
    let (mut d, k) = driver_on(
        &streams,
        lemnos_core::Held::boxed(Box::new(streams.config.clone())),
        DEFAULT_SETTLE,
    );
    configured(&mut d);
    d.start().unwrap();
    d.tick(0);
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(5, 0, 0));
    d.tick(1);
    for n in 10..13 {
        k.frame(n, &[1; 16], &[1; 16]);
    }
    d.tick(2);
    assert!(streams.statistics.take_frame().is_none());
    k.frame(13, &[2; 16], &[2; 16]);
    d.tick(3);
    assert_eq!(streams.statistics.take_frame().unwrap().sequence, 13);
    assert_eq!(streams.output0.take_frame().unwrap().sequence, 13);
}

#[test]
fn the_driver_hands_in_its_copy_when_the_caller_changes_the_buffer_after_the_read() {
    let streams = Streams::new();
    // Ones in the address words, too: what the caller writes afterwards is never read.
    let config = Racing {
        inner: streams.config.clone(),
        scribble: vec![0xff; LOAD_BYTES],
        hook: Hook::default(),
    };
    let (mut d, k) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)), SETTLE);
    configured(&mut d);
    d.start().unwrap();
    d.tick(0);
    let load = image(5);
    streams.config.fill(0, &load);
    post(&streams.config, &job(1, 0, NO_BUFFER));
    d.tick(1);
    assert_eq!(streams.config.contents(0), vec![0xff; LOAD_BYTES]);
    assert_eq!(k.loads(), vec![load]);
}

#[test]
fn an_image_naming_memory_never_reaches_the_kernel() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    let mut i = 1;
    for &word in &ADDRESS_WORDS {
        let mut bad = image(3);
        bad[word + 3] = 0x80;
        streams.config.fill(0, &bad);
        post(&streams.config, &job(9, 0, 0));
        d.tick(i);
        i += 1;
    }
    assert!(k.loads().is_empty());
    assert_eq!(d.counters().unwrap().dropped, ADDRESS_WORDS.len() as u64);
    // The same buffer, cleaned, goes in.
    streams.config.fill(0, &image(3));
    post(&streams.config, &job(9, 0, 0));
    d.tick(i);
    assert_eq!(k.loads(), vec![image(3)]);
}

#[test]
fn a_job_naming_what_the_driver_does_not_hold_is_dropped() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(0, &image(1));
    for bad in [
        job(1, 4, NO_BUFFER),
        job(1, 0, 3),
        IspJob {
            layout: LAYOUT_STATISTICS,
            ..job(1, 0, 0)
        },
        IspJob {
            input_buffer: 1,
            ..job(1, 0, 0)
        },
        IspJob {
            outputs: [0, 1, NO_BUFFER, NO_BUFFER],
            ..job(1, 0, 0)
        },
    ] {
        post(&streams.config, &bad);
    }
    // Garbage on the ring.
    streams.config.post_job(&[1, 2, 3]);
    d.tick(1);
    assert!(k.loads().is_empty());
    assert_eq!(d.counters().unwrap().dropped, 6);

    // An output buffer the consumer still has.
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    let mut output = streams.output0.clone();
    assert_eq!(output.next_buffer().unwrap(), Some(0));
    d.tick(0);
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    assert!(k.loads().is_empty());
    assert_eq!(d.counters().unwrap().dropped, 1);
}

#[test]
fn a_buffer_given_back_and_named_by_a_later_job_in_the_same_tick_is_held() {
    let streams = Streams::new();
    let config = Racing {
        inner: streams.config.clone(),
        scribble: Vec::new(),
        hook: Hook::default(),
    };
    let hook = Arc::clone(&config.hook);
    let (mut d, k) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)), SETTLE);
    configured(&mut d);
    d.start().unwrap();
    d.tick(0);
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    // Job 1 finishes after its settle frame, and its frame is published on output buffer 0.
    k.frame(4, &pattern(9, STATISTICS_BYTES), &pattern(9, IMAGE));
    d.tick(2);
    k.frame(5, &pattern(2, STATISTICS_BYTES), &pattern(3, IMAGE));
    d.tick(3);
    let s = streams.statistics.take_frame().unwrap();
    streams.statistics.return_buffer(s.buffer);
    assert_eq!(d.counters().unwrap().jobs, 1);

    // Job 2 names a config buffer past the ring, so it is dropped. Then, before job 3 is read,
    // the consumer takes job 1's frame, gives its buffer back and names it in job 3.
    streams.config.fill(2, &image(2));
    post(&streams.config, &job(2, 4, NO_BUFFER));
    let mut reads = 0;
    let consumer = streams.clone();
    *hook.lock().unwrap() = Some(Box::new(move || {
        reads += 1;
        if reads == 2 {
            let frame = consumer.output0.take_frame().unwrap();
            consumer.output0.return_buffer(frame.buffer);
            post(&consumer.config, &job(3, 2, frame.buffer));
        }
    }));
    d.tick(4);
    // Job 3 reached the kernel; only job 2 was dropped.
    assert_eq!(k.loads(), vec![image(1), image(2)]);
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            jobs: 1,
            dropped: 1,
            errors: 0
        }
    );
    // And its frame comes out on the buffer the consumer gave back.
    let picture = pattern(5, IMAGE);
    k.frame(6, &pattern(4, STATISTICS_BYTES), &pattern(4, IMAGE));
    d.tick(5);
    k.frame(7, &pattern(5, STATISTICS_BYTES), &picture);
    d.tick(6);
    let o = streams.output0.take_frame().unwrap();
    assert_eq!((o.buffer, o.identity, o.sequence), (0, 3, 7));
    assert_eq!(streams.output0.contents(0), picture);
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            jobs: 2,
            dropped: 1,
            errors: 0
        }
    );
}

#[test]
fn bytes_that_are_no_job_fault_the_consumer_and_the_next_one_is_served() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(2, &image(1));
    streams.config.post_job(&[0xff; 3]);
    post(&streams.config, &job(1, 2, 1));
    d.tick(1);
    // Faulted at the bad bytes: the job behind them never reached the kernel.
    assert_eq!(streams.config.faults(), 1);
    assert!(k.loads().is_empty());
    assert_eq!(d.counters().unwrap().dropped, 1);
    assert_eq!(streams.statistics.faults() + streams.output0.faults(), 0);
    // The config buffers held under it were given up; taken again, the next consumer's job runs.
    let load = image(4);
    streams.config.fill(2, &load);
    post(&streams.config, &job(2, 2, 1));
    d.tick(2);
    assert_eq!(k.loads(), vec![load]);
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn one_job_runs_at_a_time_and_the_next_waits_on_the_ring() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(0, &image(1));
    streams.config.fill(1, &image(2));
    post(&streams.config, &job(1, 0, NO_BUFFER));
    post(&streams.config, &job(2, 1, NO_BUFFER));
    d.tick(1);
    assert_eq!(
        k.loads(),
        vec![image(1)],
        "the kernel keeps one image waiting"
    );
    k.frame(1, &[1; 8], &[1; 8]);
    k.frame(2, &[2; 8], &[2; 8]);
    d.tick(2);
    assert_eq!(streams.statistics.take_frame().unwrap().identity, 1);
    assert_eq!(
        k.loads(),
        vec![image(1), image(2)],
        "taken once the first was done"
    );
    assert_eq!(d.counters().unwrap().jobs, 1);
}

#[test]
fn a_bad_image_or_one_too_big_counts_as_an_error() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    k.frame(1, &[0; 8], &[0; 8]);
    k.with(|k| {
        k.statistics.push_back((2, vec![1; 8]));
        let frame = Frame {
            index: 2,
            sequence: 2,
            timestamp_ns: 0,
            bad: true,
            planes: 1,
        };
        k.images.push_back((frame, vec![1; 8]));
    });
    d.tick(2);
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(streams.statistics.take_frame().unwrap().identity, 1);
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            jobs: 0,
            dropped: 0,
            errors: 1
        }
    );

    // The buffer the bad frame was for is still the driver's: the next job uses it.
    streams.config.fill(0, &image(2));
    post(&streams.config, &job(2, 0, 0));
    d.tick(3);
    k.frame(3, &[0; 8], &[0; 8]);
    k.frame(4, &[0; 8], &vec![1; IMAGE + 1]);
    d.tick(4);
    assert!(
        streams.output0.take_frame().is_none(),
        "past the agreed size"
    );
    assert_eq!(d.counters().unwrap().errors, 2);
}

#[test]
fn a_failing_node_recovers_with_backoff_and_ends_the_waiting_job() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(1, 0, NO_BUFFER));
    d.tick(1);
    k.with(|k| {
        k.fail_dequeue = Some(Status::IO);
        k.fail_open = Some(Status::NO_DEVICE);
    });
    let mut now = 1_000;
    let due = d.tick(now).unwrap();
    assert_eq!(due, now + backoff_ns(0));
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.counters().unwrap().errors, 1, "the waiting job");
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    // Each failed attempt doubles the wait, to 8 s.
    now = due;
    let mut waits = Vec::new();
    for _ in 0..7 {
        let next = d.tick(now).unwrap();
        waits.push(next - now);
        now = next;
    }
    assert_eq!(
        waits,
        [
            500_000_000,
            1_000_000_000,
            2_000_000_000,
            4_000_000_000,
            8_000_000_000,
            8_000_000_000,
            8_000_000_000
        ]
    );
    k.with(|k| k.fail_open = None);
    let next = d.tick(now).unwrap();
    assert_eq!(next, now + 1_000_000);
    assert_eq!(d.health(), Health::Ok);
    k.with(|k| assert_eq!(k.opened.len(), 2));
}

#[test]
fn a_failing_load_starts_recovery() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    k.with(|k| k.fail_load = Some(Status::IO));
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(1, 0, NO_BUFFER));
    d.tick(1);
    assert!(matches!(d.health(), Health::Degraded(_)));
}

#[test]
fn stop_ends_the_waiting_job() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(0, &image(1));
    post(&streams.config, &job(1, 0, NO_BUFFER));
    d.tick(1);
    d.stop().unwrap();
    assert_eq!(d.counters().unwrap().errors, 1);
    assert_eq!(d.tick(2), None);
    k.with(|k| assert!(k.stopped >= 2));
}

// ---- Allocation.

#[test]
fn the_steady_state_allocates_nothing() {
    const JOBS: u32 = 500;
    const WARM: u32 = 4;
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    let mut now = d.tick(0).unwrap();
    for b in 0..4 {
        streams.config.fill(b, &image(1));
    }
    let stats = vec![5u8; 2048];
    let picture = vec![6u8; IMAGE];
    // Everything the stand-ins and the mocked streams hold is made before measuring.
    for i in 0..JOBS {
        post(&streams.config, &job(u64::from(i), i % 4, i % 3));
    }
    k.with(|k| {
        k.statistics.reserve(8);
        k.images.reserve(8);
        k.requeued.reserve(4 * JOBS as usize);
        k.count_only = true;
        k.last.reserve(LOAD_BYTES);
    });
    // The first job goes in; each round after delivers one and takes the next.
    now = d.tick(now).unwrap();
    let frames: Vec<(Vec<u8>, Vec<u8>)> = (0..JOBS * 2)
        .map(|_| (stats.clone(), picture.clone()))
        .collect();
    let mut frames = frames.into_iter();
    let mut one_job = |d: &mut Isp600, now: &mut u64, n: u32| {
        // Two frames: the one that goes by and the job's, moved in without copying.
        for m in [2 * n, 2 * n + 1] {
            let (s, p) = frames.next().unwrap();
            k.with(|k| {
                k.statistics.push_back((m, s));
                k.images.push_back((
                    Frame {
                        index: m % 4,
                        sequence: m,
                        timestamp_ns: 0,
                        bad: false,
                        planes: 1,
                    },
                    p,
                ));
            });
        }
        *now = d.tick(*now).unwrap();
        let s = streams.statistics.take_frame().unwrap();
        streams.statistics.return_buffer(s.buffer);
        let o = streams.output0.take_frame().unwrap();
        streams.output0.return_buffer(o.buffer);
    };
    for n in 0..WARM {
        one_job(&mut d, &mut now, n);
    }
    let ((), allocated) = allocations(|| {
        for n in WARM..JOBS - 1 {
            one_job(&mut d, &mut now, n);
        }
    });
    assert_eq!(
        allocated, 0,
        "job intake, the image read, check and load, statistics, dequeue, write, publish"
    );
    assert_eq!(d.counters().unwrap().jobs, u64::from(JOBS - 1));
    k.with(|k| {
        assert_eq!(k.load_count, JOBS);
        assert_eq!(k.last, image(1));
    });
}
