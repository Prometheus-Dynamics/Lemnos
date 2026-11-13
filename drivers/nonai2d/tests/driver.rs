//! The driver as a host runs it: bound from a board file, its methods called through the host,
//! its jobs posted by a consumer on the host's own frame stream and completed there, its ticks
//! from the host's simulated clock. Sealed memfds stand in for the stream's dma-bufs, and an
//! engine that only counts (or a path where no node exists) for the device. No device is
//! opened.

#![cfg(target_os = "linux")]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_contract_image_convert::{ConvertJob, Counters, ImageConvert, ImageRef, JOB_BYTES};
use lemnos_core::{CallerId, Decode, Encode, Health, Lifecycle, MethodId, Reader, Status, Writer};
use lemnos_driver_nonai2d::convert::Failure;
use lemnos_driver_nonai2d::driver::{BoxedEngine, Opener, POLL_NS};
use lemnos_driver_nonai2d::{
    Engine, Frame, Layout as FrameLayout, Nonai2dDriver, Nonai2dDriverNeeds, State,
};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::mock::MockStream;
use lemnos_host::board::parse;
use lemnos_host::stream::{Buffers, Lease, MemfdAllocator, StreamAllocator};
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, FrameSlot, Layout};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A buffer holds a 32 by 32 frame of four bytes a pixel: one page.
const BYTES: u32 = 4096;
const ARGB: u32 = u32::from_le_bytes(*b"AR24");
const XRGB: u32 = u32::from_le_bytes(*b"XR24");
const MS: u64 = 1_000_000;

/// Each buffer of a stream its own sealed memfd, lent as its handle. The plain allocator makes
/// shared memory, which the driver refuses.
struct PerBuffer;

impl StreamAllocator for PerBuffer {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        _name: &str,
        layout: &Layout,
        _heap: &str,
        _planes: &Planes,
    ) -> Result<Buffers, Status> {
        let mut fds = Vec::new();
        for _ in 0..layout.buffers {
            fds.push(
                sealed_memfd(c"nonai2d-driver-test", layout.stride()).map_err(|_| Status::IO)?,
            );
        }
        Ok(Buffers {
            fds,
            dma: true,
            sync: None,
        })
    }
}

/// A board with one device of `driver` and its job stream, of dma-bufs when `dma_buf`.
fn board(driver: &str, dma_buf: bool) -> String {
    let memory = if dma_buf {
        "memory = \"dma-buf\"\n"
    } else {
        ""
    };
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"g\"\ndriver = \"{driver}\"\n\
         config = {{ device = \"/nonexistent/video9\" }}\n\
         [devices.streams.jobs]\n{memory}buffers = 4\nbuffer_bytes = {BYTES}\n\
         job_slots = 4\njob_bytes = {JOB_BYTES}\n"
    )
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
            &[entry::<Nonai2dDriver>(), entry::<FakeCounter>()],
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
}

fn job(identity: u64, input: u32, output: u32, width: u32, height: u32) -> ConvertJob {
    let image = |buffer, fourcc| ImageRef {
        buffer,
        width,
        height,
        fourcc,
        stride: width * 4,
    };
    ConvertJob {
        identity,
        input: image(input, ARGB),
        output: image(output, XRGB),
    }
}

fn post(consumer: &mut Consumer, job: &ConvertJob) {
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    assert!(consumer.jobs().post(&bytes[..used]).unwrap());
}

fn consumer(lease: Lease) -> Consumer {
    lease.consumer_writable().unwrap()
}

fn completion(consumer: &mut Consumer) -> FrameSlot {
    consumer.pop_frame().unwrap().expect("a completion")
}

#[test]
fn a_board_file_binds_it_and_the_host_reaches_its_contract_and_its_jobs() {
    let rig = Rig::start(&board("nonai2d", true), Arc::new(PerBuffer));
    rig.host.tick();
    let info = rig.host.device("g").unwrap();
    assert_eq!(info.driver, "nonai2d");
    assert_eq!(rig.host.manifest(0).unwrap().provides.len(), 1);
    // `job_stream` names the stream the board gave it.
    let name = rig.call(0, 4, &32u32.to_le_bytes()).unwrap();
    assert!(name.ends_with(b"jobs"), "{name:?}");
    let count = rig.call(0, 2, &[]).unwrap();
    assert_eq!(u16::decode(&mut Reader::new(&count)).unwrap(), 49);
    // No engine is at the configured path: a job is completed as failed on its output, and the
    // driver keeps trying, reporting it.
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(lease);
    rig.after(Duration::from_millis(1));
    post(&mut consumer, &job(0xBEEF, 0, 1, 32, 32));
    rig.after(Duration::from_millis(1));
    let done = completion(&mut consumer);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xBEEF, 0));
    assert_eq!(
        rig.health(),
        Health::Degraded("the engine failed: reopening it")
    );
    let counters = rig.call(0, 5, &[]).unwrap();
    assert_eq!(
        Counters::decode(&mut Reader::new(&counters)).unwrap(),
        Counters { jobs: 0, errors: 1 }
    );
}

#[test]
fn a_stream_of_plain_shared_memory_is_refused_at_bind_before_any_consumer_comes() {
    // `MemfdAllocator` makes plain shared memory, with no handles.
    let rig = Rig::start(&board("nonai2d", false), Arc::new(MemfdAllocator));
    rig.host.tick();
    let DeviceState::Backoff { reason, .. } = rig.host.device("g").unwrap().state else {
        panic!("the driver bound to a stream it cannot use");
    };
    assert!(
        reason.contains(&format!("{:?}", Status::NOT_SUPPORTED)),
        "{reason}"
    );
    assert!(matches!(rig.health(), Health::Failed(_)));
    // Nobody had to attach for the driver to know, and nothing holds the stream's other end.
    let stats = rig.host.stream_stats(0, "jobs").unwrap();
    assert_eq!((stats.attachments, stats.claimed), (0, false));
    // The same board with the stream as dma-bufs binds.
    let rig = Rig::start(&board("nonai2d", true), Arc::new(PerBuffer));
    rig.host.tick();
    assert_eq!(rig.host.device("g").unwrap().state, DeviceState::Running);
}

/// An engine that counts its jobs and fails as told; `Send`, as a driver's must be.
#[derive(Default)]
struct Plan {
    opens: AtomicU32,
    open_failures: AtomicU32,
    runs: AtomicU32,
}

struct Counted {
    plan: Arc<Plan>,
    layouts: (FrameLayout, FrameLayout),
}

impl Engine for Counted {
    fn layouts(&self) -> (FrameLayout, FrameLayout) {
        self.layouts
    }

    fn convert(&mut self, _src: &Frame<'_>, _dst: &Frame<'_>) -> io::Result<()> {
        self.plan.runs.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn opener(plan: &Arc<Plan>) -> Opener {
    let plan = Arc::clone(plan);
    Box::new(move |src, dst| {
        plan.opens.fetch_add(1, Ordering::Relaxed);
        let left = plan.open_failures.load(Ordering::Relaxed);
        if left > 0 {
            plan.open_failures.store(left - 1, Ordering::Relaxed);
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Box::new(Counted {
            plan: Arc::clone(&plan),
            layouts: (src, dst),
        }) as BoxedEngine)
    })
}

/// The driver over a counting engine, on stream "jobs" of a device whose own driver doesn't
/// use it: the test plays the host's part in ticking it.
fn driver_on(host: &Host, plan: &Arc<Plan>) -> Nonai2dDriver {
    let jobs: lemnos_core::StreamHandle =
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, "jobs").unwrap()));
    Nonai2dDriver::with_engine(Nonai2dDriverNeeds { jobs }, opener(plan))
}

fn counter_board() -> String {
    board("fake-counter", true)
}

#[test]
fn jobs_run_on_the_engine_and_a_new_consumer_gets_its_own_buffers() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let first = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer1 = consumer(first);
    assert_eq!(driver.tick(0), Some(POLL_NS));
    post(&mut consumer1, &job(1, 0, 1, 32, 32));
    driver.tick(MS);
    let done = completion(&mut consumer1);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 1, BYTES));
    assert_eq!(plan.runs.load(Ordering::Relaxed), 1);
    assert_eq!(driver.health(), Health::Ok);
    // The consumer goes. What the driver holds has nothing behind it; the next consumer's job
    // gives that up and runs on the new consumer's memory.
    rig.host.stream_detach(0, "jobs", CallerId(1)).unwrap();
    let second = rig.host.stream_attach(0, "jobs", CallerId(2)).unwrap();
    let mut consumer2 = consumer(second);
    post(&mut consumer2, &job(2, 2, 3, 32, 32));
    driver.tick(2 * MS);
    let done = completion(&mut consumer2);
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 2, BYTES));
    assert_eq!(plan.runs.load(Ordering::Relaxed), 2);
    assert_eq!(driver.counters().unwrap(), Counters { jobs: 2, errors: 0 });
}

#[test]
fn a_frame_larger_than_the_dma_buf_never_reaches_the_engine() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(lease);
    driver.tick(0);
    // 32 by 33 at four bytes is one row past the page behind the output.
    post(&mut consumer, &job(9, 0, 1, 32, 33));
    driver.tick(MS);
    let done = completion(&mut consumer);
    assert_eq!((done.identity, done.bytes), (9, 0));
    assert_eq!(plan.runs.load(Ordering::Relaxed), 0);
    assert_eq!(driver.converter().last_failure(), Some(Failure::Frame));
}

#[test]
fn an_engine_that_wont_open_is_tried_again_doubling_to_8_s_and_never_given_up() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    plan.open_failures.store(u32::MAX, Ordering::Relaxed);
    let mut driver = driver_on(&rig.host, &plan);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(lease);
    let mut now = 0;
    for (n, want_ms) in [250, 500, 1000, 2000, 4000, 8000, 8000, 8000]
        .into_iter()
        .enumerate()
    {
        post(&mut consumer, &job(n as u64, 0, 1, 32, 32));
        driver.tick(now);
        let done = completion(&mut consumer);
        assert_eq!((done.identity, done.bytes), (n as u64, 0));
        consumer.release(done.buffer);
        let State::BackingOff { retry_at_ns, .. } = driver.converter().engine().state() else {
            panic!("backing off");
        };
        assert_eq!(retry_at_ns - now, want_ms * MS, "attempt {n}");
        assert!(matches!(driver.health(), Health::Degraded(_)));
        now = retry_at_ns;
    }
    plan.open_failures.store(0, Ordering::Relaxed);
    post(&mut consumer, &job(99, 0, 1, 32, 32));
    driver.tick(now);
    assert_eq!(completion(&mut consumer).bytes, BYTES);
    assert_eq!(driver.health(), Health::Ok);
}

#[test]
fn a_stream_error_is_tried_again_on_the_backoff() {
    // The host's stream can't be made to fail on cue; the mock's can: a job longer than any a
    // stream carries is refused by the endpoint and stays at the head of the queue.
    let stream = MockStream::with_handles(vec![
        sealed_memfd(c"nonai2d-driver-test", 4096).unwrap(),
        sealed_memfd(c"nonai2d-driver-test", 4096).unwrap(),
    ]);
    stream.post_job(&[0; 4097]);
    let plan = Arc::new(Plan::default());
    let jobs: lemnos_core::StreamHandle = lemnos_core::Held::boxed(Box::new(stream.clone()));
    let mut driver = Nonai2dDriver::with_engine(Nonai2dDriverNeeds { jobs }, opener(&plan));
    assert_eq!(driver.tick(0), Some(250 * MS));
    assert_eq!(
        driver.health(),
        Health::Degraded("the job stream failed: trying it again")
    );
    assert_eq!(driver.tick(MS), Some(250 * MS), "not due: untouched");
    assert_eq!(driver.tick(250 * MS), Some(750 * MS));
    assert_eq!(driver.tick(750 * MS), Some(1750 * MS));
}

#[test]
fn a_thousand_jobs_through_the_driver_allocate_nothing() {
    let rig = Rig::start(&counter_board(), Arc::new(PerBuffer));
    let plan = Arc::new(Plan::default());
    let mut driver = driver_on(&rig.host, &plan);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(lease);
    let mut cycle = |n: u64| {
        let output = 1 + (n % 3) as u32;
        post(&mut consumer, &job(n, 0, output, 32, 32));
        assert_eq!(driver.tick(n * MS), Some((n + 1) * MS));
        let done = completion(&mut consumer);
        assert_eq!((done.buffer, done.identity, done.bytes), (output, n, BYTES));
        assert!(consumer.release(done.buffer));
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
    assert_eq!(plan.runs.load(Ordering::Relaxed), 1010);
}

#[test]
fn an_empty_or_non_text_device_setting_is_refused_at_bind() {
    for config in ["device = \"\"", "device = 3"] {
        let text = board("nonai2d", true).replace("device = \"/nonexistent/video9\"", config);
        let rig = Rig::start(&text, Arc::new(PerBuffer));
        rig.host.tick();
        let DeviceState::Backoff { reason, .. } = rig.host.device("g").unwrap().state else {
            panic!("bound with {config}");
        };
        assert!(
            reason.contains(&format!("{:?}", Status::INVALID_ARGUMENT)),
            "{config}: {reason}"
        );
    }
}

#[test]
fn every_conversion_listed_is_one_a_job_may_name() {
    let rig = Rig::start(&board("nonai2d", true), Arc::new(PerBuffer));
    rig.host.tick();
    for index in 0..49u16 {
        let pair = rig.call(0, 3, &index.to_le_bytes()).unwrap();
        let pair =
            lemnos_contract_image_convert::ConversionPair::decode(&mut Reader::new(&pair)).unwrap();
        assert!(lemnos_driver_nonai2d::convert::format(pair.from).is_some());
        assert!(lemnos_driver_nonai2d::convert::format(pair.to).is_some());
    }
    assert_eq!(rig.call(0, 3, &49u16.to_le_bytes()), Err(Status::NOT_FOUND));
    let info = rig.call(0, 1, &[]).unwrap();
    let info = lemnos_contract_image_convert::ConvertInfo::decode(&mut Reader::new(&info)).unwrap();
    assert_eq!((info.max_width, info.max_height), (1920, 1080));
}
