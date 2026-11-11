//! The driver against a stand-in for the vendor kernel's nodes and mocked streams: every expected
//! value here is written out by hand.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, MAX_STREAM_NAME, NO_BUFFER,
};
use lemnos_core::{Encode, Health, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamEndpoint, StreamFrame};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;
use lemnos_v4l2::media::{
    Entity, EntityFlags, EntityFunction, Link, LinkFlags, Pad, PadFlags, PadRef, Topology,
};
use lemnos_v4l2::subdev::{MbusCode, MbusFormat};

use crate::driver::backoff_ns;
use crate::graph::{
    ISP_ENTITY, MAINPATH_ENTITY, PARAMS_ENTITY, SELFPATH_ENTITY, STATISTICS_ENTITY,
};
use crate::hw::{Done, Hardware, Image, Opened, QUEUES, Queue, Setup};
use crate::{
    Generation, LAYOUT_ISP21_PARAMS, LAYOUT_ISP30_PARAMS, LAYOUT_ISP30_STATISTICS, MAX_PENDING,
    Options, Rkisp, RkispNeeds,
};

/// ISP30's parameter and statistics sizes.
const PARAMS: usize = 10921;
const STATS: usize = 16172;
/// 64x32 NV12, as the stand-in kernel lays it out: lines of 64 bytes, chroma after luma.
const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
const IMAGE: usize = 64 * 32 * 3 / 2;
const NV12: u32 = u32::from_le_bytes(*b"NV12");

/// What the kernel does, shared with the test.
struct Kernel {
    topology: Topology,
    links: Vec<(PadRef, PadRef, bool)>,
    formats: Vec<(PadRef, u32, MbusFormat)>,
    opened: Vec<Setup>,
    stopped: u32,
    /// Buffers the kernel grants on each queue.
    granted: [u32; QUEUES],
    /// How the kernel lays out images; `None` lays them out as asked would be.
    answer: Option<Image>,
    fail_open: Option<Status>,
    fail_dequeue: Option<Status>,
    /// Buffers the kernel finished, per queue, with their bytes.
    finished: [VecDeque<(Done, Vec<u8>)>; QUEUES],
    requeued: Vec<(Queue, u32)>,
    /// Each parameter buffer as queued last, and how many bytes were queued.
    queued_params: Vec<(u32, u32)>,
    params_seen: Vec<Vec<u8>>,
}

impl Default for Kernel {
    fn default() -> Self {
        Self {
            topology: graph(),
            links: Vec::new(),
            formats: Vec::new(),
            opened: Vec::new(),
            stopped: 0,
            granted: [MAX_PENDING as u32, 4, 4, 4],
            answer: None,
            fail_open: None,
            fail_dequeue: None,
            finished: Default::default(),
            requeued: Vec::with_capacity(1 << 14),
            queued_params: Vec::with_capacity(1 << 12),
            params_seen: vec![vec![0; PARAMS + 64]; MAX_PENDING],
        }
    }
}

/// The kernel: the test's handle on it, the parameter buffers' mappings and the bytes of the
/// buffer dequeued last on each queue, held here so they can be lent.
struct Hw {
    kernel: Arc<Mutex<Kernel>>,
    params: Vec<Vec<u8>>,
    current: [Vec<u8>; QUEUES],
}

#[derive(Clone, Default)]
struct Handle(Arc<Mutex<Kernel>>);

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut Kernel) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// The kernel finishes a buffer on `queue`.
    fn finish(&self, queue: Queue, index: u32, sequence: u32, data: &[u8]) {
        self.finish_flagged(queue, index, sequence, data, false);
    }

    fn finish_flagged(&self, queue: Queue, index: u32, sequence: u32, data: &[u8], bad: bool) {
        let done = Done {
            index,
            sequence,
            timestamp_ns: u64::from(sequence) * 1000,
            bad,
        };
        self.with(|k| k.finished[queue as usize].push_back((done, data.to_vec())));
    }

    /// The kernel applied parameter buffer `slot` to frame `sequence`.
    fn apply(&self, slot: u32, sequence: u32) {
        self.finish(Queue::Params, slot, sequence, &[]);
    }

    fn queued_params(&self) -> Vec<(u32, u32)> {
        self.with(|k| k.queued_params.clone())
    }

    fn params_seen(&self, slot: u32) -> Vec<u8> {
        self.with(|k| k.params_seen[slot as usize][..PARAMS].to_vec())
    }
}

impl Hardware for Hw {
    fn topology(&mut self) -> Result<Topology> {
        Ok(self.kernel.lock().unwrap().topology.clone())
    }

    fn setup_link(&mut self, source: PadRef, sink: PadRef, enable: bool) -> Result<()> {
        self.kernel
            .lock()
            .unwrap()
            .links
            .push((source, sink, enable));
        Ok(())
    }

    fn set_formats(
        &mut self,
        _t: &Topology,
        source: PadRef,
        isp: u32,
        raw: MbusFormat,
    ) -> Result<()> {
        self.kernel.lock().unwrap().formats.push((source, isp, raw));
        Ok(())
    }

    fn try_output(&mut self, _output: usize, image: Image) -> Result<Image> {
        let k = self.kernel.lock().unwrap();
        Ok(k.answer.unwrap_or(Image {
            stride: image.width,
            size: image.width * image.height * 3 / 2,
            ..image
        }))
    }

    fn open(&mut self, _t: &Topology, setup: &Setup) -> Result<Opened> {
        let mut k = self.kernel.lock().unwrap();
        if let Some(status) = k.fail_open {
            return Err(status);
        }
        k.opened.push(*setup);
        let mut buffers = k.granted;
        for (output, queue) in [(0, Queue::Main), (1, Queue::SelfPath)] {
            if setup.outputs[output].is_none() {
                buffers[queue as usize] = 0;
            }
        }
        Ok(Opened {
            buffers,
            outputs: setup
                .outputs
                .map(|o| o.map(|image| k.answer.unwrap_or(image))),
        })
    }

    fn stop(&mut self) {
        self.kernel.lock().unwrap().stopped += 1;
    }

    fn dequeue(&mut self, queue: Queue) -> Result<Option<Done>> {
        let mut k = self.kernel.lock().unwrap();
        if let Some(status) = k.fail_dequeue.take() {
            return Err(status);
        }
        let Some((done, data)) = k.finished[queue as usize].pop_front() else {
            return Ok(None);
        };
        // Into memory made before, as the kernel's mappings are.
        let current = &mut self.current[queue as usize];
        current.clear();
        current.extend_from_slice(&data);
        Ok(Some(done))
    }

    fn payload(&self, queue: Queue, _done: &Done) -> Result<&[u8]> {
        Ok(&self.current[queue as usize])
    }

    fn requeue(&mut self, queue: Queue, index: u32) -> Result<()> {
        self.kernel.lock().unwrap().requeued.push((queue, index));
        Ok(())
    }

    fn params_buffer(&mut self, index: u32) -> Result<&mut [u8]> {
        self.params
            .get_mut(index as usize)
            .map(Vec::as_mut_slice)
            .ok_or(Status::INVALID_ARGUMENT)
    }

    fn queue_params(&mut self, index: u32, bytes: u32) -> Result<()> {
        let mut k = self.kernel.lock().unwrap();
        let from = &self.params[index as usize];
        k.params_seen[index as usize].copy_from_slice(from);
        k.queued_params.push((index, bytes));
        Ok(())
    }
}

// ---- The graph: the ISP, its four nodes, the self path and one receiver, all linked.

fn graph() -> Topology {
    let entity = |id, name: &str, function| Entity {
        id,
        name: name.to_string(),
        function,
        flags: EntityFlags::empty(),
    };
    let pad = |id, entity_id, index| Pad {
        id,
        entity_id,
        flags: PadFlags::empty(),
        index,
    };
    let on = |id, source_id, sink_id| Link {
        id,
        source_id,
        sink_id,
        flags: LinkFlags::ENABLED,
    };
    Topology {
        version: 1,
        entities: vec![
            entity(1, ISP_ENTITY, EntityFunction::PROC_VIDEO_ISP),
            entity(2, PARAMS_ENTITY, EntityFunction::IO_V4L),
            entity(3, STATISTICS_ENTITY, EntityFunction::IO_V4L),
            entity(4, MAINPATH_ENTITY, EntityFunction::IO_V4L),
            entity(5, SELFPATH_ENTITY, EntityFunction::IO_V4L),
            entity(6, "rkcif-mipi-lvds", EntityFunction::PROC_VIDEO_COMPOSER),
        ],
        interfaces: vec![],
        pads: vec![
            pad(10, 1, 0),
            pad(11, 1, 1),
            pad(12, 1, 2),
            pad(13, 1, 3),
            pad(20, 2, 0),
            pad(21, 3, 0),
            pad(22, 4, 0),
            pad(23, 5, 0),
            pad(24, 6, 0),
        ],
        links: vec![
            on(1, 24, 10),
            on(2, 20, 11),
            on(3, 12, 22),
            on(4, 13, 21),
            Link {
                flags: LinkFlags::empty(),
                ..on(5, 12, 23)
            },
        ],
    }
}

// ---- The consumer's side of the streams.

/// The streams, as the test holds them.
#[derive(Clone)]
struct Streams {
    config: MockStream,
    statistics: MockStream,
    output0: MockStream,
    output1: Option<MockStream>,
}

impl Streams {
    fn new() -> Self {
        Self {
            config: MockStream::with_bytes(4, PARAMS),
            statistics: MockStream::with_bytes(4, STATS),
            output0: MockStream::with_bytes(3, IMAGE),
            output1: None,
        }
    }

    fn with_output1(self) -> Self {
        Self {
            output1: Some(MockStream::with_bytes(2, IMAGE)),
            ..self
        }
    }

    fn needs(&self, config: StreamHandle) -> RkispNeeds {
        RkispNeeds {
            config,
            statistics: lemnos_core::Held::boxed(Box::new(self.statistics.clone())),
            output0: lemnos_core::Held::boxed(Box::new(self.output0.clone())),
            output1: self
                .output1
                .clone()
                .map(|s| -> StreamHandle { lemnos_core::Held::boxed(Box::new(s)) }),
        }
    }
}

/// Runs before each read of the config ring while set: the consumer acting between the driver's
/// looks.
type Hook = Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>;

/// A consumer that scribbles over a config buffer right after the driver read it, as a caller
/// racing the driver would, and that can keep one config buffer from the driver.
struct Racing {
    inner: MockStream,
    scribble: Vec<u8>,
    withhold: Option<u32>,
    /// Jobs let through before the ring looks empty; `None` lets every job through.
    allow: Option<Arc<AtomicUsize>>,
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
        loop {
            match self.inner.next_buffer()? {
                Some(b) if Some(b) == self.withhold => {}
                other => return Ok(other),
            }
        }
    }

    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        if let Some(hook) = self.hook.lock().unwrap().as_mut() {
            hook();
        }
        if let Some(allow) = &self.allow {
            if allow.load(Ordering::Relaxed) == 0 {
                return Ok(None);
            }
            allow.fetch_sub(1, Ordering::Relaxed);
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
        if !self.scribble.is_empty() {
            self.inner.fill(buffer, &self.scribble);
        }
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

fn driver_on(streams: &Streams, config: StreamHandle) -> (Rkisp, Handle) {
    let handle = Handle::default();
    let hw = Hw {
        kernel: Arc::clone(&handle.0),
        params: vec![vec![0; PARAMS + 64]; MAX_PENDING],
        current: std::array::from_fn(|_| Vec::with_capacity(STATS * 2)),
    };
    let options = Options::new(Generation::Isp30, None, 4).unwrap();
    (
        Rkisp::with_hardware(Box::new(hw), streams.needs(config), options),
        handle,
    )
}

fn driver(streams: &Streams) -> (Rkisp, Handle) {
    driver_on(
        streams,
        lemnos_core::Held::boxed(Box::new(streams.config.clone())),
    )
}

fn input() -> IspInput {
    IspInput {
        width: 1280,
        height: 800,
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

fn configured(d: &mut Rkisp) {
    d.set_input(input()).unwrap();
    d.set_output(0, nv12()).unwrap();
}

fn running(streams: &Streams) -> (Rkisp, Handle) {
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
        layout: LAYOUT_ISP30_PARAMS,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs: [output0, NO_BUFFER, NO_BUFFER, NO_BUFFER],
    }
}

/// Bytes every one different, and different per seed.
fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed))
        .collect()
}

// ---- What the ISP says it takes.

#[test]
fn info_and_layouts_are_the_generations() {
    let streams = Streams::new();
    let (mut d, _k) = driver(&streams);
    let info = d.info().unwrap();
    assert_eq!(info.max_outputs, 1, "no self path stream");
    assert_eq!(info.config_layouts, 1);
    assert_eq!(info.statistics_layout, LAYOUT_ISP30_STATISTICS);
    assert_eq!(info.statistics_bytes, 16172);
    let c = d.config_layout(0).unwrap();
    assert_eq!((c.layout, c.bytes), (LAYOUT_ISP30_PARAMS, 10921));
    assert_eq!(d.config_layout(1).unwrap_err(), Status::NOT_FOUND);

    let (mut d, _k) = driver(&Streams::new().with_output1());
    assert_eq!(d.info().unwrap().max_outputs, 2);
}

#[test]
fn streams_are_named_after_the_needs() {
    let streams = Streams::new().with_output1();
    let (mut d, _k) = driver(&streams);
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = d.config_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"config");
    let n = d.statistics_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"statistics");
    let n = d.output_stream(0, &mut out).unwrap();
    assert_eq!(&out[..n], b"output0");
    let n = d.output_stream(1, &mut out).unwrap();
    assert_eq!(&out[..n], b"output1");
    assert_eq!(d.output_stream(2, &mut out).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.input_stream(&mut out).unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(
        d.config_stream(&mut [0u8; 3]).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
    let (mut d, _k) = driver(&Streams::new());
    assert_eq!(
        d.output_stream(1, &mut out).unwrap_err(),
        Status::NOT_FOUND,
        "the self path has no stream"
    );
}

#[test]
fn an_input_the_isp_cannot_take_is_refused() {
    let (mut d, _k) = driver(&Streams::new());
    for bad in [
        IspInput {
            bit_depth: 14,
            ..input()
        },
        IspInput {
            bit_depth: 0,
            ..input()
        },
        IspInput {
            width: 0,
            ..input()
        },
        IspInput {
            height: 8193,
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
fn an_output_is_what_the_kernel_lays_out() {
    let (mut d, k) = driver(&Streams::new().with_output1());
    let layout = d.set_output(0, nv12()).unwrap();
    assert_eq!((layout.stride, layout.size), (64, 3072));
    let padded = d
        .set_output(
            1,
            IspOutput {
                pad_bytes: 256,
                ..nv12()
            },
        )
        .unwrap();
    assert_eq!(padded.size, 3072 + 256);
    // Padding that overflows a size.
    assert_eq!(
        d.set_output(
            0,
            IspOutput {
                pad_bytes: u32::MAX,
                ..nv12()
            }
        )
        .unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    // A format the outputs don't make, a size out of range, an output past the last.
    let rgb = IspOutput {
        fourcc: u32::from_le_bytes(*b"RGB3"),
        ..nv12()
    };
    assert_eq!(d.set_output(0, rgb).unwrap_err(), Status::INVALID_ARGUMENT);
    let wide = IspOutput {
        width: 8193,
        ..nv12()
    };
    assert_eq!(d.set_output(0, wide).unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(d.set_output(2, nv12()).unwrap_err(), Status::NOT_FOUND);
    // The kernel would make another size, or a stride shorter than a line.
    k.with(|k| {
        k.answer = Some(Image {
            width: 32,
            height: 32,
            fourcc: NV12,
            stride: 32,
            size: 1536,
        });
    });
    assert_eq!(
        d.set_output(0, nv12()).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    k.with(|k| {
        k.answer = Some(Image {
            width: WIDTH,
            height: HEIGHT,
            fourcc: NV12,
            stride: 63,
            size: 3072,
        });
    });
    assert_eq!(
        d.set_output(0, nv12()).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    d.clear_output(1).unwrap();
    assert_eq!(d.clear_output(2).unwrap_err(), Status::NOT_FOUND);
}

// ---- Starting.

#[test]
fn start_needs_an_input_and_output_0() {
    let (mut d, _k) = driver(&Streams::new());
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.set_input(input()).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.set_output(0, nv12()).unwrap();
    d.start().unwrap();
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    assert_eq!(d.set_input(input()).unwrap_err(), Status::BUSY);
    assert_eq!(d.set_output(0, nv12()).unwrap_err(), Status::BUSY);
    assert_eq!(d.clear_output(0).unwrap_err(), Status::BUSY);
    d.stop().unwrap();
    d.set_input(input()).unwrap();
}

#[test]
fn start_links_the_path_sets_the_formats_and_opens_the_nodes() {
    let streams = Streams::new().with_output1();
    let (mut d, k) = driver(&streams);
    configured(&mut d);
    d.set_output(1, nv12()).unwrap();
    d.start().unwrap();
    let at = |entity, index| PadRef { entity, index };
    k.with(|k| {
        // Only the self path's link was off.
        assert_eq!(k.links, vec![(at(1, 2), at(5, 0), true)]);
        assert_eq!(k.formats.len(), 1);
        let (source, isp, raw) = k.formats[0];
        assert_eq!((source, isp), (at(6, 0), 1));
        assert_eq!((raw.width, raw.height), (1280, 800));
        assert_eq!(raw.code, MbusCode::SRGGB10_1X10);
        let setup = k.opened[0];
        assert_eq!(setup.params_bytes, 10921);
        assert_eq!(setup.statistics_bytes, 16172);
        assert_eq!(setup.params_buffers, MAX_PENDING as u32);
        assert_eq!(setup.buffers, 4);
        assert_eq!(setup.nodes.self_path, Some(5));
        let image = setup.outputs[0].unwrap();
        assert_eq!((image.width, image.stride, image.size), (64, 64, 3072));
    });
}

#[test]
fn a_kernel_that_lays_an_image_out_otherwise_than_agreed_is_refused() {
    let streams = Streams::new();
    let (mut d, k) = driver(&streams);
    configured(&mut d);
    k.with(|k| {
        k.answer = Some(Image {
            width: WIDTH,
            height: HEIGHT,
            fourcc: NV12,
            stride: 128,
            size: 6144,
        });
    });
    assert_eq!(d.start().unwrap_err(), Status::NOT_SUPPORTED);
}

#[test]
fn a_stream_with_more_buffers_than_the_driver_tracks_is_refused() {
    let streams = Streams {
        config: MockStream::with_bytes(65, 0),
        ..Streams::new()
    };
    let (mut d, _k) = driver(&streams);
    configured(&mut d);
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
}

// ---- Jobs.

#[test]
fn a_job_carries_its_parameters_in_and_its_statistics_and_image_out() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    let params = pattern(1, PARAMS);
    streams.config.fill(2, &params);
    post(&streams.config, &job(77, 2, 1));
    d.tick(1);
    assert_eq!(k.queued_params(), vec![(0, 10921)]);
    assert_eq!(k.params_seen(0), params);

    // The kernel applied them to frame 5; frame 4 finishes first, with no job for it.
    let stats = pattern(2, 1000);
    let image = pattern(3, IMAGE);
    k.apply(0, 5);
    k.finish(Queue::Statistics, 1, 4, &pattern(9, 1000));
    k.finish(Queue::Main, 0, 4, &pattern(9, IMAGE));
    k.finish(Queue::Statistics, 2, 5, &stats);
    k.finish(Queue::Main, 3, 5, &image);
    d.tick(2);

    let s = streams.statistics.take_frame().unwrap();
    assert_eq!((s.identity, s.sequence, s.bytes), (77, 5, 1000));
    assert_eq!(s.timestamp_ns, 5000);
    assert_eq!(&streams.statistics.contents(s.buffer)[..1000], &stats[..]);
    assert!(
        streams.statistics.take_frame().is_none(),
        "frame 4 had no job"
    );
    let o = streams.output0.take_frame().unwrap();
    assert_eq!((o.buffer, o.identity, o.sequence), (1, 77, 5));
    assert_eq!(o.bytes as usize, IMAGE);
    assert_eq!(streams.output0.contents(1), image);
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            jobs: 1,
            dropped: 0,
            errors: 0
        }
    );
    // Every kernel buffer went back.
    k.with(|k| {
        assert_eq!(
            k.requeued,
            vec![
                (Queue::Statistics, 1),
                (Queue::Statistics, 2),
                (Queue::Main, 0),
                (Queue::Main, 3)
            ]
        );
    });
}

#[test]
fn the_driver_uses_its_copy_when_the_caller_changes_the_buffer_after_the_read() {
    let streams = Streams::new();
    let config = Racing {
        inner: streams.config.clone(),
        scribble: vec![0xff; PARAMS],
        withhold: None,
        allow: None,
        hook: Hook::default(),
    };
    let (mut d, k) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)));
    configured(&mut d);
    d.start().unwrap();
    d.tick(0);
    let params = pattern(5, PARAMS);
    streams.config.fill(0, &params);
    post(&streams.config, &job(1, 0, NO_BUFFER));
    d.tick(1);
    // The caller's buffer changed the moment the driver had read it.
    assert_eq!(streams.config.contents(0), vec![0xff; PARAMS]);
    // What reached the kernel is what was read.
    assert_eq!(k.queued_params(), vec![(0, 10921)]);
    assert_eq!(k.params_seen(0), params);
    // And it changes again later: the kernel's copy doesn't.
    streams.config.fill(0, &pattern(6, PARAMS));
    d.tick(2);
    assert_eq!(k.params_seen(0), params);
}

#[test]
fn a_job_the_driver_cannot_run_is_dropped_before_anything_reaches_the_kernel() {
    let streams = Streams::new().with_output1();
    let config = Racing {
        inner: streams.config.clone(),
        scribble: Vec::new(),
        withhold: Some(3),
        allow: None,
        hook: Hook::default(),
    };
    let (mut d, k) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)));
    configured(&mut d);
    // Output 1 is off.
    d.start().unwrap();
    d.tick(0);
    let refused = [
        IspJob {
            layout: LAYOUT_ISP21_PARAMS,
            ..job(1, 0, 0)
        },
        // Past the config stream, and a buffer the consumer kept.
        job(2, 4, 0),
        job(3, u32::MAX - 1, 0),
        job(4, 3, 0),
        // Past output 0's stream.
        job(5, 0, 3),
        job(6, 0, u32::MAX - 1),
        // An output that is off.
        IspJob {
            outputs: [NO_BUFFER, 0, NO_BUFFER, NO_BUFFER],
            ..job(7, 0, NO_BUFFER)
        },
        // An output the ISP doesn't have.
        IspJob {
            outputs: [NO_BUFFER, NO_BUFFER, 0, NO_BUFFER],
            ..job(8, 0, NO_BUFFER)
        },
        // An input buffer: frames come from the receiver.
        IspJob {
            input_buffer: 0,
            ..job(9, 0, 0)
        },
    ];
    for j in &refused {
        post(&streams.config, j);
    }
    // Bytes that are no job.
    streams.config.post_job(&[0xff; 3]);
    d.tick(1);
    assert_eq!(d.counters().unwrap().dropped, refused.len() as u64 + 1);
    assert!(k.queued_params().is_empty());
    // Refused jobs are the consumer's mistakes; bytes that are no job cost it the stream.
    assert_eq!(streams.config.faults(), 1);
}

#[test]
fn bytes_that_are_no_job_fault_the_consumer_and_the_next_one_is_served() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    streams.config.fill(2, &pattern(1, PARAMS));
    streams.config.post_job(&[0xff; 3]);
    post(&streams.config, &job(1, 2, 1));
    d.tick(1);
    // Faulted at the bad bytes: the job behind them never reached the kernel.
    assert_eq!(streams.config.faults(), 1);
    assert!(k.queued_params().is_empty());
    assert_eq!(d.counters().unwrap().dropped, 1);
    assert_eq!(streams.statistics.faults() + streams.output0.faults(), 0);
    // The config buffers held under it were given up; taken again, the next consumer's job runs.
    let params = pattern(4, PARAMS);
    streams.config.fill(2, &params);
    post(&streams.config, &job(2, 2, 1));
    d.tick(2);
    assert_eq!(k.queued_params().len(), 1);
    assert_eq!(k.params_seen(0), params);
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn an_output_buffer_is_used_once_until_the_consumer_gives_it_back() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    post(&streams.config, &job(1, 0, 0));
    // Named by the job still waiting.
    post(&streams.config, &job(2, 1, 0));
    d.tick(1);
    assert_eq!(d.counters().unwrap().dropped, 1);
    k.apply(0, 7);
    k.finish(Queue::Statistics, 0, 7, &[1; 8]);
    k.finish(Queue::Main, 0, 7, &[2; 8]);
    d.tick(2);
    assert_eq!(d.counters().unwrap().jobs, 1);
    // Published, and not given back yet.
    post(&streams.config, &job(3, 1, 0));
    d.tick(3);
    assert_eq!(d.counters().unwrap().dropped, 2);
    let frame = streams.output0.take_frame().unwrap();
    streams.output0.return_buffer(frame.buffer);
    post(&streams.config, &job(4, 1, 0));
    d.tick(4);
    assert_eq!(d.counters().unwrap().dropped, 2);
    assert_eq!(k.queued_params().len(), 2);
}

#[test]
fn a_buffer_given_back_and_named_by_a_later_job_in_the_same_tick_is_held() {
    let streams = Streams::new();
    let config = Racing {
        inner: streams.config.clone(),
        scribble: Vec::new(),
        withhold: None,
        allow: None,
        hook: Hook::default(),
    };
    let hook = Arc::clone(&config.hook);
    let (mut d, k) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)));
    configured(&mut d);
    d.start().unwrap();
    d.tick(0);
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    k.apply(0, 7);
    k.finish(Queue::Statistics, 0, 7, &[1; 8]);
    k.finish(Queue::Main, 0, 7, &[2; 8]);
    d.tick(2);
    assert_eq!(d.counters().unwrap().jobs, 1);
    // Job 2 is read first. Before job 3 is read, the consumer takes the frame job 1 published,
    // gives its buffer back and names it in job 3, all after this pump's first look at returns.
    post(&streams.config, &job(2, 1, NO_BUFFER));
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
    let params = pattern(8, PARAMS);
    streams.config.fill(2, &params);
    d.tick(3);
    // Job 3 reached the kernel, and nothing was dropped.
    assert_eq!(k.queued_params().len(), 3);
    assert_eq!(k.params_seen(k.queued_params()[2].0), params);
    assert_eq!(d.counters().unwrap().dropped, 0);
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn a_config_buffer_shorter_than_the_parameters_drops_the_job() {
    let streams = Streams {
        config: MockStream::with_bytes(2, PARAMS - 1),
        ..Streams::new()
    };
    let (mut d, k) = running(&streams);
    d.tick(0);
    post(&streams.config, &job(1, 0, NO_BUFFER));
    d.tick(1);
    assert_eq!(d.counters().unwrap().dropped, 1);
    assert!(k.queued_params().is_empty());
}

#[test]
fn jobs_wait_only_as_many_as_there_are_parameter_buffers() {
    let streams = Streams {
        config: MockStream::with_bytes(MAX_PENDING as u32 + 2, PARAMS),
        ..Streams::new()
    };
    let (mut d, k) = running(&streams);
    d.tick(0);
    for i in 0..=MAX_PENDING as u32 {
        post(&streams.config, &job(u64::from(i), i, NO_BUFFER));
    }
    d.tick(1);
    assert_eq!(k.queued_params().len(), MAX_PENDING);
    assert_eq!(d.counters().unwrap().dropped, 1);
    assert_eq!(d.oldest_waiting(), Some(0));
    // A parameter buffer back, and its job finished: room for one more.
    k.apply(0, 1);
    k.finish(Queue::Statistics, 0, 1, &[0; 4]);
    post(
        &streams.config,
        &job(100, MAX_PENDING as u32 + 1, NO_BUFFER),
    );
    d.tick(2);
    assert_eq!(d.counters().unwrap().jobs, 1);
    assert_eq!(k.queued_params().len(), MAX_PENDING + 1);
    assert_eq!(
        k.queued_params()[MAX_PENDING].0,
        0,
        "the slot that came back"
    );
    assert_eq!(d.oldest_waiting(), Some(1));
}

#[test]
fn a_part_whose_frame_passed_or_was_flagged_is_not_delivered_and_the_job_is_an_error() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    k.apply(0, 5);
    // Frame 5's statistics never came, and its image was flagged.
    k.finish(Queue::Statistics, 0, 6, &[1; 8]);
    k.finish_flagged(Queue::Main, 0, 5, &[2; 8], true);
    d.tick(2);
    assert!(streams.statistics.take_frame().is_none());
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(d.counters().unwrap().errors, 1);
    assert_eq!(d.oldest_waiting(), None);
    // The output buffer it named is free for the next job.
    post(&streams.config, &job(2, 1, 0));
    d.tick(3);
    assert_eq!(d.counters().unwrap().dropped, 0);

    // Parameters the kernel flagged: nothing of that job comes.
    k.finish_flagged(Queue::Params, 0, 9, &[], true);
    d.tick(4);
    assert_eq!(d.counters().unwrap().errors, 2);
}

#[test]
fn a_frame_larger_than_its_stream_buffer_is_not_delivered() {
    let streams = Streams {
        statistics: MockStream::with_bytes(2, 100),
        ..Streams::new()
    };
    let (mut d, k) = running(&streams);
    d.tick(0);
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    k.apply(0, 3);
    // Larger than the generation's statistics: refused before the stream sees it.
    k.finish(Queue::Statistics, 0, 3, &vec![0; STATS + 1]);
    // Larger than the image agreed.
    k.finish(Queue::Main, 0, 3, &vec![0; IMAGE + 1]);
    d.tick(2);
    assert!(streams.statistics.take_frame().is_none());
    assert!(streams.output0.take_frame().is_none());
    assert_eq!(d.counters().unwrap().errors, 1);

    // Statistics that fit the layout but not the stream's buffer: the stream refuses the write
    // and the buffer stays with the driver.
    post(&streams.config, &job(2, 1, 0));
    d.tick(3);
    k.apply(0, 4);
    k.finish(Queue::Statistics, 0, 4, &[7; 101]);
    k.finish(Queue::Main, 0, 4, &[8; 16]);
    d.tick(4);
    assert!(streams.statistics.take_frame().is_none());
    assert_eq!(streams.output0.take_frame().unwrap().bytes, 16);
    assert_eq!(d.counters().unwrap().errors, 2);
    post(&streams.config, &job(3, 2, NO_BUFFER));
    d.tick(5);
    k.apply(0, 5);
    k.finish(Queue::Statistics, 0, 5, &[7; 100]);
    d.tick(6);
    let s = streams.statistics.take_frame().unwrap();
    assert_eq!((s.identity, s.bytes), (3, 100));
}

#[test]
fn the_self_path_delivers_into_its_own_stream() {
    let streams = Streams::new().with_output1();
    let (mut d, k) = driver(&streams);
    configured(&mut d);
    d.set_output(1, nv12()).unwrap();
    d.start().unwrap();
    d.tick(0);
    post(
        &streams.config,
        &IspJob {
            outputs: [0, 1, NO_BUFFER, NO_BUFFER],
            ..job(42, 0, 0)
        },
    );
    d.tick(1);
    k.apply(0, 2);
    k.finish(Queue::Statistics, 0, 2, &[1; 4]);
    k.finish(Queue::Main, 0, 2, &[2; 4]);
    k.finish(Queue::SelfPath, 0, 2, &[3; 4]);
    d.tick(2);
    let out1 = streams.output1.as_ref().unwrap();
    let frame = out1.take_frame().unwrap();
    assert_eq!((frame.buffer, frame.identity), (1, 42));
    assert_eq!(&out1.contents(1)[..4], &[3; 4]);
    assert_eq!(d.counters().unwrap().jobs, 1);
}

// ---- Failures.

#[test]
fn a_failed_queue_restarts_the_path_with_capped_backoff_and_never_gives_up() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    d.tick(0);
    post(&streams.config, &job(1, 0, 0));
    d.tick(1);
    k.with(|k| {
        k.fail_dequeue = Some(Status::IO);
        k.fail_open = Some(Status::NO_DEVICE);
    });
    let started = k.with(|k| k.opened.len());
    assert_eq!(d.tick(2), Some(2 + backoff_ns(0)));
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.counters().unwrap().errors, 1, "the waiting job");
    // Attempts keep failing: the delay doubles to 8 s and stays there.
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
    // The ISP comes back.
    k.with(|k| k.fail_open = None);
    assert_eq!(d.tick(now), Some(now + 1_000_000));
    assert_eq!(d.health(), Health::Ok);
    assert_eq!(k.with(|k| k.opened.len()), started + 1);
    // The output buffer the lost job named is the driver's again.
    post(&streams.config, &job(2, 1, 0));
    d.tick(now + 1);
    assert_eq!(d.counters().unwrap().dropped, 0);
}

#[test]
fn stop_ends_recovery() {
    let streams = Streams::new();
    let (mut d, k) = running(&streams);
    k.with(|k| {
        k.fail_dequeue = Some(Status::IO);
        k.fail_open = Some(Status::IO);
    });
    d.tick(0);
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
    let (mut d, _k) = running(&streams);
    d.tick(0);
    streams.config.post_job(&[1]);
    d.tick(1);
    assert_eq!(d.counters().unwrap().dropped, 1);
    d.stop().unwrap();
    d.start().unwrap();
    assert_eq!(d.counters().unwrap(), Counters::default());
}

// ---- Allocation.

/// One job through the driver: taken on one tick, its frame finished for the next, its results
/// handed back by the consumer.
fn one_job(
    d: &mut Rkisp,
    now: &mut u64,
    streams: &Streams,
    k: &Handle,
    allow: &AtomicUsize,
    sequence: u32,
    frame: [(Queue, u32, Vec<u8>); 3],
) {
    allow.store(1, Ordering::Relaxed);
    *now = d.tick(*now).unwrap();
    k.with(|k| {
        for (queue, index, data) in frame {
            let done = Done {
                index,
                sequence,
                timestamp_ns: 0,
                bad: false,
            };
            k.finished[queue as usize].push_back((done, data));
        }
    });
    *now = d.tick(*now).unwrap();
    let s = streams.statistics.take_frame().unwrap();
    streams.statistics.return_buffer(s.buffer);
    let o = streams.output0.take_frame().unwrap();
    streams.output0.return_buffer(o.buffer);
}

#[test]
fn the_steady_state_allocates_nothing() {
    const JOBS: u32 = 2000;
    const WARM: u32 = 8;
    let streams = Streams::new();
    let allow = Arc::new(AtomicUsize::new(0));
    let config = Racing {
        inner: streams.config.clone(),
        scribble: Vec::new(),
        withhold: None,
        allow: Some(Arc::clone(&allow)),
        hook: Hook::default(),
    };
    let (mut d, k) = driver_on(&streams, lemnos_core::Held::boxed(Box::new(config)));
    configured(&mut d);
    d.start().unwrap();
    let mut now = d.tick(0).unwrap();
    streams.config.fill(0, &pattern(1, PARAMS));
    // Everything the stand-ins and the mocked streams hold is made before measuring: the jobs
    // are posted (and let through one a tick), the kernel's finished buffers made.
    for i in 0..JOBS {
        post(&streams.config, &job(u64::from(i), i % 4, i % 3));
    }
    let stats = vec![5u8; 2048];
    let image = vec![6u8; IMAGE];
    let mut frames = (0..JOBS)
        .map(|i| {
            [
                // One job waits at a time, so its parameters always take buffer 0.
                (Queue::Params, 0, Vec::new()),
                (Queue::Statistics, i % 4, stats.clone()),
                (Queue::Main, i % 4, image.clone()),
            ]
        })
        .collect::<VecDeque<_>>();
    k.with(|k| {
        for q in &mut k.finished {
            q.reserve(8);
        }
    });
    // A round first, so every queue has grown to its size.
    for i in 0..WARM {
        let frame = frames.pop_front().unwrap();
        one_job(&mut d, &mut now, &streams, &k, &allow, i, frame);
    }
    let ((), allocated) = allocations(|| {
        for i in WARM..JOBS {
            let frame = frames.pop_front().unwrap();
            one_job(&mut d, &mut now, &streams, &k, &allow, i, frame);
        }
    });
    assert_eq!(
        allocated, 0,
        "job intake, the parameter read and copy, dequeue, write, publish, requeue"
    );
    let counters = d.counters().unwrap();
    assert_eq!(
        (counters.jobs, counters.dropped, counters.errors),
        (u64::from(JOBS), 0, 0)
    );
}
