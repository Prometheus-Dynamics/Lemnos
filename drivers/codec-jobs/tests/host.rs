//! The job server on a real host's job stream, with a consumer that gives a result buffer back
//! and then names it in a new job. The consumer's steps are placed at a chosen call of the
//! server's `next_job`, so the order between the server's collection of returns and its read of
//! the job is forced, not hoped for.
//!
//! The rule under test: a consumer that releases a buffer, then posts a job naming it, must be
//! served. The release is made visible to the server before the job is read, so the server
//! takes the buffer back before it checks the job.

#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use lemnos_contract_codec_block::work::Builder;
use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{
    BlockInfo, COMPLETION_BYTES, Completion, DATA_OFFSET, JOB_BYTES, Job, JobStatus,
};
use lemnos_core::{
    BusKind, CallerId, Config, Decode, Driver, Encode, Manifest, Need, NeedKind, Reader, Resources,
    Result, StreamKind, Writer,
};
use lemnos_driver_codec_jobs::{Block, Buffers, Finished, Server, Start};
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_host::board::parse;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{Consumer, STATE_FAULT};

const CONSUMER: CallerId = CallerId(9);
/// The stream's buffers: the work buffer is 10, the result 11.
const WORK: u32 = 10;
const RESULT: u32 = 11;

/// A driver that serves `jobs` as a job-driven stream and does nothing else.
struct JobDriver;

impl Driver for JobDriver {
    const MANIFEST: Manifest = Manifest {
        name: "codec-jobs-host-test",
        needs: &[Need {
            name: "jobs",
            kind: NeedKind::Bus(BusKind::Stream),
            optional: true,
            stream: Some(StreamKind::JobDriven),
        }],
        provides: &[],
    };

    fn bind(_: &mut Resources<'_>, _: &Config<'_>) -> Result<Self> {
        Ok(Self)
    }

    fn describe(&self, _: &mut lemnos_core::Description) {}

    fn dispatch(
        &mut self,
        _: lemnos_core::MethodId,
        _: &[u8],
        _: &mut lemnos_core::Reply<'_>,
    ) -> lemnos_core::Status {
        lemnos_core::Status::NOT_SUPPORTED
    }
}

/// A block that finishes every job the moment it starts it, with no read-back.
#[derive(Default)]
struct Instant {
    started: u32,
}

impl Block for Instant {
    fn name(&self) -> &str {
        "test:instant"
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: 4096,
            max_refs: 4,
            max_items: 4,
            max_read_back: 0,
            max_buffers: 64,
        }
    }

    fn start(&mut self, _: &Work<'_>, _: &Buffers<'_>, _: &mut [u8], _: u64) -> Start {
        self.started += 1;
        Start::Done(0)
    }

    fn poll(&mut self, _: &mut [u8], _: u64) -> Option<Finished> {
        None
    }

    fn release(&mut self) {}
}

/// Runs a consumer step at a chosen call of `next_job`: the `calls`-th one from when it is set,
/// counting from 1, then never again.
struct Step {
    calls: u32,
    run: Box<dyn FnMut()>,
}

/// The producer's end, with a consumer's step that runs inside the server's read of a job.
struct Hooked {
    inner: ProducerEndpoint,
    step: Rc<RefCell<Option<Step>>>,
}

/// The hal result, with its error kind.
type R<T> = std::result::Result<T, ErrorKind>;

impl StreamEndpoint for Hooked {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.inner.buffer_count()
    }

    fn buffer_kind(&self) -> BufferKind {
        self.inner.buffer_kind()
    }

    fn buffer_current(&self, buffer: u32) -> R<bool> {
        self.inner.buffer_current(buffer)
    }

    fn publish(&mut self, frame: StreamFrame) -> R<()> {
        self.inner.publish(frame)
    }

    fn next_buffer(&mut self) -> R<Option<u32>> {
        self.inner.next_buffer()
    }

    fn next_job(&mut self, out: &mut [u8]) -> R<Option<usize>> {
        let due = match self.step.borrow_mut().as_mut() {
            Some(step) => {
                step.calls -= 1;
                step.calls == 0
            }
            None => false,
        };
        if due {
            let mut step = self.step.borrow_mut().take().expect("counted above");
            (step.run)();
        }
        self.inner.next_job(out)
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> R<bool> {
        self.inner.write(buffer, offset, data)
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> R<bool> {
        self.inner.read(buffer, offset, out)
    }

    fn buffer_handle(&self, buffer: u32) -> R<Option<BufferHandle<'_>>> {
        self.inner.buffer_handle(buffer)
    }

    fn fault_consumer(&mut self) -> R<()> {
        self.inner.fault_consumer()
    }
}

/// A consumer on the host's stream: the work and result buffers are its to fill.
type Shared = Rc<RefCell<Consumer>>;

/// Writes a work of no references and no items into `WORK` and posts a job for `result`.
fn post(ring: &Shared, identity: u64, result: u32) {
    post_on(&mut ring.borrow_mut(), identity, result);
}

fn post_on(ring: &mut Consumer, identity: u64, result: u32) {
    let mut bytes = vec![0u8; 4096];
    let builder = Builder::new(&mut bytes).unwrap();
    let len = builder.finish();
    assert!(ring.write(WORK, 0, &bytes[..len]));
    let job = Job {
        identity,
        work: WORK,
        work_bytes: len as u32,
        result,
    };
    let mut encoded = [0u8; JOB_BYTES];
    job.encode(&mut Writer::new(&mut encoded)).unwrap();
    assert!(ring.jobs().post(&encoded).unwrap());
}

/// The next completion, if one has come, with the release that goes with it: the frame is
/// popped and its buffer released, as a consumer does before it reuses that buffer.
fn take(ring: &Shared) -> Option<(u64, JobStatus)> {
    take_on(&mut ring.borrow_mut())
}

fn take_on(ring: &mut Consumer) -> Option<(u64, JobStatus)> {
    let frame = ring.pop_frame().unwrap()?;
    let mut record = [0u8; COMPLETION_BYTES];
    assert!(ring.read(frame.buffer, 0, &mut record));
    let record = Completion::decode(&mut Reader::new(&record)).unwrap();
    assert_eq!(frame.bytes as usize, DATA_OFFSET + record.count as usize);
    assert!(ring.release(frame.buffer));
    Some((frame.identity, record.status))
}

struct Rig {
    host: Host,
    server: Server,
    block: Instant,
    producer: Hooked,
    ring: Shared,
    step: Rc<RefCell<Option<Step>>>,
}

/// A host with the job stream `jobs` of device 0, and nothing claimed.
fn host() -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"codec-jobs-host-test\"\n\
         [devices.streams.jobs]\nbuffers = 12\nbuffer_bytes = 4096\n\
         job_slots = 8\njob_bytes = {JOB_BYTES}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<JobDriver>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

impl Rig {
    fn start() -> Self {
        let host = host();
        let step = Rc::new(RefCell::new(None));
        let producer = Hooked {
            inner: host.stream_producer(0, "jobs").unwrap(),
            step: Rc::clone(&step),
        };
        let lease = host.stream_attach(0, "jobs", CONSUMER).unwrap();
        let ring = Rc::new(RefCell::new(lease.consumer_writable().unwrap()));
        Self {
            host,
            server: Server::new(),
            block: Instant::default(),
            producer,
            ring,
            step,
        }
    }

    /// One serve of the server, at the next instant.
    fn serve(&mut self) {
        self.server
            .serve(&mut self.producer, &mut self.block, 1)
            .unwrap();
    }

    fn faulted(&self) -> bool {
        self.ring.borrow().state() == STATE_FAULT
    }

    fn refused(&self) -> u64 {
        self.server.counters().refused
    }
}

/// A consumer that releases a result before it names the buffer in a job, with the job posted
/// before the server's serve: the order every consumer keeps. Served before and after the fix.
#[test]
fn a_result_released_before_the_serve_is_named_again_and_served() {
    let mut rig = Rig::start();
    post(&rig.ring, 1, RESULT);
    rig.serve();
    assert_eq!(take(&rig.ring), Some((1, JobStatus::Done)));
    post(&rig.ring, 2, RESULT);
    rig.serve();
    assert_eq!(rig.refused(), 0, "the job was refused");
    assert!(!rig.faulted(), "the consumer was faulted for it");
    assert_eq!(take(&rig.ring), Some((2, JobStatus::Done)));
    assert_eq!(rig.block.started, 2);
    let stats = rig.host.stream_stats(0, "jobs").unwrap();
    assert_eq!((stats.faults, stats.refused), (0, 0), "{stats:?}");
}

/// The server collects the returns of a serve, and the consumer then releases the result and
/// posts a job naming it before the server reads that job, in the same serve. A consumer that
/// releases before it posts must be served. Refused and faulted before the fix: the server took
/// its returns once, before the release, and then checked the job against that snapshot.
#[test]
fn a_result_released_after_a_serves_collection_is_named_again_and_served() {
    let mut rig = Rig::start();
    post(&rig.ring, 1, RESULT);
    rig.serve();
    // The result is complete and unread: the consumer still holds it.
    let ring = Rc::clone(&rig.ring);
    *rig.step.borrow_mut() = Some(Step {
        calls: 1,
        run: Box::new(move || {
            // Release, then post: in this order, from this one thread.
            assert_eq!(take(&ring), Some((1, JobStatus::Done)));
            post(&ring, 2, RESULT);
        }),
    });
    rig.serve();
    assert_eq!(rig.refused(), 0, "the job was refused");
    assert!(!rig.faulted(), "the consumer was faulted for it");
    assert_eq!(take(&rig.ring), Some((2, JobStatus::Done)));
    assert_eq!(rig.block.started, 2);
}

/// Within one serve: the server completes a job, and the consumer, having seen the completion,
/// releases its result and posts a job naming it, which the same serve reads next. The same
/// order as above, with no collection between the two.
#[test]
fn a_result_released_after_its_completion_in_the_same_serve_is_named_again_and_served() {
    let mut rig = Rig::start();
    post(&rig.ring, 1, RESULT);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (ring, log) = (Rc::clone(&rig.ring), Rc::clone(&seen));
    *rig.step.borrow_mut() = Some(Step {
        // The second read of a job in this serve: the first was job 1, and it is complete.
        calls: 2,
        run: Box::new(move || {
            log.borrow_mut().push(take(&ring));
            post(&ring, 2, RESULT);
        }),
    });
    rig.serve();
    assert_eq!(*seen.borrow(), [Some((1, JobStatus::Done))]);
    assert_eq!(rig.refused(), 0, "the job was refused");
    assert!(!rig.faulted(), "the consumer was faulted for it");
    assert_eq!(take(&rig.ring), Some((2, JobStatus::Done)));
    assert_eq!(rig.block.started, 2);
}

/// The negative control: a result the consumer has not released is still held by it. Naming it
/// in a job is the consumer's broken rule, and the stream is faulted as before. The fix must not
/// make an unreleased buffer look returned.
#[test]
fn a_result_the_consumer_still_holds_is_refused_and_faults_its_stream() {
    let mut rig = Rig::start();
    post(&rig.ring, 1, RESULT);
    rig.serve();
    // No release: the completion is still in the consumer's hands.
    post(&rig.ring, 2, RESULT);
    rig.serve();
    assert!(rig.faulted());
    assert_eq!(rig.block.started, 1);
}

/// The same order on two threads: the consumer releases a result and posts the next job naming
/// it, while the server serves on its own thread, taking returns and jobs whenever it looks.
/// Nothing is ever refused, and no consumer is faulted, however the two interleave.
#[test]
fn a_consumer_on_its_own_thread_that_releases_then_names_a_result_is_never_refused() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant as Clock};

    const ROUNDS: u64 = 3000;
    let host = host();
    let mut producer = host.stream_producer(0, "jobs").unwrap();
    let mut ring = host
        .stream_attach(0, "jobs", CONSUMER)
        .unwrap()
        .consumer_writable()
        .unwrap();
    let done = AtomicBool::new(false);
    let (completed, faulted) = std::thread::scope(|scope| {
        let served = scope.spawn(|| {
            let mut server = Server::new();
            let mut block = Instant::default();
            while !done.load(Ordering::Acquire) {
                server.serve(&mut producer, &mut block, 1).unwrap();
                std::thread::yield_now();
            }
            server.counters()
        });
        let mut completed = 0;
        let mut faulted = false;
        post_on(&mut ring, 1, RESULT);
        'rounds: for identity in 1..=ROUNDS {
            let deadline = Clock::now() + Duration::from_secs(20);
            loop {
                if let Some((got, status)) = take_on(&mut ring) {
                    assert_eq!((got, status), (identity, JobStatus::Done));
                    completed += 1;
                    break;
                }
                if ring.state() == STATE_FAULT {
                    faulted = true;
                    break 'rounds;
                }
                assert!(Clock::now() < deadline, "no completion for job {identity}");
                std::thread::yield_now();
            }
            if identity < ROUNDS {
                post_on(&mut ring, identity + 1, RESULT);
            }
        }
        done.store(true, Ordering::Release);
        let counters = served.join().unwrap();
        assert_eq!(counters.refused, 0, "a job was refused");
        (completed, faulted)
    });
    assert!(
        !faulted,
        "the consumer was faulted after {completed} completions"
    );
    assert_eq!(completed, ROUNDS);
}
