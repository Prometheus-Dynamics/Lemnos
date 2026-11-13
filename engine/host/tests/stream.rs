//! Frame streams through the host: a simulated producer and a consumer on the real ring.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lemnos_core::{
    CallerId, Config, Lifecycle, MethodId, Result, Status, StreamHandle, contract, driver,
};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_host::board::parse;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimFrameSource};
use lemnos_wire::ring::{Consumer, STATE_CLOSED, STATE_FAULT, STATE_OPEN, fds};
use shm_ring::{FieldId, Flags, MemMut, SampleTime, WriteMap};

const CONSUMER: CallerId = CallerId(7);
const FRAME_BYTES: usize = 64;

#[contract("example.emitter", version = 1)]
pub trait Emitting {
    #[method(1)]
    fn emit(&mut self) -> Result<u32>;
}

#[driver("emitter", provides = [Emitting])]
pub struct Emitter {
    /// Publishes a frame each time `emit` is called, with no job behind it: free-running.
    #[needs(free)]
    frames: StreamHandle,
}

impl Lifecycle for Emitter {
    type Needs = EmitterNeeds;

    fn start(needs: EmitterNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
        })
    }
}

impl Emitting for Emitter {
    fn emit(&mut self) -> Result<u32> {
        let buffer = self
            .frames
            .next_buffer()
            .map_err(|_| Status::IO)?
            .ok_or(Status::AGAIN)?;
        self.frames
            .publish(StreamFrame {
                buffer,
                bytes: 8,
                sequence: 41,
                identity: 0xABCD,
                timestamp_ns: 5,
            })
            .map_err(|_| Status::IO)?;
        Ok(buffer)
    }
}

const BOARD: &str = "
[[devices]]
id = \"a\"
driver = \"fake-counter\"
[devices.streams.frames]
buffers = 4
buffer_bytes = 64
job_slots = 2
job_bytes = 16
[devices.streams.other]
buffers = 2
buffer_bytes = 32
[[devices]]
id = \"e\"
driver = \"emitter\"
[devices.streams.frames]
buffers = 2
buffer_bytes = 16
";

fn start() -> Host {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{BOARD}");
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>(), entry::<Emitter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

/// The descriptors of a stream that a test keeps to play the consumer's raw side with: the job
/// ring the consumer writes, and the marks page it writes. The consumer owns the rest.
struct Raw {
    job_ring: OwnedFd,
    marks: OwnedFd,
}

/// Attaches `caller` to the frames stream, and keeps the raw descriptors beside the consumer.
fn attach_raw(host: &Host, caller: CallerId) -> (Consumer, Raw) {
    let lease = host.stream_attach(0, "frames", caller).unwrap();
    let raw = Raw {
        job_ring: lease.descriptors[fds::JOB_RING].try_clone().unwrap(),
        marks: lease.descriptors[fds::MARKS].try_clone().unwrap(),
    };
    (lease.consumer().unwrap(), raw)
}

fn produce(source: &mut SimFrameSource, producer: &mut ProducerEndpoint) -> bool {
    let writer = producer.writer();
    source
        .produce(producer, &mut |buffer, bytes| {
            writer.write(buffer, 0, bytes)
        })
        .unwrap()
}

/// Checks that a fault hit, on this stream only.
fn assert_cut_off(host: &Host, consumer: &Consumer) {
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.faults, 1, "{stats:?}");
    assert!(!stats.attached);
    assert_eq!(consumer.state(), STATE_FAULT);
    assert_eq!(stats.consumer_held, 0, "the buffers came back");
    let other = host.stream_stats(0, "other").unwrap();
    assert_eq!(other.faults, 0);
}

/// Posts more jobs than the job ring holds, without the host collecting any: what a consumer that
/// ignores the acks does. The host's collector finds jobs it lost, and the stream faults.
fn overrun_jobs(raw: &Raw) {
    let mut ring = shm_ring::Writer::attach(raw.job_ring.try_clone().unwrap()).unwrap();
    for n in 0..8u8 {
        ring.publish(FieldId(0), SampleTime::ZERO, Flags::NONE, &[n])
            .unwrap();
    }
}

#[test]
fn frames_arrive_in_order_with_contents_and_identities_over_many_cycles() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = lease.consumer().unwrap();
    let mut source = SimFrameSource::new(FRAME_BYTES);
    let mut read = [0u8; FRAME_BYTES];
    let mut expected = [0u8; FRAME_BYTES];
    for sequence in 0..1000u64 {
        assert!(produce(&mut source, &mut producer), "frame {sequence}");
        let frame = consumer.pop_frame().unwrap().unwrap();
        assert_eq!(frame.sequence, sequence);
        assert_eq!(frame.identity, SimFrameSource::identity(sequence));
        assert_eq!(frame.timestamp_ns, sequence * 1000);
        assert_eq!(frame.bytes as usize, FRAME_BYTES);
        assert!(consumer.read(frame.buffer, 0, &mut read));
        SimFrameSource::fill(sequence, &mut expected);
        assert_eq!(read, expected, "frame {sequence}");
        assert!(consumer.release(frame.buffer));
    }
    assert_eq!(consumer.pop_frame().unwrap(), None);
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.published, 1000);
    assert_eq!(stats.delivered, 1000);
    // Every frame was read and released before its buffer went round again: a replacement the
    // consumer had already taken is not a loss, so nothing is dropped.
    assert_eq!(stats.dropped, 0);
    assert!(stats.returned > 0, "buffers went round");
    assert_eq!(stats.attachments, 1);
}

#[test]
fn a_consumer_that_holds_every_buffer_stops_the_producer_without_a_fault() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..4 {
        assert!(produce(&mut source, &mut producer));
    }
    // The consumer pops and holds every frame: nothing is reclaimable, so the producer stops.
    let held: Vec<_> = (0..4)
        .map(|_| consumer.pop_frame().unwrap().unwrap())
        .collect();
    assert!(!produce(&mut source, &mut producer), "no buffer free");
    assert!(consumer.release(held[0].buffer));
    assert!(produce(&mut source, &mut producer), "released");
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.faults, 0);
    assert_eq!(stats.free + stats.producer_held + stats.consumer_held, 4);
}

#[test]
fn an_unpopped_frame_is_reclaimed_only_when_the_producer_runs_out_of_buffers() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..4 {
        assert!(produce(&mut source, &mut producer));
    }
    // Free buffers are used first, so nothing is reclaimed while the producer has any. The four
    // published frames are unheld, so they are free to take back: the count shows that.
    assert_eq!(host.stream_stats(0, "frames").unwrap().free, 4);
    // A fifth frame takes back the oldest unread one; the consumer sees it skipped.
    assert!(produce(&mut source, &mut producer));
    // The lost frame reads as a skip, so the first pop may come back empty; the next has frame 1.
    let first = (0..3)
        .find_map(|_| consumer.pop_frame().unwrap())
        .expect("a frame");
    assert_eq!(first.sequence, 1, "frame 0 was reclaimed");
}

/// The consumer reads the newest frame and lets go: the producer gets its buffer back with no
/// further publish, and the frame counts as taken, not dropped.
#[test]
fn a_released_newest_frame_comes_back_without_another_publish() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let published = producer.next_buffer().unwrap().unwrap();
    producer
        .publish(StreamFrame {
            buffer: published,
            bytes: FRAME_BYTES as u32,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
        })
        .unwrap();
    let frame = consumer.pop_frame().unwrap().expect("the frame");
    assert_eq!(frame.buffer, published);
    assert!(consumer.release(frame.buffer));
    // Three buffers were never used, so they come first; the fourth is the released frame.
    let mut taken = Vec::new();
    for _ in 0..4 {
        taken.push(producer.next_buffer().unwrap().expect("a buffer"));
    }
    assert_eq!(taken.last(), Some(&published), "{taken:?}");
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.dropped, stats.faults), (0, 0));
    assert_eq!(stats.returned, 1);
}

#[test]
fn a_frame_with_no_consumer_is_dropped_and_its_buffer_comes_back() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..16 {
        assert!(produce(&mut source, &mut producer));
    }
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(
        (stats.published, stats.dropped, stats.delivered),
        (16, 16, 0)
    );
    assert_eq!(stats.free, 4);
}

#[test]
fn a_buffer_granted_before_a_consumer_attached_is_dropped_not_delivered() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    producer
        .publish(StreamFrame {
            buffer,
            bytes: 8,
            sequence: 1,
            identity: 2,
            timestamp_ns: 3,
        })
        .unwrap();
    assert_eq!(consumer.pop_frame().unwrap(), None);
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.dropped, stats.delivered, stats.free), (1, 0, 4));
}

#[test]
fn a_producer_cant_publish_or_write_what_it_doesnt_hold() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let writer = producer.writer();
    let _lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let frame = StreamFrame {
        buffer: 0,
        bytes: 8,
        sequence: 1,
        identity: 2,
        timestamp_ns: 3,
    };
    assert_eq!(producer.publish(frame), Err(ErrorKind::InvalidInput));
    assert_eq!(
        producer.publish(StreamFrame { buffer: 9, ..frame }),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(writer.write(0, 0, &[1]), Err(ErrorKind::InvalidInput));
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(
        producer.publish(StreamFrame {
            buffer,
            bytes: 65,
            ..frame
        }),
        Err(ErrorKind::InvalidInput),
        "longer than a buffer"
    );
    assert_eq!(
        writer.write(buffer, 60, &[0; 8]),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(
        writer.write(buffer, usize::MAX, &[1]),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(writer.write(buffer, 60, &[0; 4]), Ok(true));
}

/// What a driver has: the endpoint behind the trait, with nothing of the host's own type.
fn write_and_publish(stream: &mut dyn StreamEndpoint<Error = ErrorKind>, bytes: &[u8]) -> u32 {
    let buffer = stream.next_buffer().unwrap().unwrap();
    assert_eq!(stream.write(buffer, 0, bytes), Ok(true));
    stream
        .publish(StreamFrame {
            buffer,
            bytes: bytes.len() as u32,
            sequence: 1,
            identity: 2,
            timestamp_ns: 3,
        })
        .unwrap();
    buffer
}

#[test]
fn a_driver_writes_a_frame_through_the_trait_and_the_consumer_reads_it() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    assert_eq!(
        StreamEndpoint::write(&mut producer, 0, 0, &[1]),
        Err(ErrorKind::InvalidInput),
        "a buffer the producer doesn't hold"
    );
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let sent: [u8; FRAME_BYTES] = std::array::from_fn(|i| i as u8 ^ 0x5a);
    let buffer = write_and_publish(&mut producer, &sent);
    let frame = consumer.pop_frame().unwrap().unwrap();
    assert_eq!(frame.buffer, buffer);
    let mut read = [0u8; FRAME_BYTES];
    assert!(consumer.read(frame.buffer, 0, &mut read));
    assert_eq!(read, sent);
    assert_eq!(
        StreamEndpoint::write(&mut producer, buffer, 0, &[1]),
        Err(ErrorKind::InvalidInput),
        "published, so no longer the producer's"
    );
}

#[test]
fn a_driver_reads_what_the_consumer_put_in_a_buffer_through_the_trait() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut out = [0u8; 4];
    assert_eq!(
        StreamEndpoint::read(&mut producer, 0, 0, &mut out),
        Err(ErrorKind::InvalidInput),
        "a buffer the producer doesn't hold"
    );
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(
        StreamEndpoint::read(&mut producer, buffer, 0, &mut out),
        Ok(false),
        "no consumer, so nothing to read"
    );
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer_writable()
        .unwrap();
    assert!(consumer.write(buffer, 8, &[9, 8, 7, 6]));
    let stream: &mut dyn StreamEndpoint<Error = ErrorKind> = &mut producer;
    assert_eq!(stream.read(buffer, 8, &mut out), Ok(true));
    assert_eq!(out, [9, 8, 7, 6]);
    assert_eq!(
        stream.read(buffer, FRAME_BYTES - 2, &mut out),
        Err(ErrorKind::InvalidInput),
        "past the end of the buffer"
    );
    assert_eq!(
        stream.read(buffer, usize::MAX, &mut out),
        Err(ErrorKind::InvalidInput)
    );
}

#[test]
fn jobs_reach_the_producer_with_their_bytes() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    assert!(consumer.jobs().post(&[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap());
    let mut out = [0u8; 16];
    assert_eq!(producer.next_job(&mut out), Ok(Some(9)));
    assert_eq!(&out[..9], [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(producer.next_job(&mut out), Ok(None));
}

#[test]
fn a_job_too_large_for_the_driver_buffer_is_the_drivers_problem() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    assert!(consumer.jobs().post(&[1, 2, 3]).unwrap());
    let mut small = [0u8; 2];
    assert_eq!(producer.next_job(&mut small), Err(ErrorKind::InvalidInput));
    assert!(host.stream_stats(0, "frames").unwrap().attached);
}

#[test]
fn an_unknown_device_or_name_is_refused() {
    let host = start();
    assert_eq!(
        host.stream_attach(99, "frames", CONSUMER).err(),
        Some(Status::NOT_FOUND)
    );
    assert_eq!(
        host.stream_attach(0, "nope", CONSUMER).err(),
        Some(Status::NOT_FOUND)
    );
}

#[test]
fn a_second_consumer_is_refused_until_the_first_goes() {
    let host = start();
    let _first = host.stream_attach(0, "frames", CONSUMER).unwrap();
    assert_eq!(
        host.stream_attach(0, "frames", CallerId(8)).err(),
        Some(Status::BUSY)
    );
    host.stream_detach(0, "frames", CONSUMER).unwrap();
    assert!(host.stream_attach(0, "frames", CallerId(8)).is_ok());
}

#[test]
fn only_the_holder_can_detach() {
    let host = start();
    let _lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    assert_eq!(
        host.stream_detach(0, "frames", CallerId(99)),
        Err(Status::NOT_FOUND)
    );
    assert!(host.stream_stats(0, "frames").unwrap().attached);
    host.stream_detach(0, "frames", CONSUMER).unwrap();
    assert_eq!(
        host.stream_detach(0, "frames", CONSUMER),
        Err(Status::NOT_FOUND)
    );
}

#[test]
fn a_producer_is_exclusive_and_dropping_it_gives_back_what_it_held() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    assert_eq!(host.stream_producer(0, "frames").err(), Some(Status::BUSY));
    producer.next_buffer().unwrap().unwrap();
    assert_eq!(host.stream_stats(0, "frames").unwrap().producer_held, 1);
    drop(producer);
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(
        (stats.claimed, stats.producer_held, stats.free),
        (false, 0, 4)
    );
    assert!(host.stream_producer(0, "frames").is_ok());
}

#[test]
fn the_consumer_going_tears_the_stream_down_and_frees_its_buffers() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..3 {
        assert!(produce(&mut source, &mut producer));
    }
    let _held = consumer.pop_frame().unwrap().unwrap();
    // Only the frame the consumer holds counts as its; the other two were published and not held.
    assert_eq!(host.stream_stats(0, "frames").unwrap().consumer_held, 1);

    host.caller_gone(CONSUMER);

    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(!stats.attached);
    assert_eq!((stats.free, stats.consumer_held, stats.faults), (4, 0, 0));
    assert_eq!(consumer.state(), STATE_CLOSED);
    // Someone else can have it now, on memory the first consumer doesn't share: it opens clean,
    // and the first consumer's end stays closed.
    let next = host
        .stream_attach(0, "frames", CallerId(8))
        .unwrap()
        .consumer()
        .unwrap();
    assert_eq!(next.state(), STATE_OPEN);
    assert_eq!(consumer.state(), STATE_CLOSED);
}

#[test]
fn another_callers_going_changes_nothing() {
    let host = start();
    let _lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    host.caller_gone(CallerId(99));
    assert!(host.stream_stats(0, "frames").unwrap().attached);
}

/// Runs `craft` with a frame out and the consumer holding it, and the producer holding one
/// buffer, then lets the producer look.
fn fault_case(craft: impl FnOnce(&Raw)) -> (Host, Consumer, ProducerEndpoint) {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let (mut consumer, raw) = attach_raw(&host, CONSUMER);
    let mut source = SimFrameSource::new(FRAME_BYTES);
    assert!(produce(&mut source, &mut producer));
    let _frame = consumer.pop_frame().unwrap().unwrap();
    let _held = producer.next_buffer().unwrap().unwrap();
    craft(&raw);
    (host, consumer, producer)
}

// The consumer's end of the hold marks and of the returns are not on the consumer's API as
// misuse: a consumer can only set and clear its own marks, and a return is not a message the host
// reads. The three faults these tests named (a buffer returned twice, a returned number out of the
// pool, a return with the wrong fence) have nothing to trip on now.

#[test]
fn an_impossible_returns_head_is_a_fault_and_never_hangs() {
    // A consumer whose marks page is stuck mid-change, its version word left odd. The host can't
    // count its holds, and cuts it off once it has waited the hold limit (ten seconds by default).
    let (host, consumer, mut producer) = fault_case(|raw| {
        let marks = WriteMap::new(raw.marks.try_clone().unwrap()).unwrap();
        marks.store(0, 1, Ordering::SeqCst);
    });
    let started = Instant::now();
    while host.stream_stats(0, "frames").unwrap().faults == 0 {
        assert!(started.elapsed() < Duration::from_secs(15), "never cut off");
        let _ = producer.next_buffer().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_cut_off(&host, &consumer);
}

#[test]
fn an_impossible_job_is_a_fault_and_reads_as_no_job() {
    // Jobs the consumer posted past what the ring holds, with none collected: the host has lost
    // some of them, and nothing posted after them can be trusted in order.
    let (host, consumer, mut producer) = fault_case(overrun_jobs);
    let mut out = [0u8; 64];
    assert_eq!(producer.next_job(&mut out), Ok(None));
    assert_cut_off(&host, &consumer);
}

#[test]
fn after_a_fault_the_producer_carries_on_and_a_new_consumer_can_attach() {
    let (host, consumer, mut producer) = fault_case(overrun_jobs);
    let mut source = SimFrameSource::new(FRAME_BYTES);
    let mut out = [0u8; 64];
    assert_eq!(producer.next_job(&mut out), Ok(None));
    assert_cut_off(&host, &consumer);
    assert!(produce(&mut source, &mut producer), "keeps producing");

    let mut fresh = host
        .stream_attach(0, "frames", CallerId(8))
        .unwrap()
        .consumer()
        .unwrap();
    assert!(produce(&mut source, &mut producer));
    assert!(fresh.pop_frame().unwrap().is_some());
}

#[test]
fn a_producer_faults_the_consumer_that_posted_what_is_no_job() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut source = SimFrameSource::new(FRAME_BYTES);
    assert!(produce(&mut source, &mut producer));
    let _frame = consumer.pop_frame().unwrap().unwrap();
    let _held = producer.next_buffer().unwrap().unwrap();
    assert!(consumer.jobs().post(b"not a job").unwrap());
    let mut out = [0u8; 16];
    assert_eq!(producer.next_job(&mut out), Ok(Some(9)));
    let stream: &mut dyn StreamEndpoint<Error = ErrorKind> = &mut producer;
    stream.fault_consumer().unwrap();
    assert_cut_off(&host, &consumer);
    assert_eq!(consumer.state(), STATE_FAULT);
    // Jobs it had waiting went with it, and what the producer held has nothing behind it.
    assert_eq!(producer.next_job(&mut out), Ok(None));
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.producer_held, 1);
    assert!((0..4).any(|buffer| producer.write(buffer, 0, &[1]) == Ok(false)));

    // The next consumer starts clean and the producer serves it.
    let mut fresh = host
        .stream_attach(0, "frames", CallerId(8))
        .unwrap()
        .consumer()
        .unwrap();
    assert!(produce(&mut source, &mut producer));
    assert!(fresh.pop_frame().unwrap().is_some());
    assert_eq!(host.stream_stats(0, "frames").unwrap().faults, 1);
}

#[test]
fn faulting_with_no_consumer_attached_does_nothing() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    producer.fault_consumer().unwrap();
    let consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    host.stream_detach(0, "frames", CONSUMER).unwrap();
    producer.fault_consumer().unwrap();
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.faults, 0, "{stats:?}");
    assert_eq!(consumer.state(), STATE_CLOSED, "left, not faulted");
}

#[test]
fn a_driver_that_needs_a_stream_gets_this_one_and_its_frames_arrive() {
    let host = start();
    let mut consumer = host
        .stream_attach(1, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut reply = [0u8; 8];
    let method = MethodId {
        contract: 0,
        method: 1,
    };
    host.call(1, method, &[], &mut reply).unwrap();
    let frame = consumer.pop_frame().unwrap().unwrap();
    assert_eq!((frame.sequence, frame.identity), (41, 0xABCD));
    assert!(host.stream_stats(1, "frames").unwrap().claimed);
}

#[test]
fn a_device_going_off_lets_go_of_the_stream_and_the_consumer_carries_on() {
    let host = start();
    let consumer = host
        .stream_attach(1, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    assert_eq!(consumer.state(), STATE_OPEN);
    host.off("e").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.stream_stats(1, "frames").unwrap().claimed && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = host.stream_stats(1, "frames").unwrap();
    assert!(!stats.claimed);
    assert!(stats.attached);
    assert_eq!(consumer.state(), STATE_OPEN);
}

#[test]
fn shutting_the_host_down_closes_the_ring() {
    let host = start();
    let consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    host.shutdown();
    assert_eq!(consumer.state(), STATE_CLOSED);
}
