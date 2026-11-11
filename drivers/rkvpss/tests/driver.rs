//! The driver as a host runs it: bound from a board file, its methods called through the host,
//! its jobs posted by a consumer on the host's own frame stream and completed there. Sealed
//! memfds stand in for the stream's dma-bufs, and a played node for the device. No device is
//! opened.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_contract_image_scaler::{Counters, Filter, ImageRef, ImageScaler, JOB_BYTES, ScaleJob};
use lemnos_core::{CallerId, Decode, Encode, Health, Lifecycle, MethodId, Reader, Status, Writer};
use lemnos_driver_rkvpss::driver::{BoxedDevice, Opener, POLL_NS};
use lemnos_driver_rkvpss::jobs::Failure;
use lemnos_driver_rkvpss::{Channel, Device, Job, Rkvpss, RkvpssNeeds, State};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, STATE_FAULT};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Five pages: a 128 by 96 NV12 frame (18432 bytes) fits, one 16 rows taller doesn't.
const BYTES: u32 = 20480;
const NV12: u32 = u32::from_le_bytes(*b"NV12");
const MS: u64 = 1_000_000;
const EAGAIN: i32 = 11;
/// What a 64 by 48 NV12 output takes.
const OUT_BYTES: u32 = 64 * 48 * 3 / 2;

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
                sealed_memfd(c"rkvpss-driver-test", layout.stride()).map_err(|_| Status::IO)?;
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

fn board(driver: &str, config: &str, dma_buf: bool) -> String {
    let memory = if dma_buf {
        "memory = \"dma-buf\"\n"
    } else {
        ""
    };
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"v\"\ndriver = \"{driver}\"\nconfig = {{ {config} }}\n\
         [devices.streams.jobs]\n{memory}buffers = 4\nbuffer_bytes = {BYTES}\n\
         job_slots = 8\njob_bytes = {JOB_BYTES}\n"
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
            &[entry::<Rkvpss>(), entry::<FakeCounter>()],
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
            allocator,
        )
        .unwrap();
        Self { host, clock }
    }

    fn call(&self, method: u16, args: &[u8]) -> Result<Vec<u8>, Status> {
        let mut reply = [0u8; 256];
        let n = self.host.call(
            0,
            MethodId {
                contract: 0,
                method,
            },
            args,
            &mut reply,
        )?;
        Ok(reply[..n].to_vec())
    }

    fn counters(&self) -> Counters {
        Counters::decode(&mut Reader::new(&self.call(5, &[]).unwrap())).unwrap()
    }

    fn health(&self) -> Health {
        self.host.device("v").unwrap().health
    }

    fn after(&self, by: Duration) {
        self.clock.advance(by);
        self.host.tick();
    }
}

fn image(buffer: u32, width: u32, height: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc: NV12,
        stride: width,
    }
}

/// 128 by 96 NV12 in `input`, scaled to 64 by 48 in `output`.
fn job(identity: u64, input: u32, output: u32) -> ScaleJob {
    ScaleJob {
        identity,
        input: image(input, 128, 96),
        output: image(output, 64, 48),
        filter: Filter::Bilinear,
    }
}

fn post(consumer: &mut Consumer, job: &ScaleJob) {
    let mut bytes = [0u8; JOB_BYTES];
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
fn a_board_file_binds_it_and_the_host_reaches_its_contract_and_its_jobs() {
    let rig = Rig::start(
        &board(
            "rkvpss",
            "channel = 2, node = \"/nonexistent/video9\"",
            true,
        ),
        Arc::new(PerBuffer),
    );
    rig.host.tick();
    assert_eq!(rig.host.device("v").unwrap().driver, "rkvpss");
    let name = rig.call(4, &32u32.to_le_bytes()).unwrap();
    assert!(name.ends_with(b"jobs"), "{name:?}");
    // Channel 2 writes no RGB: the YUV formats the block also reads.
    let count = u16::decode(&mut Reader::new(&rig.call(2, &[]).unwrap())).unwrap();
    assert_eq!(count, 6);
    let first = u32::decode(&mut Reader::new(&rig.call(3, &0u16.to_le_bytes()).unwrap()));
    assert_eq!(first.unwrap(), NV12);
    assert_eq!(rig.call(3, &6u16.to_le_bytes()), Err(Status::NOT_FOUND));
    // No node at the configured path: the job fails, and the driver keeps trying.
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    rig.after(Duration::from_millis(1));
    post(&mut consumer, &job(0xCAFE, 0, 1));
    rig.after(Duration::from_millis(1));
    let done = completion(&mut consumer);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xCAFE, 0));
    assert_eq!(
        rig.health(),
        Health::Degraded("the scaler failed: reopening its node")
    );
    assert_eq!(rig.counters(), Counters { jobs: 0, errors: 1 });
}

#[test]
fn a_board_file_without_a_channel_is_refused() {
    let rig = Rig::start(&board("rkvpss", "", true), Arc::new(PerBuffer));
    rig.host.tick();
    assert!(matches!(rig.health(), Health::Failed(_)));
    for bad in ["channel = 6", "channel = -1", "channel = 0, node = \"\""] {
        let rig = Rig::start(&board("rkvpss", bad, true), Arc::new(PerBuffer));
        rig.host.tick();
        assert!(matches!(rig.health(), Health::Failed(_)), "{bad}");
    }
}

#[test]
fn a_stream_of_plain_shared_memory_is_refused_at_bind_before_any_consumer_comes() {
    // `HeapAllocator` makes plain shared memory, with no handles.
    let rig = Rig::start(
        &board("rkvpss", "channel = 0", false),
        Arc::new(HeapAllocator),
    );
    rig.host.tick();
    let DeviceState::Backoff { reason, .. } = rig.host.device("v").unwrap().state else {
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
    let rig = Rig::start(&board("rkvpss", "channel = 0", true), Arc::new(PerBuffer));
    rig.host.tick();
    assert_eq!(rig.host.device("v").unwrap().state, DeviceState::Running);
}

type Hook = Box<dyn FnMut(usize) + Send>;

/// What the played node saw, and how it is to fail.
#[derive(Default)]
struct Log {
    opens: AtomicU32,
    open_failures: AtomicU32,
    /// Runs that fail with this errno, while above zero.
    run_failures: AtomicU32,
    run_errno: AtomicI32,
    releases: AtomicU32,
    /// Each run: its input's width, and its input and output descriptors.
    runs: Mutex<Vec<(i32, i32, i32)>>,
    /// Called as each run starts, with the run's number from 1, so a test can act on the
    /// consumer while the block runs.
    hook: Mutex<Option<Hook>>,
}

impl Log {
    fn runs(&self) -> usize {
        self.runs.lock().unwrap().len()
    }

    fn last_width(&self) -> i32 {
        self.runs.lock().unwrap().last().unwrap().0
    }
}

struct Played(Arc<Log>);

impl Device for Played {
    fn select_offline(&mut self, _mask: u8) -> io::Result<()> {
        Ok(())
    }

    fn run(&mut self, job: &mut Job<'_>) -> io::Result<()> {
        let cfg = job.request();
        let input = cfg.input;
        let output = cfg.output[2];
        // Recorded with no allocation once the vector has room.
        let mut runs = self.0.runs.lock().unwrap();
        if runs.len() < runs.capacity() {
            runs.push(({ input.width }, { input.buf_fd }, { output.buf_fd }));
        }
        drop(runs);
        let number = self.0.runs();
        if let Some(hook) = self.0.hook.lock().unwrap().as_mut() {
            hook(number);
        }
        let left = self.0.run_failures.load(Ordering::Relaxed);
        if left > 0 {
            self.0.run_failures.store(left - 1, Ordering::Relaxed);
            return Err(io::Error::from_raw_os_error(
                self.0.run_errno.load(Ordering::Relaxed),
            ));
        }
        Ok(())
    }

    fn release_buffers(&mut self) -> io::Result<()> {
        self.0.releases.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn opener(log: &Arc<Log>) -> Opener {
    let log = Arc::clone(log);
    Box::new(move || {
        log.opens.fetch_add(1, Ordering::Relaxed);
        let left = log.open_failures.load(Ordering::Relaxed);
        if left > 0 {
            log.open_failures.store(left - 1, Ordering::Relaxed);
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Box::new(Played(Arc::clone(&log))) as BoxedDevice)
    })
}

/// A played node with room for `runs` records.
fn log(runs: usize) -> Arc<Log> {
    let log = Log::default();
    log.runs.lock().unwrap().reserve(runs);
    Arc::new(log)
}

/// The driver on channel 2 over a played node, on stream "jobs" of a device whose own driver
/// doesn't use it: the test plays the host's part in ticking it.
fn driver_on(host: &Host, log: &Arc<Log>) -> Rkvpss {
    let jobs: lemnos_core::StreamHandle =
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, "jobs").unwrap()));
    Rkvpss::with_device(RkvpssNeeds { jobs }, opener(log), Channel::Ch2)
}

fn counter_rig() -> Rig {
    Rig::start(&board("fake-counter", "", true), Arc::new(PerBuffer))
}

#[test]
fn jobs_run_on_the_handles_of_their_buffers_and_complete_on_the_ring() {
    let rig = counter_rig();
    let log = log(16);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer1 = consumer(&lease);
    assert_eq!(driver.tick(0), Some(POLL_NS));
    post(&mut consumer1, &job(1, 0, 1));
    driver.tick(MS);
    let done = completion(&mut consumer1);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 1, OUT_BYTES));
    assert_eq!(log.runs(), 1);
    assert_eq!(driver.health(), Health::Ok);
    assert_eq!(
        driver.server().holds_taken(),
        0,
        "the hold went with the frame"
    );
    // A new consumer: what the driver holds is given up, the job runs on the new memory, and
    // the kernel lets go of the old consumer's dma-bufs.
    rig.host.stream_detach(0, "jobs", CallerId(1)).unwrap();
    let second = rig.host.stream_attach(0, "jobs", CallerId(2)).unwrap();
    let mut consumer2 = consumer(&second);
    post(&mut consumer2, &job(2, 2, 3));
    driver.tick(2 * MS);
    let done = completion(&mut consumer2);
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 2, OUT_BYTES));
    assert_eq!(log.releases.load(Ordering::Relaxed), 1);
    assert_eq!(driver.counters().unwrap(), Counters { jobs: 2, errors: 0 });
}

#[test]
fn a_buffer_given_back_and_named_again_while_a_round_runs_completes_its_job() {
    let rig = counter_rig();
    let log = log(16);
    let mut driver = driver_on(&rig.host, &log);
    assert_eq!(driver.tick(0), Some(POLL_NS));
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let shared = Arc::new(Mutex::new(consumer(&lease)));
    // While the block runs the second job, the consumer takes the first one's completion,
    // gives its buffer back and posts a third job writing into that buffer.
    let hook = {
        let shared = Arc::clone(&shared);
        move |run: usize| {
            if run != 2 {
                return;
            }
            let mut consumer = shared.lock().unwrap();
            let done = completion(&mut consumer);
            assert_eq!((done.buffer, done.identity, done.bytes), (1, 1, OUT_BYTES));
            assert!(consumer.release(done.buffer, done.fence).unwrap());
            post(&mut consumer, &job(3, 0, 1));
        }
    };
    *log.hook.lock().unwrap() = Some(Box::new(hook));
    {
        let mut consumer = shared.lock().unwrap();
        post(&mut consumer, &job(1, 0, 1));
        post(&mut consumer, &job(2, 0, 2));
    }
    // One round reads all three: every job read gets its completion.
    driver.tick(MS);
    assert_eq!(log.runs(), 3);
    assert_eq!(driver.counters().unwrap(), Counters { jobs: 3, errors: 0 });
    let mut consumer = shared.lock().unwrap();
    let second = completion(&mut consumer);
    assert_eq!(
        (second.buffer, second.identity, second.bytes),
        (2, 2, OUT_BYTES)
    );
    let third = completion(&mut consumer);
    assert_eq!(
        (third.buffer, third.identity, third.bytes),
        (1, 3, OUT_BYTES)
    );
    assert_eq!(consumer.pop_frame().unwrap(), None);
}

#[test]
fn bytes_that_are_no_job_end_the_consumers_stream_as_faulted_and_the_next_consumer_is_served() {
    let rig = counter_rig();
    let log = log(16);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut broken = consumer(&lease);
    driver.tick(0);
    assert!(broken.push_job(&[1, 2, 3]).unwrap());
    post(&mut broken, &job(1, 0, 1));
    driver.tick(MS);
    // The consumer sees the fault, and the job it posted behind the bad bytes ran nowhere.
    assert_eq!(broken.state(), STATE_FAULT);
    assert_eq!(log.runs(), 0);
    assert_eq!(driver.server().last_failure(), Some(Failure::Undecodable));
    let stats = rig.host.stream_stats(0, "jobs").unwrap();
    assert_eq!((stats.faults, stats.attached), (1, false));
    assert_eq!((stats.producer_held, stats.consumer_held), (0, 0));
    assert_eq!(
        driver.health(),
        Health::Ok,
        "the consumer's mistake, not the driver's"
    );
    // The next consumer's job runs, and the kernel lets go of the old one's dma-bufs.
    let next = rig.host.stream_attach(0, "jobs", CallerId(2)).unwrap();
    let mut fresh = consumer(&next);
    post(&mut fresh, &job(2, 2, 3));
    driver.tick(2 * MS);
    let done = completion(&mut fresh);
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 2, OUT_BYTES));
    assert_eq!(log.runs(), 1);
    assert_eq!(rig.host.stream_stats(0, "jobs").unwrap().faults, 1);
}

/// Serves one job on a fresh driver and returns its completion, the failure and the runs.
fn one(job: &ScaleJob) -> (FrameSlot, Option<Failure>, usize) {
    let rig = counter_rig();
    let log = log(4);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    driver.tick(0);
    post(&mut consumer, job);
    driver.tick(MS);
    (
        completion(&mut consumer),
        driver.server().last_failure(),
        log.runs(),
    )
}

#[test]
fn an_image_larger_than_the_dma_buf_behind_its_buffer_never_reaches_the_block() {
    // 128 by 112 NV12 is 21504 bytes: past the five pages behind the input.
    let mut tall = job(7, 0, 1);
    tall.input.height = 112;
    let (done, failure, runs) = one(&tall);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 7, 0));
    assert_eq!((failure, runs), (Some(Failure::Surface), 0));
    // A stride that makes the output longer than its dma-buf.
    let mut wide = job(8, 0, 1);
    wide.output.stride = 288;
    let (done, failure, runs) = one(&wide);
    assert_eq!((done.identity, done.bytes), (8, 0));
    assert_eq!((failure, runs), (Some(Failure::Surface), 0));
    // The same frame, exactly fitting, runs.
    let mut fits = job(9, 0, 1);
    fits.output.stride = 272;
    let (done, failure, runs) = one(&fits);
    assert_eq!((done.identity, done.bytes), (9, 272 * 48 * 3 / 2));
    assert_eq!((failure, runs), (None, 1));
}

#[test]
fn what_the_channel_or_the_block_cant_do_never_reaches_it() {
    let refused = |job: &ScaleJob, failure: Failure| {
        let (done, got, runs) = one(job);
        assert_eq!((done.identity, done.bytes), (job.identity, 0), "{job:?}");
        assert_eq!((got, runs), (Some(failure), 0), "{job:?}");
    };
    let mut area = job(1, 0, 1);
    area.filter = Filter::Area;
    refused(&area, Failure::Format);
    // Channel 2 writes no RGB.
    let mut rgb = job(2, 0, 1);
    rgb.input.fourcc = u32::from_le_bytes(*b"RGB3");
    rgb.output.fourcc = rgb.input.fourcc;
    refused(&rgb, Failure::Format);
    let mut mixed = job(3, 0, 1);
    mixed.output.fourcc = u32::from_le_bytes(*b"NV21");
    refused(&mixed, Failure::Format);
    // An output stride the block would replace with its own.
    let mut odd = job(4, 0, 1);
    odd.output.stride = 72;
    refused(&odd, Failure::Surface);
    // Past 1:32.
    let mut far = job(5, 0, 1);
    far.input.width = 2048;
    far.input.stride = 2048;
    far.input.height = 32;
    far.output.width = 32;
    far.output.stride = 32;
    far.output.height = 32;
    refused(&far, Failure::Surface);
    let mut twice = job(6, 0, 0);
    twice.output.buffer = 0;
    let (_, got, runs) = {
        let rig = counter_rig();
        let log = log(4);
        let mut driver = driver_on(&rig.host, &log);
        let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
        let mut consumer = consumer(&lease);
        driver.tick(0);
        post(&mut consumer, &twice);
        driver.tick(MS);
        (
            consumer.pop_frame().unwrap(),
            driver.server().last_failure(),
            log.runs(),
        )
    };
    assert_eq!((got, runs), (Some(Failure::Buffers), 0));
}

#[test]
fn a_frame_that_times_out_holds_its_output_back_until_the_block_finishes_another() {
    let rig = counter_rig();
    let log = log(16);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    driver.tick(0);
    // The block doesn't finish in time: the kernel gives up, the block may still be writing.
    log.run_errno.store(EAGAIN, Ordering::Relaxed);
    log.run_failures.store(1, Ordering::Relaxed);
    post(&mut consumer, &job(0xD1, 0, 1));
    driver.tick(MS);
    assert_eq!(consumer.pop_frame().unwrap(), None, "nothing published");
    assert!(driver.server().held_back(1));
    assert_eq!(driver.server().holds_taken(), 1, "the driver's own hold");
    assert!(matches!(driver.health(), Health::Degraded(m) if m.contains("held back")));
    assert_eq!(driver.counters().unwrap(), Counters { jobs: 0, errors: 1 });
    // A job can't use it meanwhile, and the node is backing off.
    post(&mut consumer, &job(0xD2, 1, 2));
    driver.tick(2 * MS);
    assert_eq!(driver.server().last_failure(), Some(Failure::Buffers));
    assert_eq!(completion(&mut consumer).identity, 0xD2);
    assert_eq!(log.runs(), 1);
    // Due, the driver runs a frame of its own into the memory it holds. It times out too:
    // everything stays held.
    log.run_failures.store(1, Ordering::Relaxed);
    driver.tick(MS + 250 * MS);
    assert_eq!(log.runs(), 2);
    assert_eq!(log.last_width(), 32, "the driver's own frame");
    assert_eq!(consumer.pop_frame().unwrap(), None);
    assert_eq!(driver.server().holds_taken(), 1);
    // The next one finishes: the held-back output goes out as its job's failed completion,
    // and the hold is let go.
    driver.tick(MS + 750 * MS);
    assert_eq!(log.runs(), 3);
    let done = completion(&mut consumer);
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xD1, 0));
    assert_eq!(driver.server().holds_taken(), 0);
    assert!(!driver.server().held_back(1));
    assert_eq!(driver.health(), Health::Ok);
    // And jobs run again.
    consumer.release(done.buffer, done.fence).unwrap();
    post(&mut consumer, &job(0xD3, 0, 1));
    driver.tick(MS + 751 * MS);
    assert_eq!(completion(&mut consumer).bytes, OUT_BYTES);
}

#[test]
fn a_later_job_finishing_also_ends_the_hold() {
    let rig = counter_rig();
    let log = log(16);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    driver.tick(0);
    log.run_errno.store(EAGAIN, Ordering::Relaxed);
    log.run_failures.store(1, Ordering::Relaxed);
    post(&mut consumer, &job(1, 0, 1));
    driver.tick(0);
    // The input of the failed job is free to use: the block only reads it.
    post(&mut consumer, &job(2, 0, 2));
    driver.tick(250 * MS);
    // The driver's own frame ran first and finished; then the job.
    let first = completion(&mut consumer);
    assert_eq!((first.buffer, first.identity, first.bytes), (1, 1, 0));
    let second = completion(&mut consumer);
    assert_eq!(
        (second.buffer, second.identity, second.bytes),
        (2, 2, OUT_BYTES)
    );
    assert_eq!(driver.server().holds_taken(), 0);
}

#[test]
fn a_consumer_that_goes_while_an_output_is_held_back_leaves_the_hold_in_place() {
    let rig = counter_rig();
    let log = log(16);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer1 = consumer(&lease);
    driver.tick(0);
    log.run_errno.store(EAGAIN, Ordering::Relaxed);
    log.run_failures.store(1, Ordering::Relaxed);
    post(&mut consumer1, &job(1, 0, 1));
    driver.tick(0);
    rig.host.stream_detach(0, "jobs", CallerId(1)).unwrap();
    drop(lease);
    let second = rig.host.stream_attach(0, "jobs", CallerId(2)).unwrap();
    let mut consumer2 = consumer(&second);
    // The node is still backing off: the new consumer's job fails, but its buffers are taken
    // anew, the held-back one with them. The hold on the old memory stays, and the kernel's
    // attachments aren't released while it does.
    post(&mut consumer2, &job(2, 1, 2));
    driver.tick(MS);
    assert_eq!(completion(&mut consumer2).identity, 2);
    assert!(!driver.server().held_back(1));
    assert_eq!(driver.server().holds_taken(), 1);
    assert_eq!(log.releases.load(Ordering::Relaxed), 0);
    // Due: the driver's own frame finishes; the hold goes, nothing reaches the new consumer for
    // the old job, and the old attachments are released.
    driver.tick(250 * MS);
    assert_eq!(driver.server().holds_taken(), 0);
    assert_eq!(consumer2.pop_frame().unwrap(), None);
    assert_eq!(log.releases.load(Ordering::Relaxed), 1);
}

#[test]
fn a_node_that_wont_open_is_tried_again_doubling_to_8_s_and_holds_nothing_back() {
    let rig = counter_rig();
    let log = log(4);
    log.open_failures.store(u32::MAX, Ordering::Relaxed);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    let mut now = 0;
    for (n, want_ms) in [250, 500, 1000, 2000, 4000, 8000, 8000, 8000]
        .into_iter()
        .enumerate()
    {
        post(&mut consumer, &job(n as u64, 0, 1));
        driver.tick(now);
        let done = completion(&mut consumer);
        assert_eq!((done.identity, done.bytes), (n as u64, 0));
        consumer.release(done.buffer, done.fence).unwrap();
        let State::BackingOff { retry_at_ns, .. } = driver.server().scaler().state() else {
            panic!("backing off");
        };
        assert_eq!(retry_at_ns - now, want_ms * MS, "attempt {n}");
        assert_eq!(driver.server().holds_taken(), 0, "no frame was started");
        now = retry_at_ns;
    }
    log.open_failures.store(0, Ordering::Relaxed);
    post(&mut consumer, &job(99, 0, 1));
    driver.tick(now);
    assert_eq!(completion(&mut consumer).bytes, OUT_BYTES);
    assert_eq!(driver.health(), Health::Ok);
}

#[test]
fn a_thousand_jobs_through_the_driver_allocate_nothing() {
    let rig = counter_rig();
    let log = log(1100);
    let mut driver = driver_on(&rig.host, &log);
    let lease = rig.host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    let mut cycle = |n: u64| {
        let output = 1 + (n % 3) as u32;
        post(&mut consumer, &job(n, 0, output));
        assert_eq!(driver.tick(n * MS), Some((n + 1) * MS));
        let done = completion(&mut consumer);
        assert_eq!(
            (done.buffer, done.identity, done.bytes),
            (output, n, OUT_BYTES)
        );
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
    assert_eq!(log.runs(), 1010);
}
