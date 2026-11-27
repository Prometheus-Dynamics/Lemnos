//! A consumer's declared hold: the ticket math reserves exactly that many buffers for the consumer,
//! and its own holds are bounded by it. With no declaration the numbers are the ones the default
//! always had (`HOLD-DEFAULT`).

mod test_options;

use std::sync::Arc;

use lemnos_core::{CallerId, Status};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_host::{Host, entry};
use lemnos_sim::{SimBoard, SimFrameSource};
use lemnos_wire::ring::Consumer;

const CONSUMER: CallerId = CallerId(3);

fn start(buffers: u32, readers: u32) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {buffers}\nbuffer_bytes = 64\nreaders = {readers}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        test_options::options(),
    )
    .unwrap()
}

/// Attaches the consumer with a declared hold, and returns its wire end, bounded by that hold.
fn declared(host: &Host, hold: u32) -> Consumer {
    host.stream_attach_declared(0, "frames", CONSUMER, hold)
        .unwrap_or_else(|refused| panic!("{}", refused.text))
        .consumer_declared(hold)
        .unwrap()
}

/// Publishes one frame.
fn produce(source: &mut SimFrameSource, producer: &mut ProducerEndpoint) {
    let writer = producer.writer();
    assert!(
        source
            .produce(producer, &mut |buffer, bytes| writer
                .write(buffer, 0, bytes))
            .unwrap()
    );
}

/// The consumer takes the frame just published, keeping its buffer held.
fn take(consumer: &mut Consumer) -> Option<u32> {
    consumer
        .pop_entry()
        .unwrap()
        .and_then(|entry| entry.frame())
        .map(|frame| frame.buffer)
}

/// B = 6, the consumer declares 3: the producer keeps 2, so 1 is left for the outside readers. The
/// first outside reader's default request is granted that one, and the next is refused by name.
#[test]
fn b6_declared_3_first_outside_reader_gets_1_and_the_next_is_refused_busy() {
    let host = start(6, 3);
    let _consumer = declared(&host, 3);
    let first = host
        .stream_ticket(0, "frames", CONSUMER, 0, 0, false)
        .expect("the one buffer left is granted");
    assert_eq!(first.held, 1);
    let Err(refused) = host.stream_ticket(0, "frames", CONSUMER, 0, 0, false) else {
        panic!("the next reader is refused: nothing is left for it");
    };
    assert_eq!(refused.status, Status::BUSY);
    assert!(refused.text.contains("so none fits"), "{}", refused.text);
}

/// B = 4, the consumer declares 2: 2 are left, and the producer keeps both. Any other reader is
/// refused, a default request and an explicit one alike.
#[test]
fn b4_declared_2_any_other_reader_is_refused() {
    let host = start(4, 3);
    let _consumer = declared(&host, 2);
    let Err(default) = host.stream_ticket(0, "frames", CONSUMER, 0, 0, false) else {
        panic!("a default reader is refused");
    };
    assert_eq!(default.status, Status::BUSY);
    let Err(explicit) = host.stream_ticket(0, "frames", CONSUMER, 1, 0, false) else {
        panic!("an explicit reader is refused");
    };
    assert_eq!(explicit.status, Status::BUSY);
}

/// B = 8, the consumer declares 4: 4 are left, and a default reader gets the two it asks for.
#[test]
fn b8_declared_4_a_default_reader_gets_2() {
    let host = start(8, 3);
    let _consumer = declared(&host, 4);
    let granted = host
        .stream_ticket(0, "frames", CONSUMER, 0, 0, false)
        .expect("two buffers fit");
    assert_eq!(granted.held, 2);
}

/// B = 4, the consumer declares 3: that leaves the producer one, and it needs two. The declaration
/// is refused at attach, by name, and nothing is attached: a plain attach still succeeds.
#[test]
fn a_declaration_that_does_not_fit_is_refused_at_attach_by_name() {
    let host = start(4, 3);
    let Err(refused) = host.stream_attach_declared(0, "frames", CONSUMER, 3) else {
        panic!("a declaration of 3 on 4 buffers is refused");
    };
    assert_eq!(refused.status, Status::BUSY);
    assert!(
        refused.text.contains("declares 3") && refused.text.contains("leave 1"),
        "{}",
        refused.text
    );
    host.stream_attach(0, "frames", CONSUMER)
        .expect("the refused declaration attached nothing");
}

/// A declaration of no buffers is refused: a consumer that holds nothing is not a declaration.
#[test]
fn a_declaration_of_zero_is_refused() {
    let host = start(4, 3);
    let Err(refused) = host.stream_attach_declared(0, "frames", CONSUMER, 0) else {
        panic!("a declaration of 0 is refused");
    };
    assert_eq!(refused.status, Status::INVALID_ARGUMENT);
}

/// The consumer declared 2 on six buffers: it takes two frames and keeps them. A frame that comes
/// while both are held is not taken (it is skipped, as a reader's is), and the next one is taken
/// once a hold is given back.
#[test]
fn a_consumer_past_its_declared_hold_is_not_given_the_frame() {
    let host = start(6, 3);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut source = SimFrameSource::new(64);
    let mut consumer = declared(&host, 2);

    produce(&mut source, &mut producer);
    let first = take(&mut consumer).expect("the first frame");
    produce(&mut source, &mut producer);
    let _second = take(&mut consumer).expect("the second frame");
    produce(&mut source, &mut producer);
    assert_eq!(
        take(&mut consumer),
        None,
        "past its declared hold the consumer is not given the third"
    );
    assert!(consumer.release(first));
    // The frame that came at the limit was skipped, as a reader's is: the next one is the first
    // the consumer takes once a hold is given back.
    produce(&mut source, &mut producer);
    assert!(
        take(&mut consumer).is_some(),
        "once a hold is given back the next frame comes"
    );
}

/// With no declaration the consumer is not bounded, as it has always been: it takes three frames
/// without giving one back. The default reader on six buffers is granted two.
#[test]
fn with_no_declaration_the_numbers_are_todays() {
    let host = start(6, 3);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut source = SimFrameSource::new(64);
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    for _ in 0..3 {
        produce(&mut source, &mut producer);
        assert!(take(&mut consumer).is_some(), "no declaration, no bound");
    }
    let granted = host
        .stream_ticket(0, "frames", CONSUMER, 0, 0, false)
        .expect("the default reader");
    assert_eq!(granted.held, 2);
}
