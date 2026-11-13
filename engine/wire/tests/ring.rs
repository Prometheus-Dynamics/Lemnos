//! The frame stream's shared memory: what a consumer reads, what it gives back, what it posts, and
//! what the host can refuse. Every guarantee below is one the stream's contract depends on.

#![cfg(feature = "testing")]

use lemnos_wire::ring::{FrameInfo, Layout, RingError, STATE_CLOSED, STATE_FAULT, STATE_OPEN};
use lemnos_wire::testing::Harness;

fn layout(buffers: u32) -> Layout {
    Layout::new(buffers, 4096, 4, 64).expect("layout")
}

#[test]
fn a_layout_outside_the_limits_is_refused() {
    assert!(Layout::new(0, 4096, 4, 64).is_err());
    assert!(Layout::new(1025, 4096, 4, 64).is_err());
    assert!(Layout::new(4, 0, 4, 64).is_err());
    assert!(Layout::new(4, 4096, 4, 0).is_err());
    assert!(Layout::new(4, 4096, 65, 64).is_err());
}

#[test]
fn a_frame_round_trips_through_its_metadata_and_a_short_one_is_refused() {
    let info = FrameInfo {
        sequence: 7,
        identity: 0xfeed,
        bytes: 1234,
    };
    assert_eq!(FrameInfo::decode(&info.encode()), Some(info));
    assert_eq!(FrameInfo::decode(&[0; 19]), None);
}

#[test]
fn a_published_frame_is_read_once_and_its_buffer_is_held_until_released() {
    let (mut host, mut consumer) = Harness::new(layout(2), true, false).expect("stream");
    let buffer = host.take().expect("a free buffer");
    assert!(host.write(buffer, 0, &[9, 8, 7]));
    assert!(host.publish(buffer, 3, 5, 0xabc, 42));

    let frame = consumer.pop_frame().expect("no error").expect("a frame");
    assert_eq!(
        (
            frame.buffer,
            frame.bytes,
            frame.sequence,
            frame.identity,
            frame.timestamp_ns
        ),
        (buffer, 3, 5, 0xabc, 42)
    );
    assert_eq!(consumer.pop_frame().expect("no error"), None);

    // Held: the producer can't take it back while the consumer has it. The other buffer is
    // published too, so it is the newest frame and is left alone as well.
    let other = host.take().expect("the other buffer");
    assert_ne!(other, buffer);
    assert!(host.publish(other, 0, 6, 0, 0));
    assert!(host.take().is_none(), "both buffers are spoken for");

    let mut out = [0_u8; 3];
    assert!(consumer.read(buffer, 0, &mut out));
    assert_eq!(out, [9, 8, 7]);

    assert!(consumer.release(buffer));
    assert!(!consumer.release(buffer), "a frame is released once");
    assert!(host.reclaim() >= 1, "the released buffer comes back");
    assert_eq!(host.take(), Some(buffer), "and is free to fill again");
}

#[test]
fn a_consumer_cannot_give_back_a_buffer_it_never_took() {
    let (_host, mut consumer) = Harness::new(layout(2), true, false).expect("stream");
    assert!(!consumer.release(0));
    assert!(!consumer.release(1));
}

#[test]
fn a_consumer_that_was_not_attached_writable_cannot_write_a_buffer() {
    let (mut host, mut consumer) = Harness::new(layout(2), true, false).expect("stream");
    let buffer = host.take().expect("a free buffer");
    host.publish(buffer, 0, 1, 0, 0);
    assert!(!consumer.write(buffer, 0, &[1]));
}

#[test]
fn a_writable_consumer_fills_a_buffer_the_producer_then_sees() {
    let (mut host, mut consumer) = Harness::new(layout(2), true, true).expect("stream");
    let buffer = host.take().expect("a free buffer");
    host.publish(buffer, 0, 1, 0, 0);
    assert!(consumer.write(buffer, 4, b"job"));
    let mut seen = [0_u8; 3];
    let frame = consumer.pop_frame().expect("no error").expect("frame");
    assert_eq!(frame.buffer, buffer);
    assert!(consumer.read(buffer, 4, &mut seen));
    assert_eq!(&seen, b"job");
}

#[test]
fn a_job_posted_by_the_consumer_arrives_in_order_and_the_ring_refuses_more_than_it_holds() {
    let (mut host, mut consumer) = Harness::new(layout(2), true, false).expect("stream");
    let mut out = [0_u8; 64];
    for n in 0..4_u8 {
        assert!(consumer.jobs().post(&[n; 8]).expect("post"), "job {n} fits");
    }
    assert!(
        !consumer.jobs().post(&[9; 8]).expect("post"),
        "a fifth job waits for the host to collect one"
    );
    for n in 0..4_u8 {
        let len = host.pop_job(&mut out).expect("no error").expect("a job");
        assert_eq!(&out[..len], &[n; 8]);
    }
    assert_eq!(host.pop_job(&mut out).expect("no error"), None);
    assert!(
        consumer.jobs().post(&[9; 8]).expect("post"),
        "once the host collected, there is room again"
    );
}

#[test]
fn a_job_longer_than_the_layout_allows_is_refused_before_anything_is_posted() {
    let (_host, mut consumer) = Harness::new(layout(2), true, false).expect("stream");
    assert!(matches!(
        consumer.jobs().post(&[0; 65]),
        Err(RingError::JobTooLarge)
    ));
}

#[test]
fn the_end_word_tells_a_consumer_the_stream_ended_and_why() {
    let (mut host, consumer) = Harness::new(layout(2), true, false).expect("stream");
    assert_eq!(consumer.state(), STATE_OPEN);
    host.end(STATE_FAULT);
    assert_eq!(consumer.state(), STATE_FAULT);
    let (mut other, consumer) = Harness::new(layout(2), true, false).expect("stream");
    other.end(STATE_CLOSED);
    assert_eq!(consumer.state(), STATE_CLOSED);
}

#[test]
fn a_dma_buf_style_stream_maps_one_descriptor_per_buffer() {
    let (mut host, mut consumer) = Harness::new(layout(3), false, true).expect("stream");
    let buffer = host.take().expect("free");
    assert!(consumer.write(buffer, 0, &[5; 16]));
    host.publish(buffer, 16, 1, 0, 0);
    let frame = consumer.pop_frame().expect("no error").expect("frame");
    assert_eq!(frame.bytes, 16);
}
