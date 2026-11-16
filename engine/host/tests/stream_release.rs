//! When a stream's memory is released. With standby off (the default) the last consumer's
//! detach lets go of every descriptor the host holds for the attachment, the producer's own
//! borrowed handles included, and the next attach makes fresh memory. With `standby = true` the
//! memory is kept for a reattach, and the kept frame comes at once.
//!
//! What the stand-in does not share with the real thing: the buffers are sealed memfds, not
//! dma-bufs from a DMA heap. Memfds are measured here through this process's own descriptors
//! (`/proc/self/fd`: the count of memfd descriptors and their logical size), so a descriptor the
//! host or the producer keeps shows up as a number that does not return to its before-value. The
//! kernel's page residency and the heap's accounting are not measured, and no real device is
//! touched. The dma-buf case is the one that failed before this change: the producer end kept
//! its borrowed handles after the consumer left (see `released_when_the_last_consumer_detaches`).

#![cfg(unix)]

use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex, MutexGuard};

use lemnos_core::{CallerId, Status};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::{BufferHandle, StreamFrame};
use lemnos_host::board::parse;
use lemnos_host::stream::{Buffers, StreamAllocator};
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::SimBoard;
use lemnos_wire::Planes;
use lemnos_wire::ring::Layout;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const FIRST: CallerId = CallerId(7);
const SECOND: CallerId = CallerId(8);
const BUFFERS: u32 = 4;
const BYTES: usize = 64;
const DMA_BUF: &str = "memory = \"dma-buf\"\n";
const STANDBY: &str = "memory = \"dma-buf\"\nstandby = true\n";

/// The tests in this file share this process's descriptors, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Every memfd this process holds, by count and by logical size.
fn held_memfds() -> (usize, u64) {
    let mut count = 0;
    let mut bytes = 0;
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        let path = entry.unwrap().path();
        let Ok(target) = std::fs::read_link(&path) else {
            continue;
        };
        if !target.to_string_lossy().starts_with("/memfd:") {
            continue;
        }
        count += 1;
        bytes += std::fs::metadata(&path).map_or(0, |meta| meta.len());
    }
    (count, bytes)
}

/// Makes every stream's buffers memfds standing in for dma-bufs, one per plane of each buffer.
struct Standins;

impl StreamAllocator for Standins {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        lemnos_host::stream::MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        _: &str,
        layout: &Layout,
        _: &str,
        planes: &Planes,
    ) -> Result<Buffers, Status> {
        let fds: Vec<OwnedFd> = (0..layout.buffers)
            .flat_map(|_| planes.lengths().iter().map(|len| unnamed(u64::from(*len))))
            .collect();
        Ok(Buffers {
            fds,
            dma: true,
            sync: None,
        })
    }
}

fn unnamed(len: u64) -> OwnedFd {
    shm_ring::sealed_memfd(len as usize).unwrap()
}

/// A host whose `frames` stream has `keys` added in the board file.
fn start(keys: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n{keys}"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        Arc::new(Standins),
    )
    .unwrap()
}

fn frame(buffer: u32, sequence: u64) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: BYTES as u32,
        sequence,
        identity: 9,
        timestamp_ns: 0,
        end_ns: 0,
    }
}

/// Hardware's view of a buffer's dma-buf, through a descriptor of its own.
fn hardware(handle: BufferHandle<'_>) -> File {
    handle.fd().try_clone_to_owned().unwrap().into()
}

#[test]
fn released_when_the_last_consumer_detaches() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    // The consumer's attach makes the memory; the producer then borrows a buffer's handle.
    let consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let during = held_memfds();
    assert!(
        during.0 > before.0,
        "the stand-in memory is visible: {during:?} vs {before:?}"
    );
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert!(producer.buffer_handle(buffer).unwrap().is_some());
    drop(consumer);
    host.stream_detach(0, "frames", FIRST).unwrap();
    // The producer's next call is what lets go of the copy it borrowed handles from: a device
    // calls once per frame, so nothing waits longer than a frame.
    let held = held_memfds();
    assert_eq!(
        held,
        before,
        "producer end of stream frames still holds {} dma-bufs (lent) after the last detach",
        held.0.saturating_sub(before.0)
    );
    // The producer's own buffers are still its to hold.
    assert_eq!(producer.buffer_current(buffer), Ok(false));
}

#[test]
fn released_memory_comes_back_to_its_before_value_after_the_last_detach() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    for round in 0..3 {
        let consumer = host
            .stream_attach(0, "frames", FIRST)
            .unwrap()
            .consumer()
            .unwrap();
        let buffer = producer.next_buffer().unwrap().unwrap();
        let into = hardware(producer.buffer_handle(buffer).unwrap().unwrap());
        into.write_all_at(&[round as u8; 4], 0).unwrap();
        producer.publish(frame(buffer, round)).unwrap();
        drop(consumer);
        host.stream_detach(0, "frames", FIRST).unwrap();
        drop(into);
        let held = held_memfds();
        assert_eq!(
            held,
            before,
            "round {round}: producer end of stream frames still holds {} dma-bufs (lent) after the last detach",
            held.0.saturating_sub(before.0)
        );
    }
}

#[test]
fn a_reattach_after_release_makes_fresh_memory_and_the_frames_flow() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    let first = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let old = producer.next_buffer().unwrap().unwrap();
    drop(first);
    host.stream_detach(0, "frames", FIRST).unwrap();
    let mut second = host
        .stream_attach(0, "frames", SECOND)
        .unwrap()
        .consumer()
        .unwrap();
    // The buffer the producer held under the first consumer is not on the new memory.
    assert_eq!(producer.buffer_current(old), Ok(false));
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(producer.buffer_current(buffer), Ok(true));
    let into = hardware(producer.buffer_handle(buffer).unwrap().unwrap());
    into.write_all_at(&[0x5A; 4], 0).unwrap();
    producer.publish(frame(buffer, 42)).unwrap();
    let got = second
        .pop_entry()
        .unwrap()
        .and_then(|entry| entry.frame())
        .expect("the frame, after the reattach");
    assert_eq!((got.buffer, got.sequence), (buffer, 42));
    let mut out = [0; 4];
    assert!(second.read(buffer, 0, &mut out));
    assert_eq!(out, [0x5A; 4]);
    drop(second);
    drop(into);
    host.stream_detach(0, "frames", SECOND).unwrap();
    let held = held_memfds();
    assert_eq!(
        held,
        before,
        "producer end of stream frames still holds {} dma-bufs (lent) after the last detach",
        held.0.saturating_sub(before.0)
    );
    assert_eq!(host.stream_stats(0, "frames").unwrap().attachments, 2);
}

#[test]
fn with_nobody_attached_the_producer_keeps_running_and_its_frames_are_dropped() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    let mut cycle = |sequence: u64| {
        let buffer = producer
            .next_buffer()
            .unwrap()
            .expect("a buffer with no consumer attached");
        assert_eq!(
            producer.write(buffer, 0, &[1; 4]),
            Ok(false),
            "nowhere to land"
        );
        producer.publish(frame(buffer, sequence)).unwrap();
    };
    cycle(0);
    let ((), count) = allocations(|| {
        for sequence in 1..1001 {
            cycle(sequence);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 frames with nobody attached");
    assert_eq!(held_memfds(), before, "no memory is made for nobody");
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.delivered, 0);
    assert!(
        stats.dropped >= 1000,
        "frames with no consumer are dropped: {stats:?}"
    );
    assert!(!stats.standby);
}

#[test]
fn with_standby_the_memory_is_kept_while_nobody_is_attached_and_the_kept_frame_comes_at_once() {
    let _serial = serial();
    let host = start(STANDBY);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    let buffer = producer.next_buffer().unwrap().unwrap();
    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(stats.standby);
    assert_eq!(stats.standby_bytes, u64::from(BUFFERS) * BYTES as u64);
    let kept_memory = held_memfds();
    assert!(kept_memory.0 > before.0, "standby memory is made and held");
    let into = hardware(producer.buffer_handle(buffer).unwrap().unwrap());
    into.write_all_at(&[7; 4], 0).unwrap();
    producer.publish(frame(buffer, 5)).unwrap();
    drop(into);
    // Nobody attached: the memory is still held, and the frame is kept for the next consumer.
    assert_eq!(held_memfds(), kept_memory, "held while nobody is attached");
    assert_eq!(host.stream_stats(0, "frames").unwrap().kept, 1);
    let mut consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let got = consumer
        .pop_entry()
        .unwrap()
        .and_then(|entry| entry.frame())
        .expect("the kept frame, at once");
    assert_eq!((got.buffer, got.sequence), (buffer, 5));
    let mut out = [0; 4];
    assert!(consumer.read(buffer, 0, &mut out));
    assert_eq!(out, [7; 4]);
    drop(consumer);
    host.stream_detach(0, "frames", FIRST).unwrap();
    // After the last consumer leaves, standby memory is made fresh when the producer next asks.
    producer.next_buffer().unwrap().unwrap();
    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(stats.standby);
    assert_eq!(stats.standby_bytes, u64::from(BUFFERS) * BYTES as u64);
}

#[test]
fn without_standby_nothing_is_held_while_nobody_is_attached() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    producer.next_buffer().unwrap().unwrap();
    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(!stats.standby);
    assert_eq!(stats.standby_bytes, 0);
    assert_eq!(held_memfds(), before);
}

#[test]
fn a_job_in_flight_keeps_its_buffer_until_the_job_ends() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let before = held_memfds();
    let consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    // The driver keeps the handle for a hardware job it has started.
    let job = producer.buffer_handle(buffer).unwrap().unwrap();
    drop(consumer);
    host.stream_detach(0, "frames", FIRST).unwrap();
    assert!(
        held_memfds().0 > before.0,
        "the job's buffer stays open while the job holds its handle"
    );
    // The job ends: the driver drops its handle, and the memory goes with it.
    drop(job);
    let held = held_memfds();
    assert_eq!(
        held,
        before,
        "driver fake-counter still holds {} dma-bufs of stream frames for an in-flight job after it ended",
        held.0.saturating_sub(before.0)
    );
}

#[test]
fn a_handle_cached_past_its_job_is_counted_as_held_by_the_driver_until_it_drops() {
    let _serial = serial();
    let host = start(DMA_BUF);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    // The driver caches its handle past the job.
    let cached = producer.buffer_handle(buffer).unwrap().unwrap();
    drop(consumer);
    host.stream_detach(0, "frames", FIRST).unwrap();
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(
        stats.held_by_driver,
        u64::from(BUFFERS) * BYTES as u64,
        "the cached handle holds the whole attachment open: {stats:?}"
    );
    drop(cached);
    assert_eq!(
        host.stream_stats(0, "frames").unwrap().held_by_driver,
        0,
        "nothing held once the handle drops"
    );
}
