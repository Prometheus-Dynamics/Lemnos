//! A stream of dma-bufs whose buffers are in planes, and a stream on standby, over the real
//! socket. Sealed memfds stand in for the dma-bufs, so nothing here opens a device node.

mod common;

use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use common::{Running, StandIn, start_with_buffers};
use lemnos_client::{FrameStream, Session, StreamPlatform};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::mmap::Mapping;

const DEVICE: u32 = 0;
const PLANAR: &str = "planar";
const STANDBY: &str = "standby";
const BUFFERS: u32 = 3;
/// The planes of a planar buffer, and what they add up to.
const PLANES: [usize; 2] = [3000, 1000];
const BYTES: usize = 4000;

fn board() -> String {
    format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.planar]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n\
         memory = \"dma-buf\"\nplanes = [{}, {}]\n\
         [devices.streams.standby]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n\
         memory = \"dma-buf\"\nstandby = true\n",
        PLANES[0], PLANES[1]
    )
}

fn service() -> (Running, Arc<StandIn>) {
    let source = Arc::new(StandIn::default());
    let running = start_with_buffers(&board(), Arc::clone(&source) as _);
    (running, source)
}

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn host(running: &Running) -> &lemnos_host::Host {
    running.service.as_ref().unwrap().host()
}

fn producer(running: &Running, name: &str) -> ProducerEndpoint {
    host(running).stream_producer(DEVICE, name).unwrap()
}

fn frame(buffer: u32, sequence: u64) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: BYTES as u32,
        sequence,
        identity: sequence,
        timestamp_ns: 0,
    }
}

/// Plane `plane` of `buffer` as hardware given its descriptor reaches it.
fn hardware(
    producer: &ProducerEndpoint,
    buffer: u32,
    plane: u32,
    len: usize,
) -> (OwnedFd, Mapping) {
    let handle = producer
        .plane_handle(buffer, plane)
        .expect("the stream has handles")
        .expect("memory is behind the buffer");
    let fd = handle.fd().try_clone_to_owned().unwrap();
    let map = Mapping::new(fd.as_fd(), len, 0, true).unwrap();
    (fd, map)
}

#[test]
fn a_buffer_in_planes_reaches_the_consumer_a_descriptor_per_plane() {
    let (running, source) = service();
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let mut stream = session.open_stream(DEVICE, PLANAR, platform()).unwrap();
    assert_eq!(stream.planes().lengths(), [3000, 1000]);
    // One request per plane, each for every buffer, at the plane's own length.
    assert_eq!(
        *source.asked.lock().unwrap(),
        [
            ("system".to_string(), 3000, BUFFERS),
            ("system".to_string(), 1000, BUFFERS)
        ]
    );
    let mut producing = producer(&running, PLANAR);
    assert_eq!(producing.planes(), 2);
    assert_eq!(producing.plane_offset(1), Some(3000));
    assert_eq!(producing.plane_offset(2), None);
    for sequence in 0..2 * u64::from(BUFFERS) {
        let buffer = producing.next_buffer().unwrap().unwrap();
        let (first_fd, first) = hardware(&producing, buffer, 0, PLANES[0]);
        let (second_fd, second) = hardware(&producing, buffer, 1, PLANES[1]);
        assert_ne!(
            dma_buf::identity(first_fd.as_fd()).unwrap(),
            dma_buf::identity(second_fd.as_fd()).unwrap(),
            "each plane is its own memory"
        );
        let stamp = sequence as u8;
        first.write_bytes(0, &[stamp; PLANES[0]]).unwrap();
        second.write_bytes(0, &[!stamp; PLANES[1]]).unwrap();
        // Software reaches across the seam, end to end.
        assert_eq!(producing.write(buffer, PLANES[0] - 1, &[1, 2]), Ok(true));
        producing.publish(frame(buffer, sequence)).unwrap();

        let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        let luma = stream.plane(&got, 0);
        assert_eq!(luma.len(), PLANES[0]);
        assert!(luma[..PLANES[0] - 1].iter().all(|b| *b == stamp));
        assert_eq!(luma[PLANES[0] - 1], 1);
        let chroma = stream.plane(&got, 1);
        assert_eq!(chroma.len(), PLANES[1]);
        assert_eq!(chroma[0], 2);
        assert!(chroma[1..].iter().all(|b| *b == !stamp));
        stream.release(got).unwrap();
    }
    session.close_stream(stream).unwrap();
}

/// Fills and publishes `count` frames on `producing`, from `first`.
fn capture(producing: &mut ProducerEndpoint, first: u64, count: u64) {
    for sequence in first..first + count {
        let buffer = producing.next_buffer().unwrap().unwrap();
        let (_fd, map) = hardware(producing, buffer, 0, BYTES);
        map.write_bytes(0, &[sequence as u8; 16]).unwrap();
        producing.publish(frame(buffer, sequence)).unwrap();
    }
}

fn first_frame(stream: &mut FrameStream) -> (u64, u8) {
    let got = stream.try_next().unwrap().expect("a frame waits at once");
    let seen = (got.sequence(), stream.bytes(&got)[0]);
    stream.release(got).unwrap();
    seen
}

#[test]
fn on_standby_capture_runs_with_nobody_attached_and_a_consumer_starts_on_the_newest_frame() {
    let (running, source) = service();
    let mut producing = producer(&running, STANDBY);
    // Nobody attached, and still memory to fill: more frames than buffers.
    capture(&mut producing, 1, 3 * u64::from(BUFFERS));
    let stats = host(&running).stream_stats(DEVICE, STANDBY).unwrap();
    assert!(stats.standby);
    assert_eq!(stats.kept, 1);
    assert_eq!(source.asked.lock().unwrap().len(), 1, "made once");

    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let mut stream = session.open_stream(DEVICE, STANDBY, platform()).unwrap();
    // The consumer took over that memory, and the newest frame is its first, already there.
    assert_eq!(source.asked.lock().unwrap().len(), 1, "nothing made for it");
    let newest = 3 * u64::from(BUFFERS);
    assert_eq!(first_frame(&mut stream), (newest, newest as u8));
    capture(&mut producing, 100, 1);
    let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(got.sequence(), 100);
    stream.release(got).unwrap();

    // It leaves: its memory goes with it, and fresh memory waits for the next stretch.
    session.close_stream(stream).unwrap();
    capture(&mut producing, 200, 2);
    assert_eq!(source.asked.lock().unwrap().len(), 2, "made again");
    let mut stream = session.open_stream(DEVICE, STANDBY, platform()).unwrap();
    assert_eq!(first_frame(&mut stream), (201, 201));
}
