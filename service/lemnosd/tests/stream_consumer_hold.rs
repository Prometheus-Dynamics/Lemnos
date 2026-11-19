//! A consumer's declared hold through the daemon and the Rust client: a declaration that does not
//! fit is refused by name at open, and a consumer past its declared hold gets `Next::AtLimit`.

mod common;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{Running, start_with};
use lemnos_client::{Error, FrameStream, Next, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_linux_stream::LinuxStreams;
use lemnos_sim::SimFrameSource;

const FRAME_BYTES: usize = 4096;
const DEVICE: u32 = 0;
const NAME: &str = "frames";

fn board(buffers: u32) -> String {
    format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {buffers}\nbuffer_bytes = {FRAME_BYTES}\nreaders = 2\n"
    )
}

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn producer(running: &Running) -> ProducerEndpoint {
    running
        .service
        .as_ref()
        .unwrap()
        .host()
        .stream_producer(DEVICE, NAME)
        .unwrap()
}

fn produce(source: &mut SimFrameSource, producer: &mut ProducerEndpoint) {
    let writer = producer.writer();
    assert!(
        source
            .produce(producer, &mut |buffer, bytes| writer
                .write(buffer, 0, bytes))
            .unwrap()
    );
}

/// The next thing the stream says, within a bound.
fn next(stream: &mut FrameStream) -> Next {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match stream
            .next_or_end(Duration::from_millis(50))
            .expect("no stop")
        {
            Next::TimedOut if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            other => return other,
        }
    }
}

/// Four buffers and a declaration of 3: the producer would keep one, and it needs two. The open is
/// refused at once, by name, and the stream is still free for a consumer that declares nothing.
#[test]
fn a_declared_hold_that_does_not_fit_is_refused_by_name_at_open() {
    let running = start_with(&board(4));
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let Err(error) = session.open_stream_held(DEVICE, NAME, 3, platform()) else {
        panic!("a declaration of 3 on 4 buffers is refused");
    };
    match &error {
        Error::Refused { status, text } => {
            assert_eq!(*status, Status::BUSY, "{error:?}");
            assert!(
                text.contains("declares 3") && text.contains("leave 1"),
                "{text}"
            );
        }
        other => panic!("refused by name, not {other:?}"),
    }
    session
        .open_stream(DEVICE, NAME, platform())
        .expect("nothing was attached by the refused declaration");
}

/// Six buffers, the consumer declares 2: it takes two frames and keeps them, and the third frame
/// comes as `Next::AtLimit`. Giving a frame back lets the next one come.
#[test]
fn a_consumer_past_its_declared_hold_gets_at_limit() {
    let running = start_with(&board(6));
    let mut source = SimFrameSource::new(FRAME_BYTES);
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let mut stream = session
        .open_stream_held(DEVICE, NAME, 2, platform())
        .expect("a declaration of 2 on 6 buffers fits");
    let mut producer = producer(&running);

    let mut held = Vec::new();
    for _ in 0..2 {
        produce(&mut source, &mut producer);
        match next(&mut stream) {
            Next::Frame(frame) => held.push(frame),
            other => panic!("a frame within the declared hold: {other:?}"),
        }
    }
    produce(&mut source, &mut producer);
    assert!(
        matches!(next(&mut stream), Next::AtLimit),
        "past its declared hold the consumer is told it is at its limit"
    );
    drop(held.pop());
    produce(&mut source, &mut producer);
    assert!(
        matches!(next(&mut stream), Next::Frame(_)),
        "once a hold is given back the next frame comes"
    );
}
