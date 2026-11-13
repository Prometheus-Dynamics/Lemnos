//! A consumer that lets go of a stream without closing it, or goes away with it open. The daemon
//! must release the stream, and the next open must succeed within a deadline. It must never hang.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Running, StandIn, start_with, start_with_buffers};
use lemnos_client::{Error, FrameStream, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;

const DEVICE: u32 = 0;
const NAME: &str = "frames";
const BOARD: &str = "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
                     [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\n";
const DMA_BOARD: &str = "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
                         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\n\
                         memory = \"dma-buf\"\nheap = \"system\"\n";

/// How long an open may take before the test calls it a hang. The client's own deadline is 5 s.
const DEADLINE: Duration = Duration::from_secs(2);

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn attached(running: &Running) -> bool {
    running
        .service
        .as_ref()
        .unwrap()
        .host()
        .stream_stats(DEVICE, NAME)
        .unwrap()
        .attached
}

/// Opens the stream, and checks the open answered within the deadline.
fn open_in_time(session: &mut Session) -> Result<FrameStream, Error> {
    let started = Instant::now();
    let opened = session.open_stream(DEVICE, NAME, platform());
    let took = started.elapsed();
    assert!(took < DEADLINE, "the open took {took:?}");
    opened
}

#[test]
fn dropping_a_stream_without_closing_lets_the_next_open_succeed() {
    let running = start_with(BOARD);
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let stream = session.open_stream(DEVICE, NAME, platform()).unwrap();
    drop(stream);

    let again =
        open_in_time(&mut session).unwrap_or_else(|error| panic!("second open refused: {error:?}"));
    assert!(attached(&running), "the second open holds the stream");
    session.close_stream(again).unwrap();
}

#[test]
fn a_dropped_stream_is_closed_by_the_next_call_on_its_session() {
    let running = start_with(BOARD);
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    drop(session.open_stream(DEVICE, NAME, platform()).unwrap());
    assert!(attached(&running));
    session.list().unwrap();
    let deadline = Instant::now() + DEADLINE;
    while attached(&running) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!attached(&running), "the close never reached the service");
}

#[test]
fn a_close_owed_for_a_dropped_stream_never_closes_the_stream_opened_after_it() {
    let running = start_with(BOARD);
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    drop(session.open_stream(DEVICE, NAME, platform()).unwrap());
    // The owed close goes out before this open, so the new stream stays open.
    let second = open_in_time(&mut session).unwrap();
    session.list().unwrap();
    assert!(
        attached(&running),
        "the second stream was closed by the first's drop"
    );
    session.close_stream(second).unwrap();
}

#[test]
fn a_dropped_dma_buf_stream_is_closed_and_the_next_open_succeeds() {
    let running = start_with_buffers(DMA_BOARD, Arc::new(StandIn::default()));
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    drop(session.open_stream(DEVICE, NAME, platform()).unwrap());
    let again = open_in_time(&mut session)
        .unwrap_or_else(|error| panic!("second open of a dma-buf stream refused: {error:?}"));
    session.close_stream(again).unwrap();
}

#[test]
fn a_client_killed_with_a_stream_open_frees_it_for_the_next_client() {
    let running = start_with(BOARD);
    let mut first = Session::connect(&running.socket, "first").unwrap();
    let stream = first.open_stream(DEVICE, NAME, platform()).unwrap();
    // The process goes: its stream and its connection with it, no close sent.
    std::mem::forget(stream);
    drop(first);

    let mut second = Session::connect(&running.socket, "second").unwrap();
    let opened = open_in_time(&mut second)
        .unwrap_or_else(|error| panic!("open after a killed client refused: {error:?}"));
    second.close_stream(opened).unwrap();
}

#[test]
fn a_stream_dropped_by_one_client_is_free_for_another_once_its_connection_ends() {
    let running = start_with(BOARD);
    let mut first = Session::connect(&running.socket, "first").unwrap();
    drop(first.open_stream(DEVICE, NAME, platform()).unwrap());
    drop(first);

    let mut second = Session::connect(&running.socket, "second").unwrap();
    let opened = open_in_time(&mut second).unwrap_or_else(|error| panic!("refused: {error:?}"));
    second.close_stream(opened).unwrap();
}

#[test]
fn a_second_open_while_a_consumer_still_holds_the_stream_is_refused_with_busy() {
    let running = start_with(BOARD);
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let held = session.open_stream(DEVICE, NAME, platform()).unwrap();
    let mut other = Session::connect(&running.socket, "other").unwrap();
    let error = open_in_time(&mut other).unwrap_err();
    assert!(
        matches!(&error, Error::Refused { status, .. } if *status == Status::BUSY),
        "{error:?}"
    );
    session.close_stream(held).unwrap();
}
