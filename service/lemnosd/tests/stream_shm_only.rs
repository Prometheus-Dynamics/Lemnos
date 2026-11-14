//! A process with nothing but the shm crates reads a lemnosd stream: the protocol is spoken by
//! hand with `lemnos_wire`, and the consumer is built from the descriptors with `shm_ring`,
//! `shm_ring_buffers`, `shm_ring_holds` and `shm_ring_doorbell` alone. No `lemnos-client`.

mod common;

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use common::{Running, start_with};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_linux_sys::scm::recv_with_fds;
use lemnos_wire::ring::{FrameInfo, fds};
use lemnos_wire::{Frame, VERSION, append, decode};
use shm_ring::{FieldId, Reader, WriteMap};
use shm_ring_buffers::{BufferDesc, BufferReader, NextBatch, PoolView};
use shm_ring_doorbell::Doorbell;
use shm_ring_holds::Marks;

const DEVICE: u32 = 0;
const NAME: &str = "frames";
const BUFFER_BYTES: u32 = 4096;
const BUFFERS: u32 = 4;
const WORKING: u32 = 1;
const BOARD: &str = "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
                     [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\nworking = 1\n";
/// A capture time, CLOCK_MONOTONIC in nanoseconds: far from the time the test runs.
const CAPTURED_NS: u64 = 1_234_567_890_123;
const PAYLOAD: [u8; 5] = [7, 8, 9, 10, 11];

/// The protocol, spoken by hand: send a frame, read what comes back with its descriptors.
struct Wire {
    socket: UnixStream,
    buffer: Vec<u8>,
    filled: usize,
    fds: Vec<OwnedFd>,
}

impl Wire {
    fn connect(path: &Path) -> Self {
        let socket = UnixStream::connect(path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut wire = Self {
            socket,
            buffer: vec![0; 1 << 16],
            filled: 0,
            fds: Vec::new(),
        };
        wire.send(&Frame::Hello {
            core: lemnos_wire::CORE_HASH,
            lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
            version: VERSION,
            name: "shm-only",
            events: false,
            parts: lemnos_core::Tail(&lemnos_wire::PART_BYTES),
        });
        wire.next(|frame| matches!(frame, Frame::Welcome { .. }).then_some(()));
        wire
    }

    fn send(&mut self, frame: &Frame<'_>) {
        let mut out = Vec::new();
        append(frame, &mut out).unwrap();
        self.socket.write_all(&out).unwrap();
    }

    fn next<R>(&mut self, mut want: impl FnMut(Frame<'_>) -> Option<R>) -> R {
        loop {
            let mut used = 0;
            let mut found = None;
            while let Ok(Some((frame, n))) = decode(&self.buffer[used..self.filled]) {
                used += n;
                found = want(frame);
                if found.is_some() {
                    break;
                }
            }
            self.buffer.copy_within(used..self.filled, 0);
            self.filled -= used;
            if let Some(found) = found {
                return found;
            }
            let n = recv_with_fds(
                self.socket.as_fd(),
                &mut self.buffer[self.filled..],
                &mut self.fds,
            )
            .expect("the socket");
            assert!(n > 0, "the service closed the connection");
            self.filled += n;
        }
    }
}

#[test]
fn a_process_with_only_the_shm_crates_reads_a_lemnosd_stream() {
    let running: Running = start_with(BOARD);

    // Open the stream over the protocol, and take its answer and descriptors.
    let mut wire = Wire::connect(&running.socket);
    wire.send(&Frame::StreamOpen {
        id: 1,
        device: DEVICE,
        name: NAME,
    });
    let layout = wire.next(|frame| match frame {
        Frame::StreamRings {
            buffers,
            descriptors,
            buffer_bytes,
            frame_slots,
            job_slots,
            job_bytes,
            working,
            ..
        } => {
            // Shared memory: the seven base descriptors and the pool's one.
            assert_eq!(descriptors, 8);
            Some((
                buffers,
                buffer_bytes,
                frame_slots,
                job_slots,
                job_bytes,
                working.0,
            ))
        }
        _ => None,
    });
    assert_eq!(layout.0, BUFFERS);
    assert_eq!(layout.1, BUFFER_BYTES);
    // The working set the board declares: a foreign reader's margin is BUFFERS - 1 - it.
    assert_eq!(layout.5, WORKING);
    assert_eq!(wire.fds.len(), 8, "eight descriptors came with it");
    let mut all = std::mem::take(&mut wire.fds);
    let buffer_fd = all.remove(fds::BUFFERS);
    let base = all;

    // The driver side: a producer in this process, publishing one frame into buffer 1.
    let mut producer = running
        .service
        .as_ref()
        .unwrap()
        .host()
        .stream_producer(DEVICE, NAME)
        .unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(producer.write(buffer, 0, &PAYLOAD), Ok(true));
    producer
        .publish(StreamFrame {
            buffer,
            bytes: PAYLOAD.len() as u32,
            sequence: 42,
            identity: 7,
            timestamp_ns: CAPTURED_NS,
        })
        .unwrap();

    // The consumer, from the descriptors alone, in the order `lemnos_wire::ring::fds` gives.
    let stride = u64::from(BUFFER_BYTES).next_multiple_of(4096);
    let descs: Vec<BufferDesc> = (0..BUFFERS)
        .map(|b| BufferDesc {
            descriptor: 0,
            offset: u64::from(b) * stride,
            size: u64::from(BUFFER_BYTES),
        })
        .collect();
    let reader = Reader::attach_from_start(base[fds::RING].try_clone().unwrap()).unwrap();
    let view = PoolView::attach(
        base[fds::POOL_PAGE].try_clone().unwrap(),
        vec![buffer_fd],
        &descs,
    )
    .unwrap();
    let marks = Marks::over(
        WriteMap::new(base[fds::MARKS].try_clone().unwrap()).unwrap(),
        BUFFERS,
        0,
        0,
    )
    .unwrap();
    let mut frames = BufferReader::new(reader, FieldId(0), view, marks);
    let doorbell = Doorbell::attach(
        base[fds::BELL].try_clone().unwrap(),
        base[fds::BELL_PAGE].try_clone().unwrap(),
    )
    .unwrap();
    let _ = doorbell;

    // Read one frame: its metadata decodes, and its bytes are the driver's.
    match frames.next() {
        NextBatch::Batch(batch) => {
            let info = FrameInfo::decode(batch.meta).expect("the frame's metadata");
            assert_eq!(
                (info.sequence, info.identity, info.bytes),
                (42, 7, PAYLOAD.len() as u32)
            );
            // The batch's time is the frame's capture time, not the time it was read.
            assert_eq!(batch.time.as_nanos(), CAPTURED_NS);
            let index = batch.held.buffer(0).expect("the frame names a buffer");
            assert_eq!(index.0, buffer);
            let bytes = batch.held.bytes(0).expect("its bytes");
            assert_eq!(&bytes[..info.bytes as usize], PAYLOAD);
            // Dropping the held frame gives its buffer back.
            drop(batch);
        }
        _ => panic!("the published frame did not come"),
    }
}
