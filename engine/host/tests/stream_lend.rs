//! A block lent a buffer of the stream its board entry names, on the device linked to it: the sink
//! never names a descriptor, the stream is never output0 by default, and a lent buffer is never reused
//! under it.

use std::sync::{Arc, Mutex};

use lemnos_core::{
    BusKind, CallerId, Config, Driver, LendHandle, Manifest, Need, NeedKind, Resources, Result,
    StreamKind,
};
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{Lend, StreamEndpoint};
use lemnos_host::board::parse;
use lemnos_host::stream::StreamAllocator;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;
use lemnos_wire::Planes;
use std::os::fd::OwnedFd;

const CONSUMER: CallerId = CallerId(7);
const FRAME: usize = 64;

/// The stream's producer side: a job-driven source, whose output0 the board file gives it.
struct Source;

impl Driver for Source {
    const MANIFEST: Manifest = Manifest {
        name: "lend-source",
        needs: &[Need {
            name: "output0",
            kind: NeedKind::Bus(BusKind::Stream),
            optional: true,
            stream: Some(StreamKind::JobDriven),
        }],
        provides: &[],
    };

    fn bind(_: &mut Resources<'_>, _: &Config<'_>) -> Result<Self> {
        Ok(Self)
    }

    fn describe(&self, _: &mut lemnos_core::Description) {}

    fn dispatch(
        &mut self,
        _: lemnos_core::MethodId,
        _: &[u8],
        _: &mut lemnos_core::Reply<'_>,
    ) -> lemnos_core::Status {
        lemnos_core::Status::NOT_SUPPORTED
    }
}

/// The reader side: takes the lend the link graph gives it, and keeps it for the test.
static LEND: Mutex<Option<LendHandle>> = Mutex::new(None);
/// The tests share `LEND`: one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Sink;

impl Driver for Sink {
    const MANIFEST: Manifest = Manifest {
        name: "lend-sink",
        needs: &[Need {
            name: "lend",
            kind: NeedKind::Bus(BusKind::Lend),
            optional: false,
            stream: None,
        }],
        provides: &[],
    };

    fn bind(res: &mut Resources<'_>, _: &Config<'_>) -> Result<Self> {
        let handle = res.lend("lend")?;
        *LEND.lock().unwrap() = Some(handle);
        Ok(Self)
    }

    fn describe(&self, _: &mut lemnos_core::Description) {}

    fn dispatch(
        &mut self,
        _: lemnos_core::MethodId,
        _: &[u8],
        _: &mut lemnos_core::Reply<'_>,
    ) -> lemnos_core::Status {
        lemnos_core::Status::NOT_SUPPORTED
    }
}

/// Makes every stream's buffers memfds standing in for dma-bufs, one per plane of each buffer: this
/// sandbox has no DMA heap. The stand-in differs from a dma-buf in that it is not a dma-buf (it has no
/// DMA attachment), so the lend's refusals for a non-dma-buf memory are not exercised here.
struct Standins;

impl StreamAllocator for Standins {
    fn allocate(
        &self,
        name: &str,
        layout: &lemnos_wire::ring::Layout,
    ) -> lemnos_core::Result<lemnos_host::stream::Buffers> {
        lemnos_host::stream::MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        _: &str,
        layout: &lemnos_wire::ring::Layout,
        _: &str,
        planes: &Planes,
    ) -> lemnos_core::Result<lemnos_host::stream::Buffers> {
        let fds: Vec<OwnedFd> = (0..layout.buffers)
            .flat_map(|_| {
                planes
                    .lengths()
                    .iter()
                    .map(|len| shm_ring::sealed_memfd(*len as usize).unwrap())
            })
            .collect();
        Ok(lemnos_host::stream::Buffers {
            fds,
            dma: true,
            sync: None,
        })
    }
}

/// The source is linked to the sink, and the sink's board entry names the source's stream it reads.
/// The source has `output0` and `frames`, so a lend from the wrong stream is visible.
fn start(lent: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"src\"\ndriver = \"lend-source\"\nlink = \"sink\"\n\
         [devices.streams.output0]\nbuffers = 4\nbuffer_bytes = {FRAME}\njob_slots = 2\njob_bytes = 16\nmemory = \"dma-buf\"\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = {FRAME}\njob_slots = 2\njob_bytes = 16\nmemory = \"dma-buf\"\n\
         [[devices]]\nid = \"sink\"\ndriver = \"lend-sink\"\nlend = \"{lent}\"\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<Source>(), entry::<Sink>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        Arc::new(Standins),
    )
    .unwrap()
}

#[test]
fn a_block_is_lent_a_published_frame_by_identity_and_a_lent_buffer_is_never_reused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let host = start("output0");
    let mut producer = host.stream_producer(0, "output0").unwrap();
    let _consumer = host
        .stream_attach(0, "output0", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let buffer = producer.ask_buffer().unwrap().expect("a free buffer");
    assert!(producer.write(buffer, 0, &[7; FRAME]).unwrap());
    producer
        .publish(StreamFrame {
            buffer,
            bytes: FRAME as u32,
            sequence: 1,
            identity: 42,
            timestamp_ns: 10,
            end_ns: 20,
        })
        .unwrap();

    let mut lend = LEND
        .lock()
        .unwrap()
        .take()
        .expect("the sink was bound with its lend");
    // The frame's identity is the one the lend names; any other is a mismatch, not a lend.
    assert!(matches!(
        lend.lend(buffer, 99).unwrap(),
        Lend::IdentityMismatch
    ));
    let Lend::Lent(frame) = lend.lend(buffer, 42).unwrap() else {
        panic!("the published frame is lent");
    };
    assert_eq!(frame.bytes as usize, FRAME);
    // Lent once: a second lend of the same buffer is busy, whatever the identity.
    assert!(matches!(lend.lend(buffer, 42).unwrap(), Lend::Busy));
    // While lent, the producer takes every other buffer back before it reuses this one.
    let mut seen = Vec::new();
    for _ in 0..8 {
        match producer.ask_buffer().unwrap() {
            Some(b) => {
                assert_ne!(b, buffer, "a lent buffer is never handed to the producer");
                seen.push(b);
            }
            None => break,
        }
    }
    drop(frame);
    lend.unlend(buffer, 42).unwrap();
    // Given back: a second unlend is refused, and the buffer can be lent again.
    assert!(lend.unlend(buffer, 42).is_err());
    assert!(matches!(lend.lend(buffer, 42).unwrap(), Lend::Lent(_)));
    let _ = seen;
}

#[test]
fn a_block_is_lent_the_stream_its_board_entry_names_and_no_other() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let host = start("frames");
    let mut output = host.stream_producer(0, "output0").unwrap();
    let _output_consumer = host
        .stream_attach(0, "output0", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut frames = host.stream_producer(0, "frames").unwrap();
    let _frames_consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    // Buffer 0 of each stream holds a frame, under a different identity: output0 under 42, frames under 43.
    let out = output.ask_buffer().unwrap().expect("a free buffer");
    assert!(output.write(out, 0, &[1; FRAME]).unwrap());
    output
        .publish(StreamFrame {
            buffer: out,
            bytes: FRAME as u32,
            sequence: 1,
            identity: 42,
            timestamp_ns: 10,
            end_ns: 20,
        })
        .unwrap();
    let buffer = frames.ask_buffer().unwrap().expect("a free buffer");
    assert!(frames.write(buffer, 0, &[2; FRAME]).unwrap());
    frames
        .publish(StreamFrame {
            buffer,
            bytes: FRAME as u32,
            sequence: 1,
            identity: 43,
            timestamp_ns: 10,
            end_ns: 20,
        })
        .unwrap();
    assert_eq!(
        out, buffer,
        "both streams' first buffer is 0: the identity tells them apart"
    );

    let mut lend = LEND
        .lock()
        .unwrap()
        .take()
        .expect("the sink was bound with its lend");
    // The sink reads `frames`: output0's identity 42 is not what the buffer holds.
    assert!(matches!(
        lend.lend(buffer, 42).unwrap(),
        Lend::IdentityMismatch
    ));
    let Lend::Lent(frame) = lend.lend(buffer, 43).unwrap() else {
        panic!("the frames stream's frame is lent");
    };
    assert_eq!(frame.bytes as usize, FRAME);
    drop(frame);
    lend.unlend(buffer, 43).unwrap();
}

/// A lent buffer outlives its stream's consumer: the detach does not give it back, the producer
/// never takes it while it is lent, and the block's unlend is what returns it.
#[test]
fn a_lent_buffer_survives_its_consumers_detach_until_the_block_unlends_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let host = start("output0");
    let mut producer = host.stream_producer(0, "output0").unwrap();
    let consumer = host
        .stream_attach(0, "output0", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let buffer = producer.ask_buffer().unwrap().expect("a free buffer");
    assert!(producer.write(buffer, 0, &[9; FRAME]).unwrap());
    producer
        .publish(StreamFrame {
            buffer,
            bytes: FRAME as u32,
            sequence: 1,
            identity: 42,
            timestamp_ns: 10,
            end_ns: 20,
        })
        .unwrap();
    let mut lend = LEND
        .lock()
        .unwrap()
        .take()
        .expect("the sink was bound with its lend");
    let Lend::Lent(frame) = lend.lend(buffer, 42).unwrap() else {
        panic!("the published frame is lent");
    };
    drop(consumer);
    host.stream_detach(0, "output0", CONSUMER).unwrap();
    // The detach took nothing back: the lent buffer is still not the producer's to reuse.
    for _ in 0..8 {
        match producer.ask_buffer().unwrap() {
            Some(b) => assert_ne!(b, buffer, "a lent buffer is never handed to the producer"),
            None => break,
        }
    }
    // Still lent, so a second lend is busy; the unlend, once the block gives it back, is accepted.
    assert!(matches!(lend.lend(buffer, 42).unwrap(), Lend::Busy));
    drop(frame);
    lend.unlend(buffer, 42).unwrap();
    assert!(
        lend.unlend(buffer, 42).is_err(),
        "a second unlend is refused"
    );
}
