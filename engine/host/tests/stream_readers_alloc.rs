//! The frame path with a reader attached: publish, the consumer's take-back, the reader's pop, read,
//! validate and release, and the host's tick make no system call and allocate nothing once running. A
//! reader arriving and leaving does not disturb the producer or the consumer.
//!
//! The steady state runs in a child with a seccomp filter that allows only `exit`
//! (`shm_measure::sched::without_system_calls`), and counts its allocations there. The child is a fork of
//! this test, which has no other thread, so the frame path can use its own memory safely.

use std::sync::Arc;

use lemnos_core::CallerId;
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimFrameSource};
use lemnos_wire::ring::{Consumer, TicketReader};
use shm_measure::sched::{SystemCalls, without_system_calls};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const CONSUMER: CallerId = CallerId(3);
const SLOTS: u32 = 2;

fn start() -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = 8\nbuffer_bytes = 64\nreaders = {SLOTS}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

/// The stream's producer and consumer, and whichever reader is attached.
struct Path {
    host: Host,
    producer: ProducerEndpoint,
    consumer: Consumer,
    writer: lemnos_host::stream::BufferWriter,
    reader: Option<TicketReader>,
    source: SimFrameSource,
    /// The sequence of the next frame to publish.
    next: u64,
}

impl Path {
    fn new() -> Self {
        let host = start();
        let producer = host.stream_producer(0, "frames").unwrap();
        let writer = producer.writer();
        let consumer = host
            .stream_attach(0, "frames", CONSUMER)
            .unwrap()
            .consumer()
            .unwrap();
        let mut path = Self {
            host,
            producer,
            consumer,
            writer,
            reader: None,
            source: SimFrameSource::new(64),
            next: 0,
        };
        path.reader = Some(path.arrive());
        path
    }

    /// A reader asks the consumer's stream for a slot and attaches to what it is granted.
    fn arrive(&self) -> TicketReader {
        let ticket = self
            .host
            .stream_ticket(0, "frames", CONSUMER, 2, 0, false)
            .expect("a reader slot");
        TicketReader::attach(
            ticket.descriptors,
            ticket.layout,
            ticket.planes.lengths(),
            true,
            ticket.slot,
            ticket.held,
        )
        .expect("the reader attaches to its ticket")
    }

    /// One frame through the whole path: published, taken and released by the consumer, and, if a
    /// reader is attached, read, validated and released by it. `false` if anything was not as it should.
    /// It does not panic, since a panic is a system call in a child under the filter.
    fn step(&mut self) -> bool {
        let sequence = self.next;
        let mut write = |buffer, bytes: &[u8]| self.writer.write(buffer, 0, bytes);
        if !matches!(
            self.source.produce(&mut self.producer, &mut write),
            Ok(true)
        ) {
            return false;
        }
        let Ok(Some(got)) = self.consumer.pop_frame() else {
            return false;
        };
        if got.sequence != sequence || !self.consumer.release(got.buffer) {
            return false;
        }
        if let Some(reader) = self.reader.as_mut() {
            // A reader that has just arrived starts at the oldest frame the ring still holds: it
            // releases those first, and the current frame is the one it must read and validate.
            // A new reader's first pop can report the frames the ring lapped as nothing, and the
            // current frame on the next call: a few empty pops are allowed, no more.
            let mut empty = 0;
            loop {
                let frame = match reader.pop() {
                    Ok(Some(frame)) => frame,
                    Ok(None) if empty < 8 => {
                        empty += 1;
                        continue;
                    }
                    _ => return false,
                };
                if frame.sequence < sequence {
                    if !reader.release(frame.buffer) {
                        return false;
                    }
                    continue;
                }
                let mut bytes = [0_u8; 16];
                if frame.sequence != sequence
                    || !reader.read(frame.buffer, 0, &mut bytes)
                    || reader.validate(frame.buffer).is_err()
                    || !reader.release(frame.buffer)
                {
                    return false;
                }
                break;
            }
        }
        self.next += 1;
        true
    }

    /// `frames` steps. The host's tick, when `tick` is set, runs every 32 of them as its loop does.
    fn run(&mut self, frames: u64, tick: bool) -> bool {
        (0..frames).all(|index| {
            if tick && index % 32 == 0 {
                self.host.streams_tick();
            }
            self.step()
        })
    }
}

#[test]
fn the_frame_path_with_a_reader_attached_makes_no_system_call_and_allocates_nothing() {
    let mut path = Path::new();
    assert!(path.run(200, true), "warm-up frames went through");
    let ended = without_system_calls(|| {
        let (ran, count) = allocations(|| path.run(5_000, false));
        ran && count == 0
    });
    assert_eq!(
        ended,
        SystemCalls::None,
        "the steady state, with a reader attached"
    );
}

#[test]
fn a_reader_arriving_and_leaving_does_not_disturb_the_producer_or_the_consumer() {
    let mut path = Path::new();
    assert!(path.run(64, true));
    for round in 0..40 {
        // The reader leaves: half the rounds give its slot back, half simply go (the host finds
        // its liveness pipe hung up on the next tick).
        let reader = path.reader.take().expect("a reader is attached");
        let slot = reader.slot();
        drop(reader);
        if round % 2 == 0 {
            path.host
                .stream_unticket(0, "frames", CONSUMER, slot)
                .expect("the reader's slot is given back");
        }
        assert!(
            path.run(16, true),
            "frames flow with no reader, round {round}"
        );
        path.host.streams_tick();
        path.reader = Some(path.arrive());
        assert!(
            path.run(16, true),
            "frames flow with the reader back, round {round}"
        );
    }
    let stats = path.host.stream_stats(0, "frames").expect("stats");
    assert_eq!(
        stats.faults, 0,
        "the stream is not faulted by readers coming and going"
    );
    assert!(stats.attached, "the consumer is still attached");
    assert_eq!(stats.published, path.next, "every frame was published");
}

#[test]
fn the_host_tick_allocates_nothing_with_readers_attached() {
    // The tick is the daemon loop's pass, not the frame path. It checks each reader's liveness pipe with
    // one non-blocking poll, which is a system call, so it is measured for allocations only.
    let mut path = Path::new();
    assert!(path.run(64, false));
    let ((), count) = allocations(|| {
        for _ in 0..1_000 {
            path.host.streams_tick();
        }
    });
    assert_eq!(count, 0, "allocations in 1000 ticks with a reader attached");
}
