//! Frames through the C surface allocate nothing once running: the surface's
//! next frame, its copy out of a buffer, its write into one, and its release,
//! with the service and the producer in the same process and counted too.
//! Sealed memfds stand in for the dma-bufs.
//!
//! The static build only: with `shared` the surface is a library with its own
//! allocator, which this process's counting allocator never sees.

#![cfg(not(feature = "shared"))]

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FRAME_BYTES, INPUT_BYTES, fill, frame_is_right, stream_service};
use lemnos_capi::Client;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_measure::test_alloc::{Counting, TOTAL};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn frames_through_the_c_surface_allocate_nothing_anywhere_in_the_process() {
    let running = stream_service();
    let client = Client::connect(
        running.socket.to_str().unwrap(),
        "t",
        Duration::from_secs(2),
    )
    .unwrap();
    let mut frames = client.open_stream(0, "frames", false).unwrap();
    let mut input = client.open_stream(0, "input", true).unwrap();
    let host = running.service().host();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut pattern = vec![0u8; FRAME_BYTES];
    let image = [0x5Au8; INPUT_BYTES];
    let mut copy = [0u8; 64];
    let mut frame = |sequence: u64| {
        let buffer = producer.next_buffer().unwrap().expect("a buffer back");
        fill(sequence, &mut pattern);
        assert_eq!(producer.write(buffer, 0, &pattern), Ok(true));
        producer
            .publish(StreamFrame {
                buffer,
                bytes: FRAME_BYTES as u32,
                sequence,
                identity: 0,
                timestamp_ns: 0,
            })
            .unwrap();
        let got = frames
            .next_frame(Duration::from_secs(5))
            .unwrap()
            .expect("the frame");
        assert_eq!((got.buffer, got.sequence), (buffer, sequence));
        assert!(frame_is_right(got.bytes, sequence));
        frames.read(buffer, 1000, &mut copy).unwrap();
        assert_eq!(copy[0], sequence.wrapping_mul(7).wrapping_add(1000) as u8);
        frames.release(buffer).unwrap();
        input.write((sequence % 2) as u32, 0, &image).unwrap();
        // No job waits, so a look comes back empty.
        assert!(input.next_frame(Duration::ZERO).unwrap().is_none());
    };
    // Threads start and settle.
    for sequence in 0..300 {
        frame(sequence);
    }
    std::thread::sleep(Duration::from_millis(300));
    for sequence in 300..400 {
        frame(sequence);
    }

    let before = TOTAL.load(Ordering::Relaxed);
    for sequence in 400..1400 {
        frame(sequence);
    }
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "allocations in 1000 frames");
}
