//! The VGLite driver as a host runs it: bound from a board file, its methods called through the host,
//! its jobs posted by a consumer on the host's own frame stream and completed there, its ticks
//! from the host's simulated clock. Sealed memfds stand in for the stream's dma-bufs, and an
//! engine that only counts (or a path where no node exists) for the device. No device is
//! opened.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_contract_image_convert::{self as convert, ConvertJob, ImageConvert, ImageRef};
use lemnos_contract_image_scaler::{self as scaler, Filter as WireFilter, ScaleJob};
use lemnos_core::{CallerId, Decode, Encode, Health, Lifecycle, MethodId, Reader, Status, Writer};
use lemnos_driver_vglite::driver::{BoxedGpu, POLL_NS};
use lemnos_driver_vglite::serve::{Contract, Failure};
use lemnos_driver_vglite::{
    Filter, Frame, Gpu, RunError, State, Transfer, VgliteDriver, VgliteDriverNeeds,
};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::mock::MockStream;
use lemnos_host::board::parse;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A buffer holds a 32 by 32 frame of four bytes a pixel: one page.
const BYTES: u32 = 4096;
const BGRA: u32 = u32::from_le_bytes(*b"AR24");
const RGBA: u32 = u32::from_le_bytes(*b"AB24");
const MS: u64 = 1_000_000;

/// The pool as the host addresses it, over one mapping per buffer.
struct Mapped {
    buffers: Vec<Mapping>,
    stride: usize,
    bytes: usize,
}

impl Mapped {
    fn at(&self, offset: usize, len: usize) -> Option<(&Mapping, usize)> {
        let inner = offset % self.stride;
        let fits = inner.checked_add(len).is_some_and(|end| end <= self.bytes);
        let map = self.buffers.get(offset / self.stride)?;
        fits.then_some((map, inner))
    }
}

impl BufferMemory for Mapped {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.at(offset, data.len())
            .is_some_and(|(map, inner)| map.write_bytes(inner, data).is_some())
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some((map, inner)) = self.at(offset, out.len()) else {
            return false;
        };
        match map.as_slice().get(inner..inner + out.len()) {
            Some(from) => {
                out.copy_from_slice(from);
                true
            }
            None => false,
        }
    }
}

/// Each buffer of a stream its own sealed memfd, mapped for the host and lent as its handle.
struct PerBuffer;

impl StreamAllocator for PerBuffer {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        let plain = HeapAllocator.allocate(name, layout)?;
        let mut handles = Vec::new();
        let mut buffers = Vec::new();
        for _ in 0..layout.buffers {
            let fd: OwnedFd =
                sealed_memfd(c"vglite-driver-test", layout.stride()).map_err(|_| Status::IO)?;
            let bytes = layout.buffer_bytes as usize;
            buffers.push(Mapping::new(fd.as_fd(), bytes, 0, true).map_err(|_| Status::IO)?);
            handles.push(fd);
        }
        Ok(Backing {
            buffers: Arc::new(Mapped {
                buffers,
                stride: layout.stride() as usize,
                bytes: layout.buffer_bytes as usize,
            }),
            handles: Some(handles.into()),
            ..plain
        })
    }

    fn allocate_handles(
        &self,
        name: &str,
        layout: &Layout,
        _heap: &str,
    ) -> Result<Backing, Status> {
        self.allocate(name, layout)
    }
}

/// The ids of the driver's two contracts on the device, in `provides` order.
const SCALER: u8 = 0;
const CONVERTER: u8 = 1;

/// A board with one device of `driver` and the given job streams, of dma-bufs when `dma_buf`.
fn board_with(driver: &str, streams: &[&str], dma_buf: bool) -> String {
    let memory = if dma_buf {
        "memory = \"dma-buf\"\n"
    } else {
        ""
    };
    let mut text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"g\"\ndriver = \"{driver}\"\n\
         config = {{ device = \"/nonexistent/vg_lite\" }}\n"
    );
    for stream in streams {
        text += &format!(
            "[devices.streams.{stream}]\n{memory}buffers = 4\nbuffer_bytes = {BYTES}\n\
             job_slots = 4\njob_bytes = {}\n",
            scaler::JOB_BYTES
        );
    }
    text
}

fn board(driver: &str, dma_buf: bool) -> String {
    board_with(driver, &["scale", "convert"], dma_buf)
}

struct Rig {
    host: Host,
    clock: SimClock,
}

impl Rig {
    fn start(text: &str, allocator: Arc<dyn StreamAllocator>) -> Self {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start_with_streams(
            &parse(text).unwrap(),
            &[entry::<VgliteDriver>(), entry::<FakeCounter>()],
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
            allocator,
        )
        .unwrap();
        Self { host, clock }
    }

    fn call(&self, contract: u8, method: u16, args: &[u8]) -> Result<Vec<u8>, Status> {
        let mut reply = [0u8; 256];
        let n = self
            .host
            .call(0, MethodId { contract, method }, args, &mut reply)?;
        Ok(reply[..n].to_vec())
    }

    fn health(&self) -> Health {
        self.host.device("g").unwrap().health
    }

    fn after(&self, by: Duration) {
        self.clock.advance(by);
        self.host.tick();
    }

    fn refusal(&self) -> String {
        self.host.tick();
        match self.host.device("g").unwrap().state {
            DeviceState::Backoff { reason, .. } => reason,
            other => panic!("the driver bound: {other:?}"),
        }
    }
}

fn image(buffer: u32, width: u32, height: u32, fourcc: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc,
        stride: width * 4,
    }
}

fn convert_job(identity: u64, input: ImageRef, output: ImageRef) -> ConvertJob {
    ConvertJob {
        identity,
        input,
        output,
    }
}

fn scale_ref(i: ImageRef) -> scaler::ImageRef {
    scaler::ImageRef {
        buffer: i.buffer,
        width: i.width,
        height: i.height,
        fourcc: i.fourcc,
        stride: i.stride,
    }
}

fn scale_job(identity: u64, input: ImageRef, output: ImageRef, filter: WireFilter) -> ScaleJob {
    ScaleJob {
        identity,
        input: scale_ref(input),
        output: scale_ref(output),
        filter,
    }
}

fn post<J: Encode>(consumer: &mut Consumer, job: &J) {
    let mut bytes = [0u8; scaler::JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    assert!(consumer.push_job(&bytes[..used]).unwrap());
}

fn consumer(lease: &Lease) -> Consumer {
    Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap()
}

fn completion(consumer: &mut Consumer) -> FrameSlot {
    consumer.pop_frame().unwrap().expect("a completion")
}

#[test]
fn a_board_file_binds_it_and_the_host_reaches_both_contracts_and_their_jobs() {
    let rig = Rig::start(&board("vglite", true), Arc::new(PerBuffer));
    rig.host.tick();
    let info = rig.host.device("g").unwrap();
    assert_eq!(info.driver, "vglite");
    assert_eq!(rig.host.manifest(0).unwrap().provides.len(), 2);
    let name = rig.call(SCALER, 4, &32u32.to_le_bytes()).unwrap();
    assert!(name.ends_with(b"scale"), "{name:?}");
    let name = rig.call(CONVERTER, 4, &32u32.to_le_bytes()).unwrap();
    assert!(name.ends_with(b"convert"), "{name:?}");
    let formats = rig.call(SCALER, 2, &[]).unwrap();
    assert_eq!(u16::decode(&mut Reader::new(&formats)).unwrap(), 4);
    let pairs = rig.call(CONVERTER, 2, &[]).unwrap();
    assert_eq!(u16::decode(&mut Reader::new(&pairs)).unwrap(), 10);
    // No GPU is at the configured path: a job is completed as failed on its output, and the
    // driver keeps trying, reporting it.
    let lease = rig.host.stream_attach(0, "scale", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    rig.after(Duration::from_millis(1));
    post(
        &mut consumer,
        &scale_job(
            0xBEEF,
            image(0, 32, 32, RGBA),
            image(1, 16, 16, RGBA),
            WireFilter::Bilinear,
        ),
    );
    rig.after(Duration::from_millis(1));
    let done = completion(&mut consumer);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xBEEF, 0));
    assert_eq!(
        rig.health(),
        Health::Degraded("the GPU failed: reopening it")
    );
    let counters = rig.call(SCALER, 5, &[]).unwrap();
    assert_eq!(
        scaler::Counters::decode(&mut Reader::new(&counters)).unwrap(),
        scaler::Counters { jobs: 0, errors: 1 }
    );
    let counters = rig.call(CONVERTER, 5, &[]).unwrap();
    assert_eq!(
        convert::Counters::decode(&mut Reader::new(&counters)).unwrap(),
        convert::Counters::default()
    );
}

#[test]
fn a_contract_whose_stream_the_board_leaves_out_names_none() {
    let rig = Rig::start(
        &board_with("vglite", &["convert"], true),
        Arc::new(PerBuffer),
    );
    rig.host.tick();
    assert_eq!(rig.host.device("g").unwrap().state, DeviceState::Running);
    assert_eq!(
        rig.call(SCALER, 4, &32u32.to_le_bytes()),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(rig.call(CONVERTER, 4, &32u32.to_le_bytes()).is_ok());
}

#[test]
fn bind_refuses_plain_streams_no_streams_and_a_bad_device_setting() {
    // `HeapAllocator` makes plain shared memory, with no handles.
    let rig = Rig::start(&board("vglite", false), Arc::new(HeapAllocator));
    let reason = rig.refusal();
    assert!(
        reason.contains(&format!("{:?}", Status::NOT_SUPPORTED)),
        "{reason}"
    );
    let stats = rig.host.stream_stats(0, "scale").unwrap();
    assert_eq!((stats.attachments, stats.claimed), (0, false));

    let rig = Rig::start(&board_with("vglite", &[], true), Arc::new(PerBuffer));
    let reason = rig.refusal();
    assert!(
        reason.contains(&format!("{:?}", Status::NOT_FOUND)),
        "{reason}"
    );

    for config in ["device = \"\"", "device = 3"] {
        let text = board("vglite", true).replace("device = \"/nonexistent/vg_lite\"", config);
        let rig = Rig::start(&text, Arc::new(PerBuffer));
        let reason = rig.refusal();
        assert!(
            reason.contains(&format!("{:?}", Status::INVALID_ARGUMENT)),
            "{config}: {reason}"
        );
    }
}

/// A GPU that counts its jobs and fails as told; `Send`, as a driver's must be.
#[derive(Default)]
struct Plan {
    resizes: AtomicU32,
    converts: AtomicU32,
    /// Jobs left to fail as the device.
    failures: AtomicU32,
    last_filter: std::sync::Mutex<Option<Filter>>,
}

struct Counted(Arc<Plan>);

impl Counted {
    fn ran(&self, frames: [Frame<'_>; 2]) -> Result<Transfer, RunError> {
        let left = self.0.failures.load(Ordering::Relaxed);
        if left > 0 {
            self.0.failures.store(left - 1, Ordering::Relaxed);
            return Err(RunError::Device {
                error: io::ErrorKind::TimedOut.into(),
                retry_at_ns: 0,
            });
        }
        let staged_bytes = frames.iter().map(|f| f.image.frame_len()).sum();
        Ok(Transfer { staged_bytes })
    }
}

impl Gpu for Counted {
    fn state(&self) -> State {
        State::Open
    }

    fn resize(
        &mut self,
        _now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
        filter: Filter,
    ) -> Result<Transfer, RunError> {
        self.0.resizes.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last) = self.0.last_filter.try_lock() {
            *last = Some(filter);
        }
        self.ran([src, dst])
    }

    fn convert(
        &mut self,
        _now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
    ) -> Result<Transfer, RunError> {
        self.0.converts.fetch_add(1, Ordering::Relaxed);
        self.ran([src, dst])
    }
}

/// The driver over a counting GPU, on the streams of a device whose own driver doesn't use
/// them: the test plays the host's part in ticking it.
fn driver_on(host: &Host, plan: &Arc<Plan>) -> VgliteDriver {
    let stream = |name| -> Option<lemnos_core::StreamHandle> {
        Some(lemnos_core::Held::boxed(Box::new(
            host.stream_producer(0, name).unwrap(),
        )))
    };
    let needs = VgliteDriverNeeds {
        scale: stream("scale"),
        convert: stream("convert"),
    };
    VgliteDriver::with_gpu(needs, Box::new(Counted(Arc::clone(plan))) as BoxedGpu)
}

fn counter_board() -> String {
    board("fake-counter", true)
}

#[test]
fn scale_and_convert_jobs_run_on_the_gpu_each_from_its_own_stream() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let scale = rig.host.stream_attach(0, "scale", CallerId(1)).unwrap();
    let conv = rig.host.stream_attach(0, "convert", CallerId(1)).unwrap();
    let (mut scale, mut conv) = (consumer(&scale), consumer(&conv));
    assert_eq!(driver.tick(0), Some(POLL_NS));
    post(
        &mut scale,
        &scale_job(
            1,
            image(0, 32, 32, RGBA),
            image(1, 16, 16, RGBA),
            WireFilter::Area,
        ),
    );
    post(
        &mut conv,
        &convert_job(2, image(2, 32, 32, RGBA), image(3, 32, 32, BGRA)),
    );
    assert_eq!(driver.tick(MS), Some(2 * MS));
    let done = completion(&mut scale);
    assert_eq!(
        (done.buffer, done.identity, done.bytes),
        (1, 1, 16 * 16 * 4)
    );
    let done = completion(&mut conv);
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 2, BYTES));
    assert_eq!(plan.resizes.load(Ordering::Relaxed), 1);
    assert_eq!(plan.converts.load(Ordering::Relaxed), 1);
    assert_eq!(*plan.last_filter.lock().unwrap(), Some(Filter::Area));
    assert_eq!(driver.health(), Health::Ok);
    assert_eq!(
        ImageConvert::counters(&mut driver).unwrap(),
        convert::Counters { jobs: 1, errors: 0 }
    );
    // The consumer goes. What the lane holds has nothing behind it; the next consumer's job
    // gives that up and runs on the new consumer's memory.
    rig.host.stream_detach(0, "convert", CallerId(1)).unwrap();
    let again = rig.host.stream_attach(0, "convert", CallerId(2)).unwrap();
    let mut again = consumer(&again);
    post(
        &mut again,
        &convert_job(3, image(0, 32, 32, BGRA), image(1, 32, 32, RGBA)),
    );
    driver.tick(2 * MS);
    let done = completion(&mut again);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 3, BYTES));
    assert_eq!(plan.converts.load(Ordering::Relaxed), 2);
}

#[test]
fn what_a_job_names_wrongly_never_reaches_the_gpu() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let conv = rig.host.stream_attach(0, "convert", CallerId(1)).unwrap();
    let scale = rig.host.stream_attach(0, "scale", CallerId(1)).unwrap();
    let (mut conv, mut scale) = (consumer(&conv), consumer(&scale));
    driver.tick(0);
    let padded = ImageRef {
        stride: 32 * 4 + 64,
        ..image(1, 32, 32, BGRA)
    };
    let cases = [
        // 32 by 33 at four bytes is one row past the page behind the output.
        (
            image(0, 32, 32, RGBA),
            image(1, 32, 33, BGRA),
            Failure::Size,
        ),
        // The same size: a padded stride takes the frame past its dma-buf.
        (image(0, 32, 32, RGBA), padded, Failure::Frame),
        (
            image(0, 32, 32, RGBA),
            image(1, 32, 32, u32::from_le_bytes(*b"NV12")),
            Failure::Format,
        ),
        (image(0, 0, 32, RGBA), image(1, 0, 32, BGRA), Failure::Size),
        (
            ImageRef {
                stride: 8,
                ..image(0, 32, 32, RGBA)
            },
            image(1, 32, 32, BGRA),
            Failure::Size,
        ),
        (
            image(9, 32, 32, RGBA),
            image(1, 32, 32, BGRA),
            Failure::Buffers,
        ),
    ];
    for (n, (input, output, failure)) in cases.into_iter().enumerate() {
        post(&mut conv, &convert_job(n as u64, input, output));
        driver.tick((n as u64 + 1) * MS);
        let done = completion(&mut conv);
        assert_eq!((done.identity, done.bytes), (n as u64, 0), "case {n}");
        conv.release(done.buffer, done.fence).unwrap();
        assert_eq!(
            driver.last_failure(Contract::Convert),
            Some(failure),
            "case {n}"
        );
    }
    // A scale that changes the format.
    post(
        &mut scale,
        &scale_job(
            50,
            image(0, 32, 32, RGBA),
            image(1, 16, 16, BGRA),
            WireFilter::Bilinear,
        ),
    );
    driver.tick(20 * MS);
    assert_eq!(completion(&mut scale).bytes, 0);
    assert_eq!(driver.last_failure(Contract::Scale), Some(Failure::Format));
    assert_eq!(plan.converts.load(Ordering::Relaxed), 0);
    assert_eq!(plan.resizes.load(Ordering::Relaxed), 0);
    assert_eq!(
        ImageConvert::counters(&mut driver).unwrap(),
        convert::Counters { jobs: 0, errors: 6 }
    );
}

#[test]
fn bytes_that_are_no_job_fault_the_consumer() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let lease = rig.host.stream_attach(0, "scale", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    driver.tick(0);
    assert!(consumer.push_job(&[1, 2, 3]).unwrap());
    driver.tick(MS);
    assert_eq!(
        driver.last_failure(Contract::Scale),
        Some(Failure::Undecodable)
    );
    assert_eq!(plan.resizes.load(Ordering::Relaxed), 0);
}

#[test]
fn a_gpu_failure_fails_the_job_and_the_next_job_runs() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    plan.failures.store(3, Ordering::Relaxed);
    let mut driver = driver_on(&rig.host, &plan);
    let lease = rig.host.stream_attach(0, "convert", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    for n in 0..4u64 {
        post(
            &mut consumer,
            &convert_job(n, image(0, 32, 32, RGBA), image(1, 32, 32, BGRA)),
        );
        driver.tick(n * MS);
        let done = completion(&mut consumer);
        let want = if n < 3 { 0 } else { BYTES };
        assert_eq!((done.identity, done.bytes), (n, want));
        consumer.release(done.buffer, done.fence).unwrap();
    }
    assert_eq!(
        driver.last_failure(Contract::Convert),
        Some(Failure::Engine)
    );
    assert_eq!(
        ImageConvert::counters(&mut driver).unwrap(),
        convert::Counters { jobs: 1, errors: 3 }
    );
}

#[test]
fn a_stream_error_is_tried_again_on_the_backoff_and_the_other_stream_carries_on() {
    // The host's stream can't be made to fail on cue; the mock's can: a job longer than any a
    // stream carries is refused by the endpoint and stays at the head of the queue.
    let broken = MockStream::with_handles(vec![
        sealed_memfd(c"vglite-driver-test", 4096).unwrap(),
        sealed_memfd(c"vglite-driver-test", 4096).unwrap(),
    ]);
    broken.post_job(&[0; 4097]);
    let plan = Arc::new(Plan::default());
    let needs = VgliteDriverNeeds {
        scale: Some(lemnos_core::Held::boxed(Box::new(broken.clone()))),
        convert: None,
    };
    let mut driver = VgliteDriver::with_gpu(needs, Box::new(Counted(Arc::clone(&plan))));
    assert_eq!(driver.tick(0), Some(250 * MS));
    assert_eq!(
        driver.health(),
        Health::Degraded("a job stream failed: trying it again")
    );
    assert_eq!(driver.tick(MS), Some(250 * MS), "not due: untouched");
    assert_eq!(driver.tick(250 * MS), Some(750 * MS));
    assert_eq!(driver.tick(750 * MS), Some(1750 * MS));
    let mut now = 1750 * MS;
    let mut last = 0;
    for _ in 0..8 {
        let next = driver.tick(now).unwrap();
        last = next - now;
        now = next;
    }
    assert_eq!(last, 8000 * MS, "capped at 8 s, never given up");
}

#[test]
fn a_thousand_jobs_through_the_driver_allocate_nothing() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let lease = rig.host.stream_attach(0, "convert", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    let mut cycle = |n: u64| {
        let output = 1 + (n % 3) as u32;
        post(
            &mut consumer,
            &convert_job(n, image(0, 32, 32, RGBA), image(output, 32, 32, BGRA)),
        );
        assert_eq!(driver.tick(n * MS), Some((n + 1) * MS));
        let done = completion(&mut consumer);
        assert_eq!((done.buffer, done.identity, done.bytes), (output, n, BYTES));
        assert!(consumer.release(done.buffer, done.fence).unwrap());
    };
    for n in 0..10 {
        cycle(n);
    }
    let ((), count) = allocations(|| {
        for n in 10..1010 {
            cycle(n);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 jobs");
    assert_eq!(plan.converts.load(Ordering::Relaxed), 1010);
}
