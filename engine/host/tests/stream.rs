//! Frame streams through the host: a simulated producer and a consumer on the real ring.

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
use lemnos_host::stream::{Lease, ProducerEndpoint};
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimFrameSource};
use lemnos_wire::ring::{Consumer, FRAME_WORDS, Layout, STATE_CLOSED, STATE_FAULT, offsets};

const CONSUMER: CallerId = CallerId(7);
const FRAME_BYTES: usize = 64;

#[contract("example.emitter", version = 1)]
pub trait Emitting {
    #[method(1)]
    fn emit(&mut self) -> Result<u32>;
}

#[driver("emitter", provides = [Emitting])]
pub struct Emitter {
    #[needs]
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

fn consumer(lease: &Lease) -> Consumer {
    Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap()
}

fn set(lease: &Lease, at: usize, value: u64) {
    lease.ring.words()[at].store(value, Ordering::SeqCst);
}

fn get(lease: &Lease, at: usize) -> u64 {
    lease.ring.words()[at].load(Ordering::SeqCst)
}

fn returns_at(layout: &Layout) -> usize {
    offsets::FRAMES + layout.frame_slots as usize * FRAME_WORDS
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
fn assert_cut_off(host: &Host, lease: &Lease) {
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.faults, 1, "{stats:?}");
    assert!(!stats.attached);
    assert_eq!(get(lease, offsets::STATE), STATE_FAULT);
    assert_eq!(stats.consumer_held, 0, "the buffers came back");
    let other = host.stream_stats(0, "other").unwrap();
    assert_eq!(other.faults, 0);
}

#[test]
fn frames_arrive_in_order_with_contents_and_identities_over_many_cycles() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
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
        let offset = lease.layout.buffer_offset(frame.buffer).unwrap();
        assert!(lease.buffers.read(offset, &mut read));
        SimFrameSource::fill(sequence, &mut expected);
        assert_eq!(read, expected, "frame {sequence}");
        assert!(consumer.release(frame.buffer, frame.fence).unwrap());
    }
    assert_eq!(consumer.pop_frame().unwrap(), None);
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.published, 1000);
    assert_eq!(stats.delivered, 1000);
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.attachments, 1);
}

#[test]
fn a_consumer_that_holds_every_buffer_stops_the_producer_without_a_fault() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..4 {
        assert!(produce(&mut source, &mut producer));
    }
    assert!(!produce(&mut source, &mut producer), "no buffer free");
    let first = consumer.pop_frame().unwrap().unwrap();
    assert!(!produce(&mut source, &mut producer), "not released yet");
    consumer.release(first.buffer, first.fence).unwrap();
    assert!(produce(&mut source, &mut producer), "released");
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.faults, 0);
    assert_eq!(stats.returned, 1);
    assert_eq!(stats.free + stats.producer_held + stats.consumer_held, 4);
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
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
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
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
    let sent: [u8; FRAME_BYTES] = std::array::from_fn(|i| i as u8 ^ 0x5a);
    let buffer = write_and_publish(&mut producer, &sent);
    let frame = consumer.pop_frame().unwrap().unwrap();
    assert_eq!(frame.buffer, buffer);
    let mut read = [0u8; FRAME_BYTES];
    let offset = lease.layout.buffer_offset(frame.buffer).unwrap();
    assert!(lease.buffers.read(offset, &mut read));
    assert_eq!(read, sent);
    assert_eq!(
        StreamEndpoint::write(&mut producer, buffer, 0, &[1]),
        Err(ErrorKind::InvalidInput),
        "published, so no longer the producer's"
    );
}

#[test]
fn jobs_reach_the_producer_with_their_bytes() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
    assert!(consumer.push_job(&[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap());
    let mut out = [0u8; 16];
    assert_eq!(producer.next_job(&mut out), Ok(Some(9)));
    assert_eq!(&out[..9], [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(producer.next_job(&mut out), Ok(None));
}

#[test]
fn a_job_too_large_for_the_driver_buffer_is_the_drivers_problem() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
    consumer.push_job(&[1, 2, 3]).unwrap();
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
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..3 {
        assert!(produce(&mut source, &mut producer));
    }
    let _held = consumer.pop_frame().unwrap().unwrap();
    assert_eq!(host.stream_stats(0, "frames").unwrap().consumer_held, 3);

    host.caller_gone(CONSUMER);

    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(!stats.attached);
    assert_eq!((stats.free, stats.consumer_held, stats.faults), (4, 0, 0));
    assert_eq!(get(&lease, offsets::STATE), STATE_CLOSED);
    // Someone else can have it now, on memory the first consumer can't see.
    let next = host.stream_attach(0, "frames", CallerId(8)).unwrap();
    assert!(!Arc::ptr_eq(&next.ring, &lease.ring));
    set(&lease, offsets::STATE, 0xbad);
    assert_eq!(get(&next, offsets::STATE), 0);
}

#[test]
fn another_callers_going_changes_nothing() {
    let host = start();
    let _lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    host.caller_gone(CallerId(99));
    assert!(host.stream_stats(0, "frames").unwrap().attached);
}

/// Runs `craft` with a frame out and the producer holding buffer `held`, then lets the producer
/// look.
fn fault_case(craft: impl FnOnce(&Lease, u32, u32)) -> (Host, Lease, ProducerEndpoint) {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
    let mut source = SimFrameSource::new(FRAME_BYTES);
    assert!(produce(&mut source, &mut producer));
    let frame = consumer.pop_frame().unwrap().unwrap();
    let held = producer.next_buffer().unwrap().unwrap();
    craft(&lease, frame.buffer, frame.fence);
    let _ = held;
    (host, lease, producer)
}

fn return_word(lease: &Lease, buffer: u32, fence: u32) {
    let layout = lease.layout;
    set(
        lease,
        returns_at(&layout),
        u64::from(buffer) | (u64::from(fence) << 32),
    );
    set(lease, offsets::RETURNS_HEAD, 1);
}

#[test]
fn a_buffer_returned_twice_loses_the_consumer_its_stream() {
    let (host, lease, mut producer) = fault_case(|lease, buffer, fence| {
        return_word(lease, buffer, fence);
        // The same word again, as a second entry.
        let layout = lease.layout;
        set(
            lease,
            returns_at(&layout) + 1,
            u64::from(buffer) | (u64::from(fence) << 32),
        );
        set(lease, offsets::RETURNS_HEAD, 2);
    });
    let _ = producer.next_buffer().unwrap();
    assert_cut_off(&host, &lease);
    assert_eq!(host.stream_stats(0, "frames").unwrap().returned, 1);
}

#[test]
fn a_returned_number_outside_the_pool_is_a_fault() {
    let (host, lease, mut producer) = fault_case(|lease, _, fence| return_word(lease, 99, fence));
    let _ = producer.next_buffer().unwrap();
    assert_cut_off(&host, &lease);
}

#[test]
fn a_return_with_the_wrong_fence_is_a_fault() {
    let (host, lease, mut producer) = fault_case(|lease, buffer, fence| {
        return_word(lease, buffer, fence.wrapping_add(1));
    });
    let _ = producer.next_buffer().unwrap();
    assert_cut_off(&host, &lease);
}

#[test]
fn returning_a_buffer_the_producer_holds_is_a_fault() {
    let (host, lease, mut producer) = fault_case(|lease, buffer, fence| {
        // A buffer other than the one out: the producer holds it, or it's free.
        return_word(lease, buffer ^ 1, fence);
    });
    let _ = producer.next_buffer().unwrap();
    assert_cut_off(&host, &lease);
}

#[test]
fn an_impossible_returns_head_is_a_fault_and_never_hangs() {
    let (host, lease, mut producer) = fault_case(|lease, _, _| {
        set(lease, offsets::RETURNS_HEAD, u64::MAX);
    });
    let _ = producer.next_buffer().unwrap();
    assert_cut_off(&host, &lease);
}

#[test]
fn an_impossible_frames_tail_is_a_fault_when_publishing() {
    let (host, lease, mut producer) = fault_case(|lease, _, _| {
        set(lease, offsets::FRAMES_TAIL, 1 << 50);
    });
    let buffer = producer.next_buffer().unwrap().unwrap();
    producer
        .publish(StreamFrame {
            buffer,
            bytes: 8,
            sequence: 1,
            identity: 2,
            timestamp_ns: 3,
        })
        .unwrap();
    assert_cut_off(&host, &lease);
}

#[test]
fn an_impossible_job_is_a_fault_and_reads_as_no_job() {
    let (host, lease, mut producer) = fault_case(|lease, _, _| {
        let layout = lease.layout;
        let jobs_at = returns_at(&layout) + layout.frame_slots as usize;
        set(lease, jobs_at, 1 << 40);
        set(lease, offsets::JOBS_HEAD, 1);
    });
    let mut out = [0u8; 64];
    assert_eq!(producer.next_job(&mut out), Ok(None));
    assert_cut_off(&host, &lease);
}

#[test]
fn after_a_fault_the_producer_carries_on_and_a_new_consumer_can_attach() {
    let (host, lease, mut producer) = fault_case(|lease, _, _| {
        set(lease, offsets::RETURNS_HEAD, u64::MAX);
    });
    let mut source = SimFrameSource::new(FRAME_BYTES);
    assert!(producer.next_buffer().unwrap().is_some());
    assert_cut_off(&host, &lease);
    assert!(produce(&mut source, &mut producer), "keeps producing");

    let next = host.stream_attach(0, "frames", CallerId(8)).unwrap();
    let mut fresh = consumer(&next);
    assert!(produce(&mut source, &mut producer));
    assert!(fresh.pop_frame().unwrap().is_some());
}

#[test]
fn a_driver_that_needs_a_stream_gets_this_one_and_its_frames_arrive() {
    let host = start();
    let lease = host.stream_attach(1, "frames", CONSUMER).unwrap();
    let mut consumer = consumer(&lease);
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
    let lease = host.stream_attach(1, "frames", CONSUMER).unwrap();
    assert_eq!(get(&lease, offsets::STATE), 0);
    host.off("e").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.stream_stats(1, "frames").unwrap().claimed && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = host.stream_stats(1, "frames").unwrap();
    assert!(!stats.claimed);
    assert!(stats.attached);
    assert_eq!(get(&lease, offsets::STATE), 0);
}

#[test]
fn shutting_the_host_down_closes_the_ring() {
    let host = start();
    let lease = host.stream_attach(0, "frames", CONSUMER).unwrap();
    host.shutdown();
    assert_eq!(get(&lease, offsets::STATE), STATE_CLOSED);
}
