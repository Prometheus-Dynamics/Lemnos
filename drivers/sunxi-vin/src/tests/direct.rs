//! The node filling the stream's own buffers, through a real host's stream of dma-bufs (sealed
//! memfds stand in for them). The stand-in node keeps a hold on each dma-buf it is queued, as
//! the kernel does, and fills a frame by writing through that hold, as the hardware would.

use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_contract_camera_receiver::{CameraReceiver, Packing};
use lemnos_core::{CallerId, Lifecycle, Status};
use lemnos_driver_codec_jobs::testing::PerBuffer;
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamEndpoint, StreamFrame};
use lemnos_host::board::parse;
use lemnos_host::stream::{Lease, ProducerEndpoint};
use lemnos_host::{Host, Options as HostOptions, entry};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;
use lemnos_wire::ring::Consumer;

use super::{FRAME, Handle, Hw, options, raw10};
use crate::{Mode, SunxiVin};

const BUFFERS: u32 = 4;

/// The host's producer end, counting the copies a driver makes into it.
struct Counted {
    inner: ProducerEndpoint,
    writes: Arc<AtomicU32>,
}

impl StreamEndpoint for Counted {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.inner.buffer_count()
    }

    fn buffer_kind(&self) -> BufferKind {
        self.inner.buffer_kind()
    }

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        self.inner.publish(frame)
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        self.inner.next_buffer()
    }

    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        self.inner.next_job(out)
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.write(buffer, offset, data)
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, ErrorKind> {
        self.inner.read(buffer, offset, out)
    }

    fn buffer_handle(&self, buffer: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.inner.buffer_handle(buffer)
    }

    fn buffer_current(&self, buffer: u32) -> Result<bool, ErrorKind> {
        self.inner.buffer_current(buffer)
    }

    fn fault_consumer(&mut self) -> Result<(), ErrorKind> {
        self.inner.fault_consumer()
    }
}

mod nothing {
    use lemnos_core::{Config, Lifecycle, Result, driver};

    /// A driver that leaves its device's stream to the test.
    #[driver("nothing", provides = [])]
    pub struct Nothing {}

    impl Lifecycle for Nothing {
        type Needs = NothingNeeds;

        fn start(_: NothingNeeds, _: &Config<'_>) -> Result<Self> {
            Ok(Self {})
        }
    }
}

struct Rig {
    host: Host,
    driver: SunxiVin,
    node: Handle,
    writes: Arc<AtomicU32>,
    now: u64,
}

/// What a consumer holds: its lease on the stream, and its end of the ring.
struct Client {
    lease: Lease,
    ring: Consumer,
}

impl Client {
    fn attach(rig: &Rig, caller: u64) -> Self {
        let lease = rig
            .host
            .stream_attach(0, "frames", CallerId(caller))
            .unwrap();
        let ring = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
        Self { lease, ring }
    }

    /// The bytes of `buffer`, as the consumer reads its own memory.
    fn bytes(&self, buffer: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let at = self.lease.layout.buffer_offset(buffer).unwrap();
        assert!(self.lease.buffers.read(at, &mut out));
        out
    }

    /// The inode of the memory behind `buffer`: which dma-buf it is.
    fn memory(&self, buffer: u32) -> u64 {
        let handles = self.lease.handles.as_ref().expect("a stream of handles");
        let file = std::fs::File::from(handles[buffer as usize].try_clone().unwrap());
        file.metadata().unwrap().ino()
    }
}

impl Rig {
    /// A host with a `frames` stream of `bytes`-byte dma-bufs, and the driver on it, started
    /// for the usual mode.
    fn with(bytes: u32) -> Self {
        Self::on(bytes, raw10())
    }

    /// As [`Self::with`], for the mode `link`.
    fn on(bytes: u32, link: lemnos_contract_camera_receiver::LinkConfig) -> Self {
        let text = format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
             [[devices]]\nid = \"csi\"\ndriver = \"nothing\"\n\
             [devices.streams.frames]\nmemory = \"dma-buf\"\nbuffers = {BUFFERS}\n\
             buffer_bytes = {bytes}\n"
        );
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start_with_streams(
            &parse(&text).unwrap(),
            &[entry::<nothing::Nothing>()],
            Box::new(sim),
            Arc::new(clock),
            HostOptions::default(),
            Arc::new(PerBuffer),
        )
        .unwrap();
        let writes = Arc::new(AtomicU32::new(0));
        let stream = Counted {
            inner: host.stream_producer(0, "frames").unwrap(),
            writes: Arc::clone(&writes),
        };
        let node = Handle::default();
        let hw = Hw {
            node: Arc::clone(&node.0),
            planes: (0..3).map(|_| Vec::with_capacity(FRAME)).collect(),
        };
        let mut driver = SunxiVin::with_hardware(
            Box::new(hw),
            lemnos_core::Held::boxed(Box::new(stream)),
            options(),
        );
        driver.configure(link, Packing::Unpacked16).unwrap();
        CameraReceiver::start(&mut driver).unwrap();
        Self {
            host,
            driver,
            node,
            writes,
            now: 0,
        }
    }

    fn tick(&mut self) {
        self.now += 1_000_000;
        Lifecycle::tick(&mut self.driver, self.now).expect("a streaming driver ticks");
    }

    /// The node fills its slot `slot` with a frame of `byte`, through its own hold on the
    /// dma-buf it was queued, and reports it.
    fn node_fills(&self, slot: u32, sequence: u32, byte: u8) {
        self.node.with(|n| {
            let held = n.held[slot as usize].take().expect("a queued slot");
            held.write_all_at(&[byte; FRAME], 0).unwrap();
            n.filled
                .push_back((slot, sequence, false, vec![vec![byte; FRAME]]));
        });
    }
}

#[test]
fn the_node_fills_the_streams_own_buffers_and_the_driver_copies_nothing() {
    let mut rig = Rig::with(FRAME as u32);
    assert_eq!(rig.driver.mode(), Mode::Direct);
    rig.node.with(|n| {
        assert_eq!(n.importing, [BUFFERS], "a slot for each stream buffer");
        assert!(n.streamed.is_empty(), "no buffers of the node's own");
        assert_eq!(n.opened.len(), 1);
    });
    // Nobody is attached: there is nothing to give the node to fill.
    rig.tick();
    assert_eq!(rig.node.with(|n| n.handles), 0);

    let mut client = Client::attach(&rig, 1);
    rig.tick();
    // Every stream buffer went to the node, each a dma-buf that holds a frame.
    assert_eq!(rig.node.with(|n| n.handles), BUFFERS);
    assert_eq!(
        rig.node
            .with(|n| n.last_handle.map(|(_, size)| size >= FRAME as u64)),
        Some(true)
    );

    for (sequence, slot) in [2u32, 0, 3].into_iter().enumerate() {
        let byte = 0xa0 + sequence as u8;
        rig.node_fills(slot, sequence as u32, byte);
        rig.tick();
        let frame = client.ring.pop_frame().unwrap().expect("a frame");
        // The stream buffer is the node's slot, and holds what the node wrote there.
        assert_eq!((frame.buffer, frame.bytes), (slot, FRAME as u32));
        assert_eq!(frame.sequence, sequence as u64);
        assert_eq!(client.bytes(slot, FRAME), vec![byte; FRAME]);
        // Given back, it goes to the node again.
        assert!(client.ring.release(frame.buffer, frame.fence).unwrap());
        rig.tick();
        assert_eq!(
            rig.node.with(|n| n.last_handle.map(|(index, _)| index)),
            Some(slot)
        );
    }
    assert_eq!(
        rig.writes.load(Ordering::Relaxed),
        0,
        "the driver copied nothing"
    );
    let counters = rig.driver.counters().unwrap();
    assert_eq!(
        (counters.frames, counters.dropped, counters.errors),
        (3, 0, 0)
    );
    assert_eq!(
        rig.node.with(|n| n.requeued.len()),
        0,
        "no buffer of the node's own"
    );
}

#[test]
fn a_consumer_that_goes_and_another_that_comes_each_keep_their_own_memory() {
    let mut rig = Rig::with(FRAME as u32);
    let first = Client::attach(&rig, 1);
    rig.tick();
    let old_memory = first.memory(1);

    // The first consumer goes with the node still holding its buffers; a frame the node
    // fills then lands in memory that is the first's alone, and reaches no one.
    rig.host.stream_detach(0, "frames", CallerId(1)).unwrap();
    rig.node_fills(1, 0, 0x11);
    rig.tick();
    let counters = rig.driver.counters().unwrap();
    assert_eq!((counters.frames, counters.dropped), (0, 1));
    assert_eq!(
        first.bytes(1, FRAME),
        vec![0x11; FRAME],
        "the old memory took the write"
    );

    // The next consumer's buffers are handed to the node afresh, on memory of its own.
    let mut second = Client::attach(&rig, 2);
    assert_ne!(second.memory(1), old_memory);
    let before = rig.node.with(|n| n.handles);
    rig.tick();
    rig.tick();
    assert!(rig.node.with(|n| n.handles) > before);
    assert!(
        second.ring.pop_frame().unwrap().is_none(),
        "nothing of the first's reaches it"
    );
    // Slots still queued from the first consumer's time fill its memory, and are dropped.
    let slot = rig
        .node
        .with(|n| n.last_handle.map(|(index, _)| index))
        .unwrap();
    rig.node_fills(slot, 1, 0x22);
    rig.tick();
    let frame = second.ring.pop_frame().unwrap().expect("a frame");
    assert_eq!((frame.buffer, frame.bytes), (slot, FRAME as u32));
    assert_eq!(second.bytes(slot, FRAME), vec![0x22; FRAME]);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
}

#[test]
fn a_stream_whose_buffers_cannot_hold_a_frame_is_never_handed_to_the_node() {
    // Frames twice as tall as the stream's buffers hold.
    let tall = lemnos_contract_camera_receiver::LinkConfig {
        height: 64,
        ..raw10()
    };
    let mut rig = Rig::on(FRAME as u32, tall);
    assert_eq!(rig.driver.mode(), Mode::Direct);
    let mut client = Client::attach(&rig, 1);
    rig.tick();
    rig.tick();
    assert_eq!(
        rig.node.with(|n| n.handles),
        0,
        "no dma-buf smaller than a frame is queued"
    );
    assert_eq!(
        rig.driver.counters().unwrap().errors,
        u64::from(BUFFERS),
        "each buffer counted once"
    );
    assert!(client.ring.pop_frame().unwrap().is_none());
}

#[test]
fn a_node_that_names_a_slot_it_was_never_given_is_restarted() {
    let mut rig = Rig::with(FRAME as u32);
    let _client = Client::attach(&rig, 1);
    rig.tick();
    // Slot 1 is filled and published: the consumer has it, and the node does not.
    rig.node_fills(1, 0, 0x33);
    rig.tick();
    assert_eq!(rig.driver.counters().unwrap().frames, 1);
    // A frame in a slot the node does not hold now, and in slots past the slots there are.
    for slot in [1, BUFFERS, 31, 32, u32::MAX] {
        rig.node
            .with(|n| n.filled.push_back((slot, 0, false, vec![vec![0; 16]])));
        let stops = rig.node.with(|n| n.stopped);
        rig.tick();
        assert_ne!(
            Lifecycle::health(&rig.driver),
            lemnos_core::Health::Ok,
            "slot {slot}: the node failed"
        );
        // Recovery opens it again.
        rig.now += 8_000_000_000;
        rig.tick();
        rig.tick();
        assert!(rig.node.with(|n| n.stopped) > stops, "slot {slot}");
        assert_eq!(
            Lifecycle::health(&rig.driver),
            lemnos_core::Health::Ok,
            "slot {slot}"
        );
    }
}

#[test]
fn a_node_that_takes_no_dma_bufs_is_opened_again_and_fed_by_copy() {
    let rig = {
        // The node refuses before the driver starts.
        let mut rig = Rig::with(FRAME as u32);
        CameraReceiver::stop(&mut rig.driver).unwrap();
        rig.node.with(|n| {
            n.refuse_import = true;
            n.opened.clear();
        });
        CameraReceiver::start(&mut rig.driver).unwrap();
        rig
    };
    assert_eq!(rig.driver.mode(), Mode::Copy);
    rig.node.with(|n| {
        assert_eq!(n.opened.len(), 2, "opened afresh for the copy path");
        assert_eq!(n.streamed.len(), 1);
    });
}

#[test]
fn frames_through_the_streams_own_buffers_allocate_nothing() {
    let mut rig = Rig::with(FRAME as u32);
    let mut client = Client::attach(&rig, 1);
    rig.tick();
    let mut allocated = 0;
    for round in 0..500u32 {
        let slot = round % BUFFERS;
        // The node's part and the consumer's part are the test's; the driver's is measured.
        rig.node_fills(slot, round, round as u8);
        let ((), count) = allocations(|| rig.tick());
        allocated += count;
        let frame = client.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!(frame.buffer, slot);
        assert!(client.ring.release(frame.buffer, frame.fence).unwrap());
        let ((), count) = allocations(|| rig.tick());
        allocated += count;
    }
    assert_eq!(allocated, 0);
    assert_eq!(rig.driver.counters().unwrap().frames, 500);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    let _ = Status::OK;
}
