//! The raw frame path: the receiver's capture node into a frame stream of a real host, with
//! the node filling the stream's own buffers (sealed memfds stand in for dma-bufs) or by copy.
//! The stand-in node keeps a hold on each dma-buf it is queued, as the kernel does.

use std::collections::VecDeque;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{BayerOrder, CameraReceiver, LinkConfig, Packing};
use lemnos_core::{CallerId, Health, Lifecycle, Result, Status};
use lemnos_driver_codec_jobs::testing::PerBuffer;
use lemnos_driver_v4l2_capture::{Frame, Granted, Graph, Hardware as Capture, Mode, Wanted};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamEndpoint, StreamFrame};
use lemnos_host::board::parse;
use lemnos_host::stream::{MemfdAllocator, ProducerEndpoint, StreamAllocator};
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;
use lemnos_v4l2::FourCc;
use lemnos_wire::ring::Consumer;

use super::{Hw, OFF, ON, receiver};
use crate::{Raw, Rkcif};

const BUFFERS: u32 = 4;
/// A 64 by 4 RAW10 frame in the compact layout: lines of 80 bytes, rounded up to 256.
const FRAME: usize = 256 * 4;

fn link() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 0,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: false,
        width: 64,
        height: 4,
        pixel_rate_hz: 80_000_000,
        link_rate_mbps: 800,
    }
}

/// What the capture node was asked and does.
#[derive(Default)]
struct Played {
    opened: Vec<Wanted>,
    streamed: Vec<u32>,
    importing: Vec<u32>,
    stopped: u32,
    handles: u32,
    held: Vec<Option<std::fs::File>>,
    filled: VecDeque<(u32, u32)>,
    requeued: u32,
    fail_dequeue: bool,
}

#[derive(Clone, Default)]
struct Node(Arc<Mutex<Played>>);

impl Node {
    fn with<R>(&self, f: impl FnOnce(&mut Played) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

/// The node, and the bytes of its own buffers for the copy path.
struct NodeEnd {
    node: Node,
    payload: Vec<u8>,
}

impl Capture for NodeEnd {
    fn open(&mut self, wanted: &Wanted) -> Result<Granted> {
        self.node.with(|n| n.opened.push(*wanted));
        Ok(Granted {
            fourcc: wanted.fourcc,
            width: wanted.width,
            height: wanted.height,
            stride: wanted.stride,
            size: wanted.size,
            planes: 1,
            plane_sizes: [wanted.size, 0, 0, 0, 0, 0, 0, 0],
        })
    }

    fn stream_on(&mut self, buffers: u32) -> Result<()> {
        self.node.with(|n| n.streamed.push(buffers));
        Ok(())
    }

    fn stream_on_importing(&mut self, slots: u32) -> Result<()> {
        self.node.with(|n| {
            n.importing.push(slots);
            n.held = (0..slots).map(|_| None).collect();
        });
        Ok(())
    }

    fn queue_handles(&mut self, index: u32, planes: &[BufferHandle<'_>]) -> Result<()> {
        let [handle] = planes else {
            return Err(Status::INVALID_ARGUMENT);
        };
        let file = std::fs::File::from(handle.fd().try_clone_to_owned().map_err(|_| Status::IO)?);
        self.node.with(|n| {
            n.handles += 1;
            let slot = n
                .held
                .get_mut(index as usize)
                .ok_or(Status::INVALID_ARGUMENT)?;
            *slot = Some(file);
            Ok(())
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.node.with(|n| n.stopped += 1);
        Ok(())
    }

    fn dequeue(&mut self) -> Result<Option<Frame>> {
        self.node.with(|n| {
            if std::mem::take(&mut n.fail_dequeue) {
                return Err(Status::IO);
            }
            Ok(n.filled.pop_front().map(|(index, sequence)| Frame {
                index,
                sequence,
                timestamp_ns: u64::from(sequence) * 1000,
                bad: false,
                planes: 1,
                bytes: FRAME as u32,
            }))
        })
    }

    fn payload(&self, _frame: &Frame, plane: usize) -> Result<&[u8]> {
        if plane == 0 {
            Ok(&self.payload)
        } else {
            Err(Status::INVALID_ARGUMENT)
        }
    }

    fn requeue(&mut self, _index: u32) -> Result<()> {
        self.node.with(|n| n.requeued += 1);
        Ok(())
    }

    fn graph(&mut self) -> Result<Graph> {
        Err(Status::NO_DEVICE)
    }
}

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

    fn publish(&mut self, frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        self.inner.publish(frame)
    }

    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        self.inner.next_buffer()
    }

    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        self.inner.next_job(out)
    }

    fn write(
        &mut self,
        buffer: u32,
        offset: usize,
        data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.write(buffer, offset, data)
    }

    fn read(
        &mut self,
        buffer: u32,
        offset: usize,
        out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        self.inner.read(buffer, offset, out)
    }

    fn buffer_handle(
        &self,
        buffer: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.inner.buffer_handle(buffer)
    }

    fn buffer_current(&self, buffer: u32) -> core::result::Result<bool, ErrorKind> {
        self.inner.buffer_current(buffer)
    }

    fn planes(&self) -> u32 {
        self.inner.planes()
    }

    fn plane_offset(&self, plane: u32) -> Option<usize> {
        self.inner.plane_offset(plane)
    }

    fn plane_handle(
        &self,
        buffer: u32,
        plane: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.inner.plane_handle(buffer, plane)
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
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
    driver: Rkcif,
    route: Hw,
    node: Node,
    writes: Arc<AtomicU32>,
    now: u64,
}

struct Client {
    ring: Consumer,
}

impl Client {
    fn attach(rig: &Rig, caller: u64) -> Self {
        let ring = rig
            .host
            .stream_attach(0, "frames", CallerId(caller))
            .unwrap()
            .consumer()
            .unwrap();
        Self { ring }
    }

    fn bytes(&self, buffer: u32) -> Vec<u8> {
        let mut out = vec![0u8; FRAME];
        assert!(self.ring.read(buffer, 0, &mut out));
        out
    }
}

impl Rig {
    /// The driver with its raw path on a host's `frames` stream, of dma-bufs when `dma_buf`,
    /// configured and started.
    fn start(dma_buf: bool) -> Self {
        Self::start_with(dma_buf, "", Packing::Packed)
    }

    /// As [`start`](Self::start), with `keys` added to the stream and the link configured with
    /// `packing`.
    fn start_with(dma_buf: bool, keys: &str, packing: Packing) -> Self {
        let memory = if dma_buf {
            format!("memory = \"dma-buf\"\n{keys}")
        } else {
            keys.to_string()
        };
        let text = format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
             [[devices]]\nid = \"cif\"\ndriver = \"nothing\"\n\
             [devices.streams.frames]\n{memory}buffers = {BUFFERS}\nbuffer_bytes = {FRAME}\n"
        );
        let allocator: Arc<dyn StreamAllocator> = if dma_buf {
            Arc::new(PerBuffer)
        } else {
            Arc::new(MemfdAllocator)
        };
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start_with_streams(
            &parse(&text).unwrap(),
            &[entry::<nothing::Nothing>()],
            Box::new(sim),
            Arc::new(clock),
            Options::default(),
            allocator,
        )
        .unwrap();
        let writes = Arc::new(AtomicU32::new(0));
        let stream = Counted {
            inner: host.stream_producer(0, "frames").unwrap(),
            writes: Arc::clone(&writes),
        };
        let route = Hw::default();
        route.with(|m| m.topology = receiver(ON, OFF, OFF));
        let node = Node::default();
        let end = NodeEnd {
            node: node.clone(),
            payload: vec![0x5a; FRAME],
        };
        let raw = Raw {
            order: BayerOrder::Rggb,
            buffers: 3,
        };
        let mut driver = Rkcif::with_hardware(Box::new(route.clone())).with_frames(
            Box::new(end),
            lemnos_core::Held::boxed(Box::new(stream)),
            raw,
        );
        driver.configure(link(), packing).unwrap();
        CameraReceiver::start(&mut driver).unwrap();
        Self {
            host,
            driver,
            route,
            node,
            writes,
            now: 0,
        }
    }

    fn tick(&mut self) {
        self.now += 1_000_000;
        Lifecycle::tick(&mut self.driver, self.now).expect("a routed driver ticks");
    }

    /// The node fills its slot `slot` through its own hold on the dma-buf, and reports it.
    fn node_fills(&self, slot: u32, sequence: u32, byte: u8) {
        self.node.with(|n| {
            let held = n.held[slot as usize].take().expect("a queued slot");
            held.write_all_at(&[byte; FRAME], 0).unwrap();
            n.filled.push_back((slot, sequence));
        });
    }
}

#[test]
fn the_capture_node_fills_the_streams_own_buffers_and_nothing_is_copied() {
    let mut rig = Rig::start(true);
    assert_eq!(rig.driver.mode(), Mode::Direct);
    let mut name = [0u8; 32];
    let len = rig.driver.frame_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"frames");
    rig.node.with(|n| {
        // The node is asked for the compact layout the receiver writes, with no input index.
        assert_eq!(
            n.opened,
            [Wanted {
                input: None,
                fourcc: FourCc::new(b"RG10"),
                width: 64,
                height: 4,
                stride: 256,
                size: FRAME as u32,
                mode: 0,
            }]
        );
        assert_eq!(n.importing, [BUFFERS]);
        assert!(n.streamed.is_empty());
    });
    let mut client = Client::attach(&rig, 1);
    rig.tick();
    assert_eq!(rig.node.with(|n| n.handles), BUFFERS);
    for (sequence, slot) in [1u32, 3, 0].into_iter().enumerate() {
        let byte = 0xc0 + sequence as u8;
        rig.node_fills(slot, sequence as u32, byte);
        rig.tick();
        let frame = client.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!((frame.buffer, frame.bytes), (slot, FRAME as u32));
        assert_eq!(client.bytes(slot), vec![byte; FRAME]);
        assert!(client.ring.release(frame.buffer));
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

    // A consumer that goes: a frame the node fills afterwards reaches no one, and the next
    // consumer's buffers go to the node afresh.
    rig.tick();
    rig.host.stream_detach(0, "frames", CallerId(1)).unwrap();
    rig.node_fills(2, 3, 0x77);
    rig.tick();
    assert_eq!(rig.driver.counters().unwrap().dropped, 1);
    let mut next = Client::attach(&rig, 2);
    let before = rig.node.with(|n| n.handles);
    rig.tick();
    rig.tick();
    assert!(rig.node.with(|n| n.handles) > before);
    assert!(next.ring.pop_frame().unwrap().is_none());
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
}

#[test]
fn on_a_stream_of_plain_memory_each_frame_is_copied_once() {
    let mut rig = Rig::start(false);
    assert_eq!(rig.driver.mode(), Mode::Copy);
    rig.node.with(|n| {
        assert!(n.importing.is_empty());
        assert_eq!(
            n.streamed,
            [3],
            "the node's own buffers, as many as configured"
        );
    });
    let mut client = Client::attach(&rig, 1);
    rig.node.with(|n| n.filled.push_back((1, 0)));
    rig.tick();
    let frame = client.ring.pop_frame().unwrap().expect("a frame");
    assert_eq!(frame.bytes, FRAME as u32);
    assert_eq!(client.bytes(frame.buffer), vec![0x5a; FRAME]);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
    assert_eq!(rig.node.with(|n| n.requeued), 1);
    assert_eq!(rig.driver.counters().unwrap().frames, 1);
}

#[test]
fn a_capture_node_that_fails_is_routed_and_started_again_and_stop_ends_it() {
    let mut rig = Rig::start(true);
    let _client = Client::attach(&rig, 1);
    rig.tick();
    rig.node.with(|n| n.fail_dequeue = true);
    rig.tick();
    assert_ne!(Lifecycle::health(&rig.driver), Health::Ok);
    rig.now += 8_000_000_000;
    rig.tick();
    rig.tick();
    assert_eq!(Lifecycle::health(&rig.driver), Health::Ok);
    rig.node.with(|n| {
        assert_eq!(n.opened.len(), 2, "opened again");
        assert_eq!(n.importing.len(), 2);
    });
    let routed = rig.route.with(|m| m.topology.links.len());
    assert!(routed > 0);
    let stops = rig.node.with(|n| n.stopped);
    CameraReceiver::stop(&mut rig.driver).unwrap();
    assert_eq!(rig.node.with(|n| n.stopped), stops + 1);
}

#[test]
fn raw_frames_through_the_streams_own_buffers_allocate_nothing() {
    let mut rig = Rig::start(true);
    let mut client = Client::attach(&rig, 1);
    rig.tick();
    let mut allocated = 0;
    for round in 0..300u32 {
        let slot = round % BUFFERS;
        rig.node_fills(slot, round, round as u8);
        let ((), count) = allocations(|| rig.tick());
        allocated += count;
        let frame = client.ring.pop_frame().unwrap().expect("a frame");
        assert!(client.ring.release(frame.buffer));
        let ((), count) = allocations(|| rig.tick());
        allocated += count;
    }
    assert_eq!(allocated, 0);
    assert_eq!(rig.driver.counters().unwrap().frames, 300);
}

#[test]
fn without_the_stream_and_the_node_there_is_no_raw_path() {
    let route = Hw::default();
    let mut driver = Rkcif::with_hardware(Box::new(route));
    assert_eq!(
        driver.frame_stream(&mut [0u8; 32]),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(driver.counters(), Err(Status::NOT_SUPPORTED));
}

#[test]
fn sixteen_bit_words_ask_the_node_for_the_uapi_format_in_word_mode() {
    let rig = Rig::start_with(true, "", Packing::Unpacked16);
    rig.node.with(|n| {
        assert_eq!(
            n.opened,
            [Wanted {
                input: None,
                // `V4L2_PIX_FMT_SRGGB10`: ten bits expanded to sixteen, as the uAPI defines it.
                fourcc: FourCc::new(b"RG10"),
                width: 64,
                height: 4,
                // 64 words of 2 bytes, rounded up to 8.
                stride: 128,
                size: 512,
                mode: crate::MODE_WORDS,
            }]
        );
    });
    let mut twelve = link();
    twelve.bit_depth = 12;
    twelve.width = 61;
    let layout = crate::layout(&twelve, Packing::Unpacked16).unwrap();
    assert_eq!(
        (layout.packing, layout.stride, layout.size),
        (Packing::Unpacked16, 128, 512),
        "61 words of 2 bytes, rounded up to 8"
    );
}

#[test]
fn on_standby_the_node_captures_with_nobody_attached_and_a_consumer_gets_the_newest_at_once() {
    let mut rig = Rig::start_with(true, "standby = true\n", Packing::Packed);
    assert_eq!(rig.driver.mode(), Mode::Direct);
    // Nobody is attached, and the node is fed the stream's buffers all the same.
    rig.tick();
    assert_eq!(rig.node.with(|n| n.handles), BUFFERS);
    rig.node_fills(0, 0, 0x11);
    rig.tick();
    rig.node_fills(1, 1, 0x22);
    rig.tick();
    let counters = rig.driver.counters().unwrap();
    assert_eq!((counters.frames, counters.dropped), (2, 0));
    let stats = rig.host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.kept, stats.delivered), (1, 0));
    // The consumer gets the newest frame the moment it attaches, in the memory the node wrote.
    let mut client = Client::attach(&rig, 1);
    let frame = client
        .ring
        .pop_frame()
        .unwrap()
        .expect("the newest frame, at once");
    assert_eq!((frame.buffer, frame.sequence), (1, 1));
    assert_eq!(client.bytes(1), vec![0x22; FRAME]);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0, "nothing was copied");
    // Buffers the node holds from before it came are still the consumer's memory.
    rig.node_fills(2, 2, 0x33);
    rig.tick();
    let frame = client.ring.pop_frame().unwrap().expect("a frame");
    assert_eq!(frame.buffer, 2);
    assert_eq!(client.bytes(2), vec![0x33; FRAME]);
    // Steady state with nobody attached allocates nothing either.
    rig.host.stream_detach(0, "frames", CallerId(1)).unwrap();
    rig.tick();
    rig.tick();
    let mut allocated = 0;
    for round in 0..200u32 {
        let slot = rig.node.with(|n| n.held.iter().position(Option::is_some));
        let Some(slot) = slot else {
            let ((), count) = allocations(|| rig.tick());
            allocated += count;
            continue;
        };
        rig.node_fills(slot as u32, 10 + round, round as u8);
        let ((), count) = allocations(|| rig.tick());
        allocated += count;
    }
    assert_eq!(allocated, 0);
    assert_eq!(rig.host.stream_stats(0, "frames").unwrap().kept, 1);
}

/// A driver whose embedded path is on: a metadata node that fills lines of `FRAME` bytes, and a
/// host with an `embedded` stream of plain memory the lines are copied into.
fn embedded_driver() -> (Host, Rkcif, Node) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"cif\"\ndriver = \"nothing\"\n\
         [devices.streams.embedded]\nbuffers = {BUFFERS}\nbuffer_bytes = {FRAME}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<nothing::Nothing>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        Arc::new(MemfdAllocator),
    )
    .unwrap();
    let stream = host.stream_producer(0, "embedded").unwrap();
    let route = Hw::default();
    route.with(|m| m.topology = receiver(ON, OFF, OFF));
    let node = Node::default();
    let end = NodeEnd {
        node: node.clone(),
        payload: vec![0x5a; FRAME],
    };
    let mut driver = Rkcif::with_hardware(Box::new(route)).with_embedded(
        Box::new(end),
        lemnos_core::Held::boxed(Box::new(stream)),
        FourCc::new(b"BG10"),
        FRAME as u32,
    );
    driver.configure(link(), Packing::Packed).unwrap();
    CameraReceiver::start(&mut driver).unwrap();
    (host, driver, node)
}

#[test]
fn embedded_lines_come_off_the_metadata_node_onto_the_embedded_stream() {
    let (host, mut driver, node) = embedded_driver();
    let mut name = [0u8; 16];
    let len = driver.embedded_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"embedded");
    node.with(|n| {
        assert_eq!(
            n.opened,
            [Wanted {
                input: None,
                fourcc: FourCc::new(b"BG10"),
                width: 0,
                height: 0,
                stride: 0,
                size: FRAME as u32,
                mode: 0,
            }],
            "the node is asked for lines of the board's size, in the board's format"
        );
        assert_eq!(n.streamed, [4], "started with every buffer queued");
    });
    let lease = host.stream_attach(0, "embedded", CallerId(7)).unwrap();
    let mut ring = lease.consumer().unwrap();
    node.with(|n| n.filled.push_back((0, 5)));
    Lifecycle::tick(&mut driver, 1_000_000).expect("a started driver ticks");
    let line = ring.pop_frame().unwrap().expect("the line is published");
    assert_eq!((line.bytes, line.sequence), (FRAME as u32, 5));
    assert!(ring.release(line.buffer));
    let mut out = vec![0u8; FRAME];
    assert!(ring.read(line.buffer, 0, &mut out));
    assert!(out.iter().all(|&b| b == 0x5a), "the line's bytes, copied");
    assert_eq!(
        node.with(|n| n.requeued),
        1,
        "the node's buffer goes back to the node"
    );
    let before = node.with(|n| n.stopped);
    assert_eq!(CameraReceiver::stop(&mut driver), Ok(()));
    assert_eq!(
        node.with(|n| n.stopped),
        before + 1,
        "the metadata node stops with the receiver"
    );
}
