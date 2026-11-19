//! A ticketed reader at its granted hold limit is told so (`Pop::AtLimit`), and one below it gets
//! its frames. The grant's arithmetic is checked on the worked case: a consumer, one reader, and
//! four buffers.

use std::sync::Arc;

use lemnos_core::{CallerId, Status};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimFrameSource};
use lemnos_wire::ring::{Consumer, Pop, TicketReader};

const CONSUMER: CallerId = CallerId(3);

fn start(buffers: u32) -> Host {
    start_with(buffers, None)
}

fn start_with(buffers: u32, reader_held_max: Option<u32>) -> Host {
    let held_max = reader_held_max
        .map(|max| format!("reader_held_max = {max}\n"))
        .unwrap_or_default();
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {buffers}\nbuffer_bytes = 64\nreaders = 2\n{held_max}"
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

fn produce(source: &mut SimFrameSource, producer: &mut ProducerEndpoint) -> bool {
    let writer = producer.writer();
    source
        .produce(producer, &mut |buffer, bytes| {
            writer.write(buffer, 0, bytes)
        })
        .unwrap()
}

/// The consumer takes the frame just published and gives its buffer back.
fn take_back(consumer: &mut Consumer) {
    let frame = consumer
        .pop_entry()
        .unwrap()
        .and_then(|entry| entry.frame())
        .expect("a frame");
    assert!(consumer.release(frame.buffer));
}

fn attach(host: &Host) -> Consumer {
    host.stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap()
}

/// Four buffers and a consumer: the default takes what fits. Two would leave the producer one
/// buffer, so the default is granted one, and the producer keeps two.
#[test]
fn with_four_buffers_and_a_consumer_a_default_request_is_granted_one() {
    let host = start(4);
    let _consumer = attach(&host);
    let granted = host
        .stream_ticket(0, "frames", CONSUMER, 0, 0, false)
        .expect("the default takes the one buffer that fits");
    assert_eq!(granted.held, 1);
}

/// Eight buffers and a consumer: the producer keeps two and the consumer one, so the default's two
/// fit and are granted in full.
#[test]
fn with_eight_buffers_a_default_request_is_granted_two() {
    let host = start(8);
    let _consumer = attach(&host);
    let granted = host
        .stream_ticket(0, "frames", CONSUMER, 0, 0, false)
        .expect("two buffers fit");
    assert_eq!(granted.held, 2);
}

/// Four buffers, a consumer, and a reader already granted one: 4 - 1 - 1 = 2 are left, and the
/// producer keeps both. A default request has no room and is refused by name; the numbers are in
/// the text.
#[test]
fn with_four_buffers_and_a_reader_holding_one_a_default_request_is_refused_by_name() {
    let host = start(4);
    let _consumer = attach(&host);
    let first = host
        .stream_ticket(0, "frames", CONSUMER, 1, 0, false)
        .expect("one buffer is granted");
    assert_eq!(first.held, 1);
    let Err(refused) = host.stream_ticket(0, "frames", CONSUMER, 0, 0, false) else {
        panic!("a default request with no room is refused");
    };
    assert_eq!(refused.status, Status::BUSY);
    assert!(
        refused.text.contains("default of 2")
            && refused.text.contains("2 of 4 are left")
            && refused.text.contains("so none fits"),
        "{}",
        refused.text
    );
}

/// An explicit request is all or refused: two on four buffers leaves the producer one, so it is
/// refused by name, and the text says what was asked and what is left.
#[test]
fn with_four_buffers_an_explicit_request_of_two_is_refused_by_name() {
    let host = start(4);
    let _consumer = attach(&host);
    let Err(refused) = host.stream_ticket(0, "frames", CONSUMER, 2, 0, false) else {
        panic!("an explicit request of two is refused");
    };
    assert_eq!(refused.status, Status::BUSY);
    assert!(
        refused.text.contains("asked for 2") && refused.text.contains("3 of 4 are left"),
        "{}",
        refused.text
    );
}

/// An explicit request of one on four buffers is granted, and leaves the producer two.
#[test]
fn with_four_buffers_an_explicit_request_of_one_is_granted() {
    let host = start(4);
    let _consumer = attach(&host);
    let granted = host
        .stream_ticket(0, "frames", CONSUMER, 1, 0, false)
        .expect("one buffer is granted");
    assert_eq!(granted.held, 1);
}

/// A stream whose `reader_held_max` is one: the default is granted one, the stream's ceiling.
#[test]
fn with_a_held_max_of_one_a_default_request_is_granted_one() {
    let host = start_with(8, Some(1));
    let _consumer = attach(&host);
    let granted = host
        .stream_ticket(0, "frames", CONSUMER, 0, 0, false)
        .expect("the default is clamped to the ceiling");
    assert_eq!(granted.held, 1);
}

/// Two frames are held by a reader granted two. The third frame is skipped and the reader is told
/// it is at its limit, not that nothing came. Giving one back lets it read on, and the frame after
/// the skipped one comes; a reader holding one (below its limit) gets its frames.
#[test]
fn a_reader_at_its_granted_limit_is_told_and_one_below_it_gets_its_frames() {
    let host = start(8);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut source = SimFrameSource::new(64);
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let ticket = host
        .stream_ticket(0, "frames", CONSUMER, 2, 0, false)
        .expect("a reader slot");
    assert_eq!(ticket.held, 2);
    let mut reader = TicketReader::attach(
        ticket.descriptors,
        ticket.layout,
        ticket.planes.lengths(),
        true,
        ticket.slot,
        ticket.held,
    )
    .expect("the reader attaches to its ticket");

    // Frame 0 and frame 1: the reader takes each, one below its limit and then at it.
    let mut first = None;
    for sequence in 0..2 {
        assert!(produce(&mut source, &mut producer));
        take_back(&mut consumer);
        match reader.pop().expect("no stop") {
            Pop::Frame(frame) => {
                assert_eq!(frame.sequence, sequence);
                first.get_or_insert(frame.buffer);
            }
            other => panic!("frame {sequence} is the reader's to hold: {other:?}"),
        }
    }
    let first = first.expect("frame 0's buffer");
    // Frame 2 comes while the reader holds two: skipped, and the reader is told so.
    assert!(produce(&mut source, &mut producer));
    take_back(&mut consumer);
    let at_limit = reader.pop().expect("no stop");
    assert!(
        matches!(at_limit, Pop::AtLimit),
        "a reader at its limit is told, not left with nothing: {at_limit:?}"
    );
    // A held frame given back: nothing is pending, and the next frame comes.
    assert!(reader.release(first));
    assert!(matches!(reader.pop().expect("no stop"), Pop::Empty));
    assert!(produce(&mut source, &mut producer));
    take_back(&mut consumer);
    match reader.pop().expect("no stop") {
        Pop::Frame(frame) => assert_eq!(frame.sequence, 3, "frame 2 was skipped"),
        other => panic!("the reader reads on once it gives one back: {other:?}"),
    }
}
