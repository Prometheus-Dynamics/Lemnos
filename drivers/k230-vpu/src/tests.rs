//! The claim against a node that plays the `mvx` driver back: the formats it keeps, the buffers
//! it is handed for each queue, the completions it gives, and its failures.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::rc::Rc;
use std::time::Duration;

use lemnos_driver_m2m_decode::{
    BufType, Capability, Dequeued, Event, Format, Memory, Node, PixFormatMplane, PlaneFormat,
    PlaneMap, PlaneMemory, Queued, Ready, Rect,
};
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::poll::{POLLIN, POLLOUT};
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    BUF_FLAG_ERROR, BUF_FLAG_LAST, BufferInfo, CAP_DEVICE_CAPS, CAP_STREAMING,
    CAP_VIDEO_M2M_MPLANE, EVENT_SOURCE_CHANGE, MAX_PLANES as V4L2_PLANES, PlaneInfo,
    SRC_CH_RESOLUTION,
};
use lemnos_measure::test_alloc::allocations;

use crate::uapi::{DRIVER, MAX_PLANES, control};
use crate::*;

/// One plane as the kernel was handed it: descriptor, length, bytes used, data offset.
type Plane = (i32, u32, u32, u32);

/// One queued buffer.
#[derive(Clone, Copy, Debug)]
struct Queue {
    index: u32,
    planes: [Plane; MAX_PLANES],
    count: usize,
}

/// What the played-back driver does and saw. Index 0 is the output (source) queue, 1 the
/// capture (destination) queue.
struct Driver {
    driver: &'static str,
    /// The pixel format the driver keeps on the output queue instead of the one asked for.
    keeps: Option<u32>,
    formats: [Option<Format>; 2],
    /// Buffers granted per queue at most.
    grant: u32,
    granted: [u32; 2],
    /// Everything queued, in order.
    queued: [Vec<Queue>; 2],
    /// Held by the device, oldest first.
    held: [VecDeque<Queue>; 2],
    /// Ready to dequeue.
    done: [VecDeque<Dequeued>; 2],
    events: VecDeque<Event>,
    controls: Vec<(u32, i32)>,
    /// A control id the kernel refuses as invalid.
    bad_control: Option<u32>,
    stream_ons: Vec<BufType>,
    stream_offs: usize,
    stops: [usize; 2],
    refuse_queue: bool,
}

impl Default for Driver {
    fn default() -> Self {
        Self {
            driver: DRIVER,
            keeps: None,
            formats: [None, None],
            grant: 4,
            granted: [0, 0],
            queued: [Vec::with_capacity(256), Vec::with_capacity(256)],
            held: [VecDeque::with_capacity(64), VecDeque::with_capacity(64)],
            done: [VecDeque::with_capacity(64), VecDeque::with_capacity(64)],
            events: VecDeque::with_capacity(8),
            controls: Vec::with_capacity(8),
            bad_control: None,
            stream_ons: Vec::with_capacity(8),
            stream_offs: 0,
            stops: [0, 0],
            refuse_queue: false,
        }
    }
}

#[derive(Clone, Default)]
struct Mock(Rc<RefCell<Driver>>);

impl Mock {
    fn with<T>(&self, f: impl FnOnce(&mut Driver) -> T) -> T {
        f(&mut self.0.borrow_mut())
    }

    /// The device finishes the oldest buffer it holds on `side`, reporting `bytes` per plane.
    fn finish(&self, side: usize, flags: u32, bytes: [u32; MAX_PLANES]) {
        self.with(|d| {
            let q = d.held[side].pop_front().expect("a held buffer");
            let mut bytes_used = [0; V4L2_PLANES];
            bytes_used[..MAX_PLANES].copy_from_slice(&bytes);
            d.done[side].push_back(Dequeued {
                index: q.index,
                flags,
                field: 1,
                timestamp: Duration::from_millis(u64::from(q.index)),
                sequence: q.index,
                bytes_used,
                data_offset: [0; V4L2_PLANES],
                num_planes: q.count as u32,
            });
        });
    }
}

struct MockNode(Mock);

struct NoMap;

impl PlaneMap for NoMap {
    fn bytes(&self) -> &[u8] {
        &[]
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut []
    }
}

fn side(buf_type: BufType) -> usize {
    usize::from(!buf_type.is_output())
}

fn enotty() -> io::Error {
    io::Error::from_raw_os_error(25)
}

fn einval() -> io::Error {
    io::Error::from_raw_os_error(22)
}

impl Node for MockNode {
    type Request = ();
    type Map = NoMap;

    fn capability(&self) -> io::Result<Capability> {
        let driver = self.0.with(|d| d.driver);
        Ok(Capability {
            driver: driver.into(),
            card: crate::uapi::CARD.into(),
            bus_info: "platform:mvx".into(),
            version: 0,
            capabilities: CAP_VIDEO_M2M_MPLANE | CAP_STREAMING | CAP_DEVICE_CAPS,
            device_caps: CAP_VIDEO_M2M_MPLANE | CAP_STREAMING,
        })
    }

    fn set_format(&self, buf_type: BufType, format: &Format) -> io::Result<Format> {
        Ok(self.0.with(|d| {
            let mut kept = *format;
            if let (Format::Multi(m), Some(code), true) = (&mut kept, d.keeps, buf_type.is_output())
            {
                m.pixelformat = code;
            }
            d.formats[side(buf_type)] = Some(kept);
            kept
        }))
    }

    fn format(&self, buf_type: BufType) -> io::Result<Format> {
        self.0
            .with(|d| d.formats[side(buf_type)])
            .ok_or_else(einval)
    }

    fn set_selection(&self, _: BufType, _: u32, _: Rect) -> io::Result<Rect> {
        Err(enotty())
    }

    fn selection(&self, _: BufType, _: u32) -> io::Result<Rect> {
        Err(enotty())
    }

    fn request_buffers(&self, buf_type: BufType, _: Memory, count: u32) -> io::Result<u32> {
        Ok(self.0.with(|d| {
            let granted = count.min(d.grant);
            d.granted[side(buf_type)] = granted;
            d.held[side(buf_type)].clear();
            granted
        }))
    }

    fn query_buffer(&self, _: BufType, _: u32, _: u32) -> io::Result<BufferInfo> {
        Err(enotty())
    }

    fn map(&self, _: &PlaneInfo) -> io::Result<NoMap> {
        Err(enotty())
    }

    fn queue(&self, buffer: &Queued<'_, ()>) -> io::Result<()> {
        self.0.with(|d| {
            if d.refuse_queue {
                return Err(einval());
            }
            let mut planes = [(0, 0, 0, 0); MAX_PLANES];
            for (to, p) in planes.iter_mut().zip(buffer.planes) {
                let fd = match p.memory {
                    PlaneMemory::Dmabuf(fd) => fd.as_raw_fd(),
                    PlaneMemory::Mmap => -1,
                };
                *to = (fd, p.length, p.bytes_used, p.data_offset);
            }
            let q = Queue {
                index: buffer.index,
                planes,
                count: buffer.planes.len(),
            };
            let s = side(buffer.buf_type);
            d.queued[s].push(q);
            d.held[s].push_back(q);
            Ok(())
        })
    }

    fn dequeue(&self, buf_type: BufType, _: Memory, _: u32) -> io::Result<Option<Dequeued>> {
        Ok(self.0.with(|d| d.done[side(buf_type)].pop_front()))
    }

    fn stream_on(&self, buf_type: BufType) -> io::Result<()> {
        self.0.with(|d| d.stream_ons.push(buf_type));
        Ok(())
    }

    fn stream_off(&self, buf_type: BufType) -> io::Result<()> {
        self.0.with(|d| {
            d.stream_offs += 1;
            d.held[side(buf_type)].clear();
        });
        Ok(())
    }

    fn stop_encoder(&self) -> io::Result<()> {
        self.0.with(|d| d.stops[1] += 1);
        Ok(())
    }

    fn stop_decoder(&self) -> io::Result<()> {
        self.0.with(|d| d.stops[0] += 1);
        Ok(())
    }

    fn subscribe(&self, _: u32, _: u32) -> io::Result<()> {
        Ok(())
    }

    fn event(&self) -> io::Result<Option<Event>> {
        Ok(self.0.with(|d| d.events.pop_front()))
    }

    fn set_control(&self, id: u32, value: i32) -> io::Result<i32> {
        self.0.with(|d| {
            if d.bad_control == Some(id) {
                return Err(einval());
            }
            d.controls.push((id, value));
            Ok(value)
        })
    }

    fn control(&self, _: u32) -> io::Result<i32> {
        Err(enotty())
    }

    fn poll(&self, events: i16, _: Option<Duration>) -> io::Result<i16> {
        let ready = self.0.with(|d| {
            let mut ready = 0;
            if !d.done[0].is_empty() {
                ready |= POLLOUT;
            }
            if !d.done[1].is_empty() {
                ready |= POLLIN;
            }
            ready
        });
        Ok(ready & events)
    }

    fn new_request(&self) -> io::Result<()> {
        Err(enotty())
    }

    fn set_request_controls(&self, _: &(), _: &mut [ExtControl<'_>]) -> io::Result<()> {
        Err(enotty())
    }

    fn queue_request(&self, _: &()) -> io::Result<()> {
        Err(enotty())
    }

    fn reinit_request(&self, _: &()) -> io::Result<()> {
        Err(enotty())
    }

    fn wait_request(&self, _: &(), _: Option<Duration>) -> io::Result<bool> {
        Err(enotty())
    }
}

/// The system: opens a played-back node, or fails to; measures buffers by their real size.
struct MockDevice {
    mock: Mock,
    opens: u32,
    /// Fail this many opens first.
    fail_opens: u32,
    /// Answer this size for every buffer instead of its own.
    size: Option<u64>,
}

impl Device for MockDevice {
    type Node = MockNode;

    fn open(&mut self) -> io::Result<MockNode> {
        self.opens += 1;
        if self.fail_opens > 0 {
            self.fail_opens -= 1;
            return Err(io::Error::from_raw_os_error(16));
        }
        Ok(MockNode(self.mock.clone()))
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        match self.size {
            Some(size) => Ok(size),
            None => dma_buf::size(fd),
        }
    }
}

fn vpu(mock: &Mock) -> Vpu<MockDevice> {
    Vpu::new(MockDevice {
        mock: mock.clone(),
        opens: 0,
        fail_opens: 0,
        size: None,
    })
}

/// H.264 is `H264`; NV12 in two memory planes is `NM12`.
const H264: u32 = u32::from_le_bytes(*b"H264");
const NV12M: u32 = u32::from_le_bytes(*b"NM12");

fn format(pixelformat: u32, sizes: &[u32]) -> PixFormatMplane {
    let mut planes = [PlaneFormat::default(); V4L2_PLANES];
    for (to, &size) in planes.iter_mut().zip(sizes) {
        *to = PlaneFormat {
            size_image: size,
            bytes_per_line: 0,
        };
    }
    PixFormatMplane {
        width: 64,
        height: 32,
        pixelformat,
        field: 1,
        colorspace: 0,
        planes,
        num_planes: sizes.len() as u32,
    }
}

/// A decoder: 4096-byte bitstream buffers in, NV12 frames of 2048 + 1024 bytes out.
fn decode() -> Setup<'static> {
    Setup {
        direction: Direction::Decode,
        source: format(H264, &[4096]),
        destination: format(NV12M, &[2048, 1024]),
        sources: 2,
        destinations: 2,
        controls: &[],
    }
}

fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"k230-vpu-test", len).expect("memfd")
}

fn source(planes: &[Payload]) -> Source<'_> {
    Source {
        identity: 7,
        planes,
        timestamp: Duration::from_millis(3),
    }
}

fn job_error(e: Result<(), SubmitError>) -> JobError {
    match e {
        Err(SubmitError::Job(e)) => e,
        other => panic!("expected a job error, got {other:?}"),
    }
}

#[test]
fn the_backoff_doubles_from_250_ms_to_8_s() {
    let ms: Vec<u64> = (0..9).map(|n| backoff_ns(n) / 1_000_000).collect();
    assert_eq!(ms, [250, 250, 500, 1000, 2000, 4000, 8000, 8000, 8000]);
}

#[test]
fn a_bad_setup_is_refused_before_the_device_is_opened() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let refused = |vpu: &mut Vpu<MockDevice>, setup: Setup<'_>| match vpu.claim(&setup, 0) {
        Err(ClaimError::Setup(e)) => e,
        other => panic!("expected a setup error, got {:?}", other.err()),
    };

    let mut s = decode();
    s.source.num_planes = 0;
    assert_eq!(
        refused(&mut vpu, s),
        SetupError::Planes {
            source: true,
            planes: 0
        }
    );
    let mut s = decode();
    s.destination.num_planes = 4;
    assert_eq!(
        refused(&mut vpu, s),
        SetupError::Planes {
            source: false,
            planes: 4
        }
    );
    for (sources, destinations, direction) in [
        (0, 2, Direction::Decode),
        (65, 2, Direction::Decode),
        (2, 65, Direction::Decode),
        (2, 0, Direction::Encode),
    ] {
        let s = Setup {
            sources,
            destinations,
            direction,
            ..decode()
        };
        assert_eq!(refused(&mut vpu, s), SetupError::Buffers);
    }
    for id in REFUSED_CONTROLS {
        let controls = [Control { id, value: 1 }];
        let s = Setup {
            controls: &controls,
            ..decode()
        };
        assert_eq!(refused(&mut vpu, s), SetupError::Control { id });
    }
    assert_eq!(vpu.device().opens, 0);
}

#[test]
fn a_claim_sets_both_formats_and_the_callers_controls() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let controls = [
        Control {
            id: control::FRAME_RATE,
            value: 30 << 16,
        },
        Control {
            id: control::INTBUF_SIZE,
            value: 1 << 20,
        },
    ];
    let claim = vpu
        .claim(
            &Setup {
                controls: &controls,
                ..decode()
            },
            0,
        )
        .expect("claim");
    assert_eq!(claim.source_format().pixelformat, H264);
    assert_eq!(claim.destination_format().pixelformat, NV12M);
    assert_eq!((claim.source_slots(), claim.destination_slots()), (2, 2));
    drop(claim);
    mock.with(|d| {
        assert_eq!(
            d.controls,
            [
                (control::FRAME_RATE, 30 << 16),
                (control::INTBUF_SIZE, 1 << 20)
            ]
        );
        // Nothing streams until the first source.
        assert!(d.stream_ons.is_empty());
    });
}

#[test]
fn another_drivers_node_or_a_changed_format_is_refused_without_a_backoff() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    mock.with(|d| d.driver = "nonai-2d");
    assert!(matches!(
        vpu.claim(&decode(), 0),
        Err(ClaimError::NotTheBlock)
    ));
    mock.with(|d| {
        d.driver = DRIVER;
        d.keeps = Some(NV12M);
    });
    assert!(matches!(
        vpu.claim(&decode(), 0),
        Err(ClaimError::Refused(_))
    ));
    mock.with(|d| {
        d.keeps = None;
        d.bad_control = Some(control::FRAME_RATE);
    });
    let controls = [Control {
        id: control::FRAME_RATE,
        value: 1,
    }];
    let s = Setup {
        controls: &controls,
        ..decode()
    };
    assert!(matches!(vpu.claim(&s, 0), Err(ClaimError::Refused(_))));
    // No backoff: the next claim opens at once.
    mock.with(|d| d.bad_control = None);
    assert!(vpu.claim(&decode(), 0).is_ok());
}

#[test]
fn a_failed_open_backs_off_doubling_and_never_gives_up() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    vpu.device.fail_opens = 3;
    let mut now = 1_000;
    for want in [250_000_000, 500_000_000, 1_000_000_000] {
        match vpu.claim(&decode(), now) {
            Err(ClaimError::Device { retry_at_ns, .. }) => assert_eq!(retry_at_ns, now + want),
            other => panic!("expected a device error, got {:?}", other.err()),
        }
        let opens = vpu.device.opens;
        match vpu.claim(&decode(), now + want - 1) {
            Err(ClaimError::BackingOff { retry_at_ns }) => assert_eq!(retry_at_ns, now + want),
            other => panic!("expected a backoff, got {:?}", other.err()),
        }
        assert_eq!(vpu.device.opens, opens, "a backoff touches nothing");
        now += want;
    }
    assert!(vpu.claim(&decode(), now).is_ok());
    assert_eq!(vpu.failures, 0);
}

#[test]
fn buffers_are_measured_once_and_refused_when_empty_too_large_or_too_many() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    {
        let mut claim = vpu.claim(&decode(), 0).expect("claim");
        assert!(matches!(
            claim.add_buffer(buffer(0)),
            Err(BufferError::Size(None))
        ));
        let id = claim.add_buffer(buffer(4096)).expect("buffer");
        assert_eq!(claim.buffer_size(id), Some(4096));
        let fd = claim.remove_buffer(id).expect("removed");
        assert_eq!(dma_buf::size(std::os::fd::AsFd::as_fd(&fd)).unwrap(), 4096);
        // A stale name stays stale, even when the slot is reused.
        let again = claim.add_buffer(buffer(4096)).expect("buffer");
        assert_eq!(claim.buffer_size(id), None);
        assert!(matches!(claim.remove_buffer(id), Err(BufferError::Unknown)));
        claim.remove_buffer(again).expect("removed");
        for _ in 0..MAX_BUFFERS {
            claim.add_buffer(buffer(1)).expect("buffer");
        }
        assert!(matches!(
            claim.add_buffer(buffer(1)),
            Err(BufferError::Full)
        ));
    }
    vpu.device.size = Some(u64::from(u32::MAX) + 1);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    assert!(matches!(
        claim.add_buffer(buffer(1)),
        Err(BufferError::TooLarge)
    ));
}

#[test]
fn a_source_is_queued_with_the_buffers_real_size_and_no_offset() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let fd = buffer(8192);
    let raw = fd.as_raw_fd();
    let b = claim.add_buffer(fd).expect("buffer");
    claim
        .submit(&source(&[Payload {
            buffer: b,
            bytes: 8192,
        }]))
        .expect("submitted");
    let (y, c) = (buffer(2048), buffer(1024));
    let (y_raw, c_raw) = (y.as_raw_fd(), c.as_raw_fd());
    let (y, c) = (claim.add_buffer(y).unwrap(), claim.add_buffer(c).unwrap());
    claim
        .give(&Destination {
            identity: 9,
            planes: &[y, c],
        })
        .expect("given");
    drop(claim);
    mock.with(|d| {
        let q = d.queued[0][0];
        assert_eq!(q.count, 1);
        assert_eq!(q.planes[0], (raw, 8192, 8192, 0));
        let q = d.queued[1][0];
        assert_eq!(q.count, 2);
        assert_eq!(q.planes[0], (y_raw, 2048, 0, 0));
        assert_eq!(q.planes[1], (c_raw, 1024, 0, 0));
        // The first source started both queues, the destination first.
        assert_eq!(
            d.stream_ons,
            [BufType::VideoCaptureMplane, BufType::VideoOutputMplane]
        );
    });
}

#[test]
fn a_payload_must_be_inside_its_buffer_and_not_empty() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    for bytes in [0, 4097, u32::MAX] {
        assert_eq!(
            job_error(claim.submit(&source(&[Payload { buffer: b, bytes }]))),
            JobError::Payload {
                plane: 0,
                bytes,
                size: 4096
            }
        );
    }
    claim
        .submit(&source(&[Payload {
            buffer: b,
            bytes: 4096,
        }]))
        .expect("the whole buffer");
    drop(claim);
    assert_eq!(mock.with(|d| d.queued[0].len()), 1);
}

#[test]
fn a_buffer_smaller_than_its_plane_is_refused_on_either_side() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let small = claim.add_buffer(buffer(4095)).expect("buffer");
    assert_eq!(
        job_error(claim.submit(&source(&[Payload {
            buffer: small,
            bytes: 1
        }]))),
        JobError::TooSmall {
            plane: 0,
            size: 4095,
            need: 4096
        }
    );
    let y = claim.add_buffer(buffer(2048)).expect("buffer");
    let c = claim.add_buffer(buffer(1023)).expect("buffer");
    assert_eq!(
        job_error(claim.give(&Destination {
            identity: 1,
            planes: &[y, c]
        })),
        JobError::TooSmall {
            plane: 1,
            size: 1023,
            need: 1024
        }
    );
    drop(claim);
    mock.with(|d| assert!(d.queued[0].is_empty() && d.queued[1].is_empty()));
}

#[test]
fn every_plane_names_a_held_buffer_once_and_the_planes_match_the_format() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let a = claim.add_buffer(buffer(4096)).expect("buffer");
    let gone = claim.add_buffer(buffer(4096)).expect("buffer");
    claim.remove_buffer(gone).expect("removed");
    let p = |buffer| Payload { buffer, bytes: 1 };

    assert_eq!(
        job_error(claim.submit(&source(&[]))),
        JobError::Planes { want: 1, got: 0 }
    );
    assert_eq!(
        job_error(claim.submit(&source(&[p(a), p(a)]))),
        JobError::Planes { want: 1, got: 2 }
    );
    assert_eq!(
        job_error(claim.submit(&source(&[p(gone)]))),
        JobError::UnknownBuffer { plane: 0 }
    );
    let forged = BufferId {
        slot: 200,
        generation: 1,
    };
    assert_eq!(
        job_error(claim.submit(&source(&[p(forged)]))),
        JobError::UnknownBuffer { plane: 0 }
    );
    // One buffer for both planes of a destination.
    let big = claim.add_buffer(buffer(4096)).expect("buffer");
    assert_eq!(
        job_error(claim.give(&Destination {
            identity: 1,
            planes: &[big, big]
        })),
        JobError::Busy { plane: 1 }
    );
    // A buffer the block holds can be named nowhere else until it comes back.
    claim.submit(&source(&[p(a)])).expect("submitted");
    assert_eq!(
        job_error(claim.submit(&source(&[p(a)]))),
        JobError::Busy { plane: 0 }
    );
    let c = claim.add_buffer(buffer(1024)).expect("buffer");
    assert_eq!(
        job_error(claim.give(&Destination {
            identity: 1,
            planes: &[a, c]
        })),
        JobError::Busy { plane: 0 }
    );
    assert!(matches!(claim.remove_buffer(a), Err(BufferError::Busy)));
    drop(claim);
    assert_eq!(mock.with(|d| d.queued[0].len() + d.queued[1].len()), 1);
}

#[test]
fn a_queue_the_block_holds_entirely_is_full() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    for _ in 0..2 {
        let b = claim.add_buffer(buffer(4096)).expect("buffer");
        claim
            .submit(&source(&[Payload {
                buffer: b,
                bytes: 1,
            }]))
            .expect("submitted");
    }
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    assert!(matches!(
        claim.submit(&source(&[Payload {
            buffer: b,
            bytes: 1
        }])),
        Err(SubmitError::Full)
    ));
    drop(claim);
    mock.with(|d| {
        let indices: Vec<u32> = d.queued[0].iter().map(|q| q.index).collect();
        assert_eq!(indices, [0, 1]);
    });
}

#[test]
fn a_refused_queue_holds_nothing() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    mock.with(|d| d.refuse_queue = true);
    assert!(matches!(
        claim.submit(&source(&[Payload {
            buffer: b,
            bytes: 1
        }])),
        Err(SubmitError::Device(_))
    ));
    claim.remove_buffer(b).expect("not held");
}

#[test]
fn completions_carry_the_identity_and_give_the_buffers_back() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    let y = claim.add_buffer(buffer(2048)).expect("buffer");
    let c = claim.add_buffer(buffer(1024)).expect("buffer");
    claim
        .give(&Destination {
            identity: 11,
            planes: &[y, c],
        })
        .expect("given");
    claim
        .submit(&source(&[Payload {
            buffer: b,
            bytes: 9,
        }]))
        .expect("submitted");
    assert!(claim.complete_source().unwrap().is_none());
    assert_eq!(
        claim
            .wait(
                Ready {
                    output: true,
                    capture: true,
                    event: false
                },
                None
            )
            .unwrap(),
        Ready::default()
    );

    mock.finish(0, 0, [0; MAX_PLANES]);
    mock.finish(1, BUF_FLAG_LAST, [2048, 512, 0]);
    let ready = claim
        .wait(
            Ready {
                output: true,
                capture: true,
                event: false,
            },
            None,
        )
        .unwrap();
    assert!(ready.output && ready.capture);
    let done = claim.complete_source().unwrap().expect("a source");
    assert_eq!((done.identity, done.planes), (7, 1));
    claim.remove_buffer(b).expect("given back");
    assert!(matches!(claim.remove_buffer(y), Err(BufferError::Busy)));
    let done = claim
        .complete_destination()
        .unwrap()
        .expect("a destination");
    assert_eq!(done.identity, 11);
    assert_eq!(done.bytes, [2048, 512, 0]);
    assert_eq!(done.flags, BUF_FLAG_LAST);
    claim.remove_buffer(y).expect("given back");
    claim.remove_buffer(c).expect("given back");
}

#[test]
fn a_destination_the_kernel_reports_outside_its_buffers_completes_as_failed() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let y = claim.add_buffer(buffer(2048)).expect("buffer");
    let c = claim.add_buffer(buffer(1024)).expect("buffer");
    for bytes in [[2049, 0, 0], [0, 1025, 0], [u32::MAX, u32::MAX, 0]] {
        claim
            .give(&Destination {
                identity: 1,
                planes: &[y, c],
            })
            .expect("given");
        mock.finish(1, 0, bytes);
        let done = claim.complete_destination().unwrap().expect("done");
        assert_eq!(done.bytes, [0; MAX_PLANES], "{bytes:?}");
        assert_eq!(done.flags & BUF_FLAG_ERROR, BUF_FLAG_ERROR);
    }
    // An offset past the bytes, and a plane count other than the job's.
    for (offset, planes) in [(5, 2), (0, 1)] {
        claim
            .give(&Destination {
                identity: 1,
                planes: &[y, c],
            })
            .expect("given");
        mock.finish(1, 0, [4, 4, 0]);
        mock.with(|d| {
            let done = d.done[1].back_mut().unwrap();
            done.data_offset[0] = offset;
            done.num_planes = planes;
        });
        let done = claim.complete_destination().unwrap().expect("done");
        assert_eq!(done.bytes, [0; MAX_PLANES]);
        assert_eq!(done.flags & BUF_FLAG_ERROR, BUF_FLAG_ERROR);
    }
}

#[test]
fn a_buffer_the_kernel_returns_unasked_is_an_error() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    mock.with(|d| {
        d.done[0].push_back(Dequeued {
            index: 1,
            flags: 0,
            field: 1,
            timestamp: Duration::ZERO,
            sequence: 0,
            bytes_used: [0; V4L2_PLANES],
            data_offset: [0; V4L2_PLANES],
            num_planes: 1,
        });
    });
    let error = claim.complete_source().expect_err("refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_decoder_waits_for_the_streams_format_before_its_destinations() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let s = Setup {
        destinations: 0,
        ..decode()
    };
    let mut claim = vpu.claim(&s, 0).expect("claim");
    assert_eq!(claim.destination_slots(), 0);
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    let y = claim.add_buffer(buffer(2048)).expect("buffer");
    let c = claim.add_buffer(buffer(1024)).expect("buffer");
    let dest = Destination {
        identity: 2,
        planes: &[y, c],
    };
    assert!(matches!(claim.give(&dest), Err(SubmitError::Pending)));
    claim
        .submit(&source(&[Payload {
            buffer: b,
            bytes: 1,
        }]))
        .expect("submitted");
    mock.with(|d| {
        assert_eq!(d.stream_ons, [BufType::VideoOutputMplane]);
        d.events.push_back(Event {
            event_type: EVENT_SOURCE_CHANGE,
            id: 0,
            pending: 0,
            sequence: 0,
            changes: SRC_CH_RESOLUTION,
            timestamp: Duration::ZERO,
        });
    });
    let change = claim.source_change().unwrap().expect("a change");
    assert!(change.resolution());
    claim.adopt_destinations(3).expect("adopted");
    assert_eq!(claim.destination_slots(), 3);
    claim.give(&dest).expect("given");
    drop(claim);
    mock.with(|d| {
        assert_eq!(
            d.stream_ons,
            [BufType::VideoOutputMplane, BufType::VideoCaptureMplane]
        );
        assert_eq!(d.queued[1].len(), 1);
    });
}

#[test]
fn a_drain_stops_the_way_the_block_runs() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    vpu.claim(&decode(), 0).unwrap().drain().unwrap();
    let encode = Setup {
        direction: Direction::Encode,
        source: format(NV12M, &[2048, 1024]),
        destination: format(H264, &[4096]),
        ..decode()
    };
    vpu.claim(&encode, 0).unwrap().drain().unwrap();
    assert_eq!(mock.with(|d| d.stops), [1, 1]);
}

#[test]
fn stopping_gives_every_buffer_back() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    let y = claim.add_buffer(buffer(2048)).expect("buffer");
    let c = claim.add_buffer(buffer(1024)).expect("buffer");
    claim
        .give(&Destination {
            identity: 1,
            planes: &[y, c],
        })
        .unwrap();
    claim
        .submit(&source(&[Payload {
            buffer: b,
            bytes: 1,
        }]))
        .unwrap();
    claim.stop().unwrap();
    for id in [b, y, c] {
        claim.remove_buffer(id).expect("given back");
    }
}

#[test]
fn refused_controls_stay_refused_after_the_claim() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    assert!(matches!(
        claim.set_control(control::SECURE_VIDEO, 1),
        Err(ControlError::Refused)
    ));
    assert_eq!(claim.set_control(control::FRAME_RATE, 25).unwrap(), 25);
    drop(claim);
    assert_eq!(
        mock.with(|d| d.controls.clone()),
        [(control::FRAME_RATE, 25)]
    );
}

#[test]
fn submitting_and_completing_allocate_nothing() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let mut claim = vpu.claim(&decode(), 0).expect("claim");
    let b = claim.add_buffer(buffer(4096)).expect("buffer");
    let y = claim.add_buffer(buffer(2048)).expect("buffer");
    let c = claim.add_buffer(buffer(1024)).expect("buffer");
    let payload = [Payload {
        buffer: b,
        bytes: 100,
    }];
    let planes = [y, c];
    // Warm up: the first source starts the session.
    claim.submit(&source(&payload)).unwrap();
    mock.finish(0, 0, [0; MAX_PLANES]);
    claim.complete_source().unwrap().unwrap();
    let ((), n) = allocations(|| {
        for _ in 0..50 {
            claim
                .give(&Destination {
                    identity: 1,
                    planes: &planes,
                })
                .unwrap();
            claim.submit(&source(&payload)).unwrap();
            mock.finish(0, 0, [0; MAX_PLANES]);
            mock.finish(1, 0, [2048, 1024, 0]);
            let want = Ready {
                output: true,
                capture: true,
                event: false,
            };
            claim.wait(want, Some(Duration::ZERO)).unwrap();
            claim.complete_source().unwrap().unwrap();
            claim.complete_destination().unwrap().unwrap();
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn an_encoders_bitstream_reports_only_its_own_plane() {
    let mock = Mock::default();
    let mut vpu = vpu(&mock);
    let encode = Setup {
        direction: Direction::Encode,
        source: format(NV12M, &[2048, 1024]),
        destination: format(H264, &[4096]),
        ..decode()
    };
    let mut claim = vpu.claim(&encode, 0).expect("claim");
    let out = claim.add_buffer(buffer(4096)).expect("buffer");
    claim
        .give(&Destination {
            identity: 5,
            planes: &[out],
        })
        .expect("given");
    // What the kernel leaves in the planes the format does not have is not read.
    mock.finish(1, 0, [100, 7, 9]);
    mock.with(|d| d.done[1].back_mut().unwrap().num_planes = 1);
    let done = claim.complete_destination().unwrap().expect("done");
    assert_eq!(
        (done.identity, done.planes, done.bytes),
        (5, 1, [100, 0, 0])
    );
    assert_eq!(done.flags, 0);
}
