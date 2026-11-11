//! The driver against a stand-in for the capture node and a mocked stream: every expected value
//! here is written out by hand.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{Health, Lifecycle, Result, Status, StreamHandle};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{BufferHandle, StreamEndpoint, StreamFrame};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;
use lemnos_v4l2::FourCc;
use lemnos_v4l2::media::{
    Entity, EntityFlags, EntityFunction, Link, LinkFlags, Pad, PadFlags, Topology,
};

use crate::hw::{Frame, Granted, Graph, Hardware, Wanted};
use crate::route::{Feeds, feeds};
use crate::{Options, SunxiVin, backoff_ns, fourcc, parse_order};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// What the node does, shared with the test.
#[derive(Default)]
struct Node {
    opened: Vec<Wanted>,
    streamed: Vec<u32>,
    stopped: u32,
    /// Frames the node will fill: buffer index, sequence, bad, and each plane's bytes.
    filled: VecDeque<(u32, u32, bool, Vec<Vec<u8>>)>,
    requeued: Vec<u32>,
    fail_open: Option<Status>,
    fail_dequeue: Option<Status>,
    /// What `open` answers instead of what was asked.
    answer: Option<Granted>,
    graph: Option<Graph>,
}

/// The node: the test's handle on it, and the bytes of the buffer dequeued last, held here so
/// `payload` can lend them.
struct Hw {
    node: Arc<Mutex<Node>>,
    planes: Vec<Vec<u8>>,
}

#[derive(Clone, Default)]
struct Handle(Arc<Mutex<Node>>);

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut Node) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// The node fills buffer `index` with one frame of `planes`.
    fn fill(&self, index: u32, sequence: u32, planes: &[&[u8]]) {
        let planes = planes.iter().map(|p| p.to_vec()).collect();
        self.with(|n| n.filled.push_back((index, sequence, false, planes)));
    }
}

impl Hardware for Hw {
    fn open(&mut self, wanted: &Wanted) -> Result<Granted> {
        let mut n = self.node.lock().unwrap();
        if let Some(status) = n.fail_open {
            return Err(status);
        }
        n.opened.push(*wanted);
        Ok(n.answer.unwrap_or(Granted {
            fourcc: wanted.fourcc,
            width: wanted.width,
            height: wanted.height,
            stride: wanted.stride,
            size: wanted.size,
        }))
    }

    fn stream_on(&mut self, buffers: u32) -> Result<()> {
        self.node.lock().unwrap().streamed.push(buffers);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.node.lock().unwrap().stopped += 1;
        Ok(())
    }

    fn dequeue(&mut self) -> Result<Option<Frame>> {
        let mut n = self.node.lock().unwrap();
        if let Some(status) = n.fail_dequeue.take() {
            return Err(status);
        }
        let Some((index, sequence, bad, planes)) = n.filled.pop_front() else {
            return Ok(None);
        };
        // Into buffers made before, as the node's mappings are.
        for (to, from) in self.planes.iter_mut().zip(&planes) {
            to.clear();
            to.extend_from_slice(from);
        }
        Ok(Some(Frame {
            index,
            sequence,
            timestamp_ns: u64::from(sequence) * 1000,
            bad,
            planes: planes.len(),
        }))
    }

    fn payload(&self, _frame: &Frame, plane: usize) -> Result<&[u8]> {
        self.planes
            .get(plane)
            .map(Vec::as_slice)
            .ok_or(Status::INVALID_ARGUMENT)
    }

    fn requeue(&mut self, index: u32) -> Result<()> {
        self.node.lock().unwrap().requeued.push(index);
        Ok(())
    }

    fn graph(&mut self) -> Result<Graph> {
        self.node
            .lock()
            .unwrap()
            .graph
            .clone()
            .ok_or(Status::NO_DEVICE)
    }
}

/// The sensor's mode: 64x4 RAW10 over 2 lanes at 800 Mbit/s each. Small, so a frame's bytes can
/// be checked one by one.
fn raw10() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 0,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: false,
        width: 64,
        height: 32,
        pixel_rate_hz: 80_000_000,
        link_rate_mbps: 800,
    }
}

/// 64x32 at 16 bits.
const FRAME: usize = 64 * 2 * 32;

fn options() -> Options {
    Options {
        order: BayerOrder::Rggb,
        ..Options::default()
    }
}

fn driver_on(stream: StreamHandle) -> (SunxiVin, Handle) {
    let handle = Handle::default();
    let hw = Hw {
        node: Arc::clone(&handle.0),
        planes: (0..3).map(|_| Vec::with_capacity(FRAME)).collect(),
    };
    (
        SunxiVin::with_hardware(Box::new(hw), stream, options()),
        handle,
    )
}

fn driver(stream: &MockStream) -> (SunxiVin, Handle) {
    driver_on(Box::new(stream.clone()))
}

fn streaming(stream: &MockStream) -> (SunxiVin, Handle) {
    let (mut d, node) = driver(stream);
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
    (d, node)
}

/// A frame's bytes: every byte different, and different per frame.
fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

#[test]
fn limits_are_the_blocks() {
    let (mut d, _node) = driver(&MockStream::new(4));
    let limits = d.limits().unwrap();
    assert_eq!(limits.max_lanes, 4);
    assert_eq!(limits.max_link_rate_mbps, 2500);
    assert_eq!((limits.max_width, limits.max_height), (4800, 4800));
}

#[test]
fn configure_works_out_the_layout_the_node_writes() {
    let (mut d, _node) = driver(&MockStream::new(4));
    assert_eq!(
        d.configure(raw10(), Packing::Unpacked16).unwrap(),
        FrameLayout {
            width: 64,
            height: 32,
            bit_depth: 10,
            packing: Packing::Unpacked16,
            stride: 128,
            size: 4096,
        }
    );
    let raw8 = LinkConfig {
        bit_depth: 8,
        data_type: 0x2a,
        width: 1001,
        ..raw10()
    };
    let got = d.configure(raw8, Packing::Packed).unwrap();
    assert_eq!((got.stride, got.size), (1001, 32_032), "no line padding");
    let raw16 = LinkConfig {
        bit_depth: 16,
        data_type: 0x2e,
        ..raw10()
    };
    let got = d.configure(raw16, Packing::Packed).unwrap();
    assert_eq!((got.stride, got.size), (128, 4096));
    assert_eq!(d.link().unwrap(), Some(raw16));
}

#[test]
fn configure_refuses_what_the_node_cannot_take() {
    let (mut d, _node) = driver(&MockStream::new(4));
    let mut bad = |f: &dyn Fn(&mut LinkConfig), packing: Packing| {
        let mut link = raw10();
        f(&mut link);
        assert_eq!(
            d.configure(link, packing).unwrap_err(),
            Status::INVALID_ARGUMENT
        );
    };
    bad(&|l| l.lanes = 0, Packing::Unpacked16);
    bad(&|l| l.lanes = 5, Packing::Unpacked16);
    bad(&|l| l.virtual_channel = 4, Packing::Unpacked16);
    bad(&|l| l.data_type = 0, Packing::Unpacked16);
    bad(&|l| l.data_type = 64, Packing::Unpacked16);
    bad(&|l| l.link_rate_mbps = 79, Packing::Unpacked16);
    bad(&|l| l.link_rate_mbps = 2501, Packing::Unpacked16);
    bad(&|l| l.width = 31, Packing::Unpacked16);
    bad(&|l| l.width = 4801, Packing::Unpacked16);
    bad(&|l| l.height = 31, Packing::Unpacked16);
    bad(&|l| l.height = 4801, Packing::Unpacked16);
    bad(&|l| l.bit_depth = 9, Packing::Unpacked16);
    // The node writes RAW10 one word per pixel only, and RAW8 packed only.
    bad(&|_| {}, Packing::Packed);
    bad(&|l| l.bit_depth = 8, Packing::Unpacked16);
    bad(&|l| l.bit_depth = 16, Packing::Unpacked16);
}

#[test]
fn a_monochrome_sensor_deeper_than_eight_bits_names_an_order() {
    let handle = Handle::default();
    let hw = Hw {
        node: Arc::clone(&handle.0),
        planes: Vec::new(),
    };
    let mono = Options::default();
    let mut d = SunxiVin::with_hardware(Box::new(hw), Box::new(MockStream::new(1)), mono);
    assert_eq!(
        d.configure(raw10(), Packing::Unpacked16).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    let raw8 = LinkConfig {
        bit_depth: 8,
        ..raw10()
    };
    d.configure(raw8, Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(handle.with(|n| n.opened[0].fourcc), FourCc::GREY);
}

#[test]
fn the_pixel_format_carries_order_and_depth() {
    let code = |order, depth| fourcc(order, depth).map(FourCc::to_bytes);
    assert_eq!(code(BayerOrder::Rggb, 10), Some(*b"RG10"));
    assert_eq!(code(BayerOrder::Grbg, 10), Some(*b"BA10"));
    assert_eq!(code(BayerOrder::Grbg, 14), Some(*b"GR14"));
    assert_eq!(code(BayerOrder::Bggr, 16), Some(*b"BYR2"));
    assert_eq!(code(BayerOrder::Bggr, 8), Some(*b"BA81"));
    assert_eq!(code(BayerOrder::Mono, 8), Some(*b"GREY"));
    assert_eq!(code(BayerOrder::Mono, 10), None);
    assert_eq!(code(BayerOrder::Rggb, 9), None);
    assert_eq!(parse_order("gbrg"), Some(BayerOrder::Gbrg));
    assert_eq!(parse_order("GBRG"), None);
}

#[test]
fn start_requires_configure_and_asks_the_node_for_the_layout() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = driver(&stream);
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
    assert_eq!(
        node.with(|n| n.opened.clone()),
        vec![Wanted {
            input: 0,
            fourcc: FourCc::new(b"RG10"),
            width: 64,
            height: 32,
            stride: 128,
            size: 4096,
        }]
    );
    assert_eq!(node.with(|n| n.streamed.clone()), vec![4]);
    assert_eq!(node.with(|n| n.stopped), 1, "starts from nothing");
}

#[test]
fn a_node_that_answers_another_format_is_refused_and_closed() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = driver(&stream);
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    let asked = Granted {
        fourcc: FourCc::new(b"RG10"),
        width: 64,
        height: 32,
        stride: 128,
        size: 4096,
    };
    for answer in [
        Granted {
            fourcc: FourCc::new(b"BA10"),
            ..asked
        },
        Granted { width: 32, ..asked },
        Granted {
            stride: 256,
            ..asked
        },
        Granted {
            size: 4095,
            ..asked
        },
    ] {
        node.with(|n| n.answer = Some(answer));
        assert_eq!(d.start().unwrap_err(), Status::NOT_SUPPORTED);
    }
    assert!(node.with(|n| n.streamed.is_empty()));
    assert_eq!(node.with(|n| n.stopped), 8, "each closed again");
    // A node that pads the buffer past the frame is fine.
    node.with(|n| {
        n.answer = Some(Granted {
            size: 8192,
            ..asked
        })
    });
    d.start().unwrap();
}

#[test]
fn frames_come_out_of_the_stream_with_the_nodes_bytes_in_order() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = streaming(&stream);
    let sent: Vec<Vec<u8>> = (0..10u8).map(|seed| pattern(seed, FRAME)).collect();
    let mut now = 0;
    for (i, bytes) in sent.iter().enumerate() {
        node.fill(i as u32 % 4, 100 + i as u32, &[bytes]);
        now = d.tick(now).unwrap();
        // The consumer takes the frame, checks it and gives the buffer back.
        let frame = stream.take_frame().expect("published");
        assert_eq!(frame.bytes as usize, FRAME);
        assert_eq!(frame.sequence, 100 + i as u64, "in order");
        assert_eq!(frame.timestamp_ns, (100 + i as u64) * 1000);
        assert_eq!(frame.identity, 0);
        assert!(
            stream.contents(frame.buffer) == *bytes,
            "frame {i} arrived with other bytes"
        );
        stream.return_buffer(frame.buffer);
    }
    assert_eq!(
        node.with(|n| n.requeued.clone()),
        (0..10).map(|i| i % 4).collect::<Vec<_>>()
    );
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            frames: 10,
            dropped: 0,
            errors: 0
        }
    );
}

#[test]
fn a_frames_planes_are_copied_back_to_back() {
    let stream = MockStream::with_bytes(2, 12);
    let (mut d, node) = streaming(&stream);
    node.fill(0, 0, &[&[1, 2, 3, 4, 5, 6, 7, 8], &[9, 10], &[11, 12]]);
    d.tick(0).unwrap();
    let frame = stream.take_frame().unwrap();
    assert_eq!(frame.bytes, 12);
    assert_eq!(
        stream.contents(frame.buffer),
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );
}

#[test]
fn a_stalled_consumer_does_not_stall_capture() {
    // Two stream buffers, and a consumer that never gives one back.
    let stream = MockStream::with_bytes(2, FRAME);
    let (mut d, node) = streaming(&stream);
    let mut now = 0;
    for i in 0..20u32 {
        node.fill(i % 4, i, &[&pattern(i as u8, FRAME)]);
        now = d.tick(now).unwrap();
    }
    assert_eq!(
        node.with(|n| n.requeued.len()),
        20,
        "every buffer went back"
    );
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            frames: 2,
            dropped: 18,
            errors: 0
        }
    );
    // The two it got are the first two, intact.
    for i in 0..2u8 {
        let frame = stream.take_frame().unwrap();
        assert_eq!(stream.contents(frame.buffer), pattern(i, FRAME));
    }
    assert!(stream.take_frame().is_none());
}

/// A stream with no consumer attached: every write has nowhere to land.
#[derive(Clone, Default)]
struct Unattached(Arc<Mutex<(u32, Vec<StreamFrame>)>>);

impl StreamEndpoint for Unattached {
    type Error = ErrorKind;
    fn buffer_count(&self) -> u32 {
        2
    }
    fn publish(&mut self, frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        self.0.lock().unwrap().1.push(frame);
        Ok(())
    }
    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        let mut s = self.0.lock().unwrap();
        s.0 += 1;
        Ok(Some(s.0 % 2))
    }
    fn next_job(&mut self, _out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        Ok(None)
    }
    fn write(
        &mut self,
        _buffer: u32,
        _offset: usize,
        _data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        Ok(false)
    }
    fn read(
        &mut self,
        _buffer: u32,
        _offset: usize,
        _out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        Ok(false)
    }
    fn buffer_handle(
        &self,
        _buffer: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        Ok(None)
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
        Ok(())
    }
}

#[test]
fn with_no_consumer_frames_are_dropped_and_buffers_go_back() {
    let stream = Unattached::default();
    let (mut d, node) = driver_on(Box::new(stream.clone()));
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
    let mut now = 0;
    for i in 0..6u32 {
        node.fill(i % 4, i, &[&pattern(0, FRAME)]);
        now = d.tick(now).unwrap();
    }
    assert_eq!(node.with(|n| n.requeued.len()), 6);
    assert_eq!(d.counters().unwrap().dropped, 6);
    assert_eq!(d.counters().unwrap().frames, 0);
    // Each stream buffer was handed back empty, so the stream never runs out.
    let given_back = stream.0.lock().unwrap().1.clone();
    assert_eq!(given_back.len(), 6);
    assert!(given_back.iter().all(|f| f.bytes == 0));
}

#[test]
fn a_bad_frame_or_one_too_big_is_counted_and_the_buffer_reused() {
    let stream = MockStream::with_bytes(1, FRAME);
    let (mut d, node) = streaming(&stream);
    node.with(|n| n.filled.push_back((0, 0, true, vec![pattern(0, FRAME)])));
    node.fill(1, 1, &[&pattern(1, FRAME + 1)]);
    node.fill(2, 2, &[&pattern(2, FRAME)]);
    d.tick(0).unwrap();
    assert_eq!(node.with(|n| n.requeued.clone()), vec![0, 1, 2]);
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            frames: 1,
            dropped: 0,
            errors: 2
        }
    );
    // The only stream buffer, kept from the frame that didn't fit, carries the next one.
    let frame = stream.take_frame().unwrap();
    assert_eq!(frame.sequence, 2);
    assert_eq!(stream.contents(frame.buffer), pattern(2, FRAME));
}

#[test]
fn sequence_numbers_widen_across_a_wrap() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = streaming(&stream);
    for (i, sequence) in [u32::MAX - 1, u32::MAX, 0, 1].into_iter().enumerate() {
        node.fill(i as u32, sequence, &[&pattern(0, FRAME)]);
    }
    d.tick(0).unwrap();
    let got: Vec<u64> = std::iter::from_fn(|| stream.take_frame())
        .map(|f| f.sequence)
        .collect();
    assert_eq!(
        got,
        [
            u64::from(u32::MAX) - 1,
            u64::from(u32::MAX),
            1 << 32,
            (1 << 32) + 1
        ]
    );
}

#[test]
fn a_tick_takes_at_most_one_round_of_the_ring() {
    let stream = MockStream::with_bytes(8, FRAME);
    let (mut d, node) = streaming(&stream);
    for i in 0..6u32 {
        node.fill(i % 4, i, &[&pattern(0, FRAME)]);
    }
    let at = d.tick(5).unwrap();
    assert_eq!(at, 5 + 1_000_000, "polled again in 1 ms");
    assert_eq!(node.with(|n| n.requeued.len()), 4);
    d.tick(at).unwrap();
    assert_eq!(node.with(|n| n.requeued.len()), 6);
}

#[test]
fn a_node_error_is_retried_with_capped_backoff() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = streaming(&stream);
    assert_eq!(d.health(), Health::Ok);

    let mut now = 0u64;
    now = d.tick(now).unwrap();
    node.with(|n| n.fail_dequeue = Some(Status::NO_DEVICE));
    // It failed: recovery starts, with its first delay.
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 250_000_000);
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.start().unwrap_err(), Status::BUSY, "while recovering");

    node.with(|n| n.fail_open = Some(Status::NO_DEVICE));
    let expected = [
        500_000_000u64,
        1_000_000_000,
        2_000_000_000,
        4_000_000_000,
        8_000_000_000,
        8_000_000_000,
        8_000_000_000,
    ];
    now = at;
    for delay in expected {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, delay, "backoff capped at 8 s");
        now = at;
    }
    // Forever: it never gives up and never turns itself off.
    for _ in 0..100 {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, 8_000_000_000);
        now = at;
        assert!(matches!(d.health(), Health::Degraded(_)));
    }

    // And it clears when an attempt works: the node opened and streaming again, frames flowing.
    node.with(|n| n.fail_open = None);
    let opened = node.with(|n| n.opened.len());
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 1_000_000);
    assert_eq!(d.health(), Health::Ok);
    assert_eq!(node.with(|n| n.opened.len()), opened + 1);
    assert_eq!(node.with(|n| n.streamed.len()), 2);
    node.fill(0, 0, &[&pattern(9, FRAME)]);
    d.tick(at).unwrap();
    let frame = stream.take_frame().unwrap();
    assert_eq!(stream.contents(frame.buffer), pattern(9, FRAME));
}

#[test]
fn stop_ends_recovery() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = streaming(&stream);
    node.with(|n| n.fail_dequeue = Some(Status::IO));
    let _ = d.tick(0);
    assert!(matches!(d.health(), Health::Degraded(_)));
    d.stop().unwrap();
    assert_eq!(d.tick(u64::MAX), None, "stopped: nothing is due");
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn a_failed_start_is_reported_and_retried_by_its_caller() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = driver(&stream);
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    node.with(|n| n.fail_open = Some(Status::NO_DEVICE));
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
    assert_eq!(
        d.tick(0),
        None,
        "no background retries of a start not begun"
    );
    assert_eq!(d.health(), Health::Ok);
    node.with(|n| n.fail_open = None);
    d.start().unwrap();
}

#[test]
fn a_stream_cannot_be_configured_or_started_twice() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, _node) = streaming(&stream);
    assert_eq!(
        d.configure(raw10(), Packing::Unpacked16).unwrap_err(),
        Status::BUSY
    );
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    d.stop().unwrap();
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
}

#[test]
fn frame_stream_gives_the_streams_name_or_refuses_a_short_buffer() {
    let (mut d, _node) = driver(&MockStream::new(1));
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = d.frame_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"frames");
    let mut short = [0u8; 2];
    assert_eq!(
        d.frame_stream(&mut short).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
}

#[test]
fn counters_start_again_at_each_start() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = streaming(&stream);
    node.fill(0, 0, &[&pattern(0, FRAME)]);
    d.tick(0).unwrap();
    assert_eq!(d.counters().unwrap().frames, 1);
    d.stop().unwrap();
    d.start().unwrap();
    assert_eq!(d.counters().unwrap(), Counters::default());
}

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(1), 500_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(100), 8_000_000_000);
}

#[test]
fn the_steady_state_allocates_nothing() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, node) = streaming(&stream);
    // Everything the stand-ins hold is made before measuring.
    let frames: Vec<_> = (0..1000u32)
        .map(|i| (i % 4, i, false, vec![pattern(i as u8, FRAME)]))
        .collect();
    node.with(|n| n.requeued.reserve(2000));
    let mut now = d.tick(0).unwrap();
    // One round first, so the mocked stream's queues have grown to the ring.
    for _ in 0..4 {
        node.with(|n| n.filled.push_back((0, 0, false, vec![pattern(0, FRAME)])));
        now = d.tick(now).unwrap();
        let frame = stream.take_frame().unwrap();
        stream.return_buffer(frame.buffer);
    }
    node.with(|n| n.filled.reserve(1000));
    let mut frames = frames.into_iter();
    let ((), allocated) = allocations(|| {
        for frame in &mut frames {
            // The node fills one buffer per poll.
            node.with(|n| n.filled.push_back(frame));
            now = d.tick(now).unwrap();
            let frame = stream.take_frame().unwrap();
            stream.return_buffer(frame.buffer);
            let counters = d.counters().unwrap();
            assert_eq!(counters.dropped, 0);
        }
    });
    assert_eq!(allocated, 0, "dequeue, copy, publish, requeue and the poll");
    assert_eq!(d.counters().unwrap().frames, 1004);
}

/// The graph the kernel driver builds for capture node 0 after its input is selected, cut to
/// what matters: sensor, CSI-2 receiver, parser, ISP, scaler, capture subdevice and video node,
/// linked in that order, with a second parser and ISP wired but off.
fn vin_graph() -> Topology {
    let entities = [
        (1, "sensor 3-0010", EntityFunction::CAM_SENSOR),
        (2, "sunxi_mipi.0", EntityFunction::IO_V4L),
        (3, "sunxi_csi.0", EntityFunction::IO_V4L),
        (4, "sunxi_isp.0", EntityFunction::PROC_VIDEO_PIXEL_FORMATTER),
        (5, "sunxi_scaler.0", EntityFunction::PROC_VIDEO_SCALER),
        (6, "vin_cap.0", EntityFunction::IO_V4L),
        (7, "vin_video0", EntityFunction::IO_V4L),
        (8, "sunxi_csi.1", EntityFunction::IO_V4L),
        (9, "sunxi_isp.1", EntityFunction::PROC_VIDEO_PIXEL_FORMATTER),
    ];
    // Each entity has a sink pad (id 100 + n) and a source pad (id 200 + n).
    let mut pads = Vec::new();
    for (id, ..) in entities {
        pads.push(Pad {
            id: 100 + id,
            entity_id: id,
            flags: PadFlags::SINK,
            index: 0,
        });
        pads.push(Pad {
            id: 200 + id,
            entity_id: id,
            flags: PadFlags::SOURCE,
            index: 1,
        });
    }
    let on = LinkFlags::ENABLED;
    let off = LinkFlags::empty();
    let links = [
        (1, 2, on),
        (2, 3, on),
        (8, 4, off), // the other parser into this ISP: wired, off
        (3, 9, off), // this parser into the other ISP: wired, off
        (3, 4, on),
        (4, 5, on),
        (9, 5, off),
        (5, 6, on),
        (6, 7, on),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (from, to, flags))| Link {
        id: 300 + i as u32,
        source_id: 200 + from,
        sink_id: 100 + to,
        flags,
    })
    .collect();
    Topology {
        version: 1,
        entities: entities
            .into_iter()
            .map(|(id, name, function)| Entity {
                id,
                name: name.into(),
                function,
                flags: EntityFlags::empty(),
            })
            .collect(),
        interfaces: Vec::new(),
        pads,
        links,
    }
}

fn names(sensor: Option<&str>, receiver: Option<&str>, isp: Option<&str>) -> Feeds {
    Feeds {
        sensor: sensor.map(Into::into),
        receiver: receiver.map(Into::into),
        isp: isp.map(Into::into),
    }
}

#[test]
fn feeds_follows_the_enabled_links_up_from_the_node() {
    assert_eq!(
        feeds(&vin_graph(), 7),
        names(
            Some("sensor 3-0010"),
            Some("sunxi_mipi.0"),
            Some("sunxi_isp.0")
        )
    );
}

#[test]
fn feeds_takes_the_isp_whose_link_is_on() {
    let mut graph = vin_graph();
    // The node's ISP fed by the other parser, through the other receiver.
    for link in &mut graph.links {
        match (link.source_id, link.sink_id) {
            (203, 104) => link.flags = LinkFlags::empty(),
            (208, 104) => link.flags = LinkFlags::ENABLED,
            _ => {}
        }
    }
    assert_eq!(
        feeds(&graph, 7),
        names(None, Some("sunxi_csi.1"), Some("sunxi_isp.0"))
    );
}

#[test]
fn feeds_with_the_input_not_selected_stops_at_the_isp() {
    let mut graph = vin_graph();
    for link in &mut graph.links {
        if (link.source_id, link.sink_id) == (203, 104) {
            link.flags = LinkFlags::empty();
        }
    }
    assert_eq!(feeds(&graph, 7), names(None, None, Some("sunxi_isp.0")));
}

#[test]
fn feeds_ends_on_a_loop_and_on_an_unknown_node() {
    let mut graph = vin_graph();
    // The ISP feeds the receiver instead of the sensor doing it: a loop with no end.
    for link in &mut graph.links {
        if (link.source_id, link.sink_id) == (201, 102) {
            link.flags = LinkFlags::empty();
        }
    }
    graph.links.push(Link {
        id: 999,
        source_id: 204,
        sink_id: 102,
        flags: LinkFlags::ENABLED,
    });
    let got = feeds(&graph, 7);
    assert_eq!(got.isp.as_deref(), Some("sunxi_isp.0"));
    assert_eq!(got.sensor, None);
    assert_eq!(feeds(&vin_graph(), 42), Feeds::default());
}

#[test]
fn the_driver_answers_feeds_from_the_nodes_graph() {
    let (mut d, node) = driver(&MockStream::new(1));
    assert_eq!(d.feeds().unwrap_err(), Status::NO_DEVICE);
    node.with(|n| {
        n.graph = Some(Graph {
            topology: vin_graph(),
            capture: 7,
        });
    });
    assert_eq!(d.feeds().unwrap().isp.as_deref(), Some("sunxi_isp.0"));
    assert_eq!(d.feeds().unwrap().receiver.as_deref(), Some("sunxi_mipi.0"));
}
