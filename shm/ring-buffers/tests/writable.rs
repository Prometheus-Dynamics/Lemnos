//! Pools the publisher writes from memory it is given, and a consumer that writes buffers for the
//! publisher; and the owner's end word on a ring.

use std::os::fd::OwnedFd;

use shm_ring::{FieldId, Ring, SampleTime, Spec, sealed_memfd};
use shm_ring_buffers::{
    BufferDesc, BufferReader, BufferWriter, NextBatch, NoMarks, Pool, PoolView,
};

const FRAME: u64 = 4096;

fn spec() -> Spec {
    Spec {
        fields: 1,
        history: 8,
        max_payload: 128,
        expiry_ms: 0,
    }
}

/// A pool of `count` buffers, one descriptor each, from sealed memfds the publisher maps.
fn mapped_pool(count: u32) -> (Pool, Vec<OwnedFd>) {
    let fds: Vec<OwnedFd> = (0..count)
        .map(|_| sealed_memfd(FRAME as usize).expect("memfd"))
        .collect();
    let copies: Vec<OwnedFd> = fds.iter().map(|fd| fd.try_clone().expect("dup")).collect();
    let buffers = (0..count)
        .map(|index| BufferDesc {
            descriptor: index as u8,
            offset: 0,
            size: FRAME,
        })
        .collect();
    (Pool::mapped(1, copies, buffers).expect("pool"), fds)
}

#[test]
fn a_publisher_writes_its_own_mapped_pool_and_a_reader_sees_the_bytes() {
    let ring = Ring::create(&spec()).expect("ring");
    let (pool, _fds) = mapped_pool(2);
    let view = pool.view().expect("view");
    let mut writer = BufferWriter::new(ring.writer().expect("writer"), FieldId(0), pool);
    let mut reader = BufferReader::new(ring.reader().expect("reader"), FieldId(0), view, NoMarks);

    let buffer = writer
        .take(&shm_ring_buffers::NoHolders)
        .expect("free buffer");
    writer.buffer_mut(buffer).expect("mapped").fill(0x5a);
    writer
        .publish(&[buffer], SampleTime::from_nanos(7), b"meta")
        .expect("publish");

    match reader.next() {
        NextBatch::Batch(batch) => {
            assert_eq!(batch.held.bytes(0).expect("bytes")[..4], [0x5a; 4]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_consumer_writes_a_buffer_it_attached_writable_and_the_publisher_sees_it() {
    let ring = Ring::create(&spec()).expect("ring");
    let (pool, _fds) = mapped_pool(1);
    let mut consumer_view: PoolView = pool.writable_view().expect("writable view");
    let mut writer = BufferWriter::new(ring.writer().expect("writer"), FieldId(0), pool);
    let buffer = writer.take(&shm_ring_buffers::NoHolders).expect("free");
    writer
        .buffer_mut(buffer)
        .expect("mapped")
        .copy_from_slice(&[0_u8; FRAME as usize]);
    let bytes = consumer_view.bytes_mut(buffer).expect("writable");
    bytes[..3].copy_from_slice(b"job");
    let read_back = writer.buffer_mut(buffer).expect("mapped");
    assert_eq!(&read_back[..3], b"job");
}

#[test]
fn a_consumer_attached_read_only_cannot_write_the_buffers() {
    let ring = Ring::create(&spec()).expect("ring");
    let (pool, _fds) = mapped_pool(1);
    let mut view = pool.view().expect("read-only view");
    assert!(view.bytes_mut(shm_ring_buffers::BufferIndex(0)).is_none());
    let _keep = ring.reader().expect("reader");
}

#[test]
fn an_end_word_is_zero_while_the_topic_runs_and_the_owner_sets_it_once_it_ends() {
    let ring = Ring::create(&spec()).expect("ring");
    let reader = ring.reader().expect("reader");
    assert_eq!(reader.end(), 0);
    assert_eq!(ring.end(), 0);
    ring.set_end(2);
    assert_eq!(reader.end(), 2);
    assert_eq!(ring.end(), 2);
}

#[test]
fn a_pool_refuses_a_buffer_past_its_descriptor() {
    let fd = sealed_memfd(FRAME as usize).expect("memfd");
    let too_far = vec![BufferDesc {
        descriptor: 0,
        offset: 1,
        size: FRAME,
    }];
    assert!(Pool::mapped(1, vec![fd], too_far).is_err());
}
