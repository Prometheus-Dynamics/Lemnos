//! The device on a node that plays the `nonai-2d` driver back: the formats it is given and
//! keeps, what is queued for each plane, and how a frame finishes or fails.

use std::cell::RefCell;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::rc::Rc;

use lemnos_driver_m2m_decode::{BufType, Capability, Dequeued, Event, PlaneMap, Queued};
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::poll::{POLLIN, POLLOUT};
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    BufferInfo, CAP_DEVICE_CAPS, CAP_STREAMING, CAP_VIDEO_M2M, CAP_VIDEO_M2M_MPLANE, PlaneInfo,
    Rect,
};
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::layout::PixelFormat;
use crate::overlay::{
    BORDER_BYTES, Border, CID_BORDER_ATTR, CID_MODE, CID_OSD_ATTR, OSD_CONTROL_BYTES, Order,
    OsdFormat, Rect as Area, Region,
};

/// One queued plane: its descriptor, length and bytes used.
type Queued3 = [(i32, u32, u32); 3];

/// What the played-back driver does and saw, on each side (output, capture).
#[derive(Default)]
pub(crate) struct Driver {
    /// A single-planar mem2mem node instead.
    single_planar: bool,
    /// The format the driver keeps instead of the one it is given.
    keeps: Option<Format>,
    formats: [Option<Format>; 2],
    queued: [bool; 2],
    planes: [Queued3; 2],
    plane_counts: [usize; 2],
    /// Never finishes a frame.
    hangs: bool,
    /// Marks the capture buffer failed.
    errors: bool,
    streaming: u32,
    /// What was set and queued, in order.
    log: Vec<Ev>,
    /// The OSD controls set, in order.
    osd: Vec<[u8; OSD_CONTROL_BYTES]>,
    /// The border controls set, in order.
    border_controls: Vec<[u8; BORDER_BYTES]>,
    /// Refuses `VIDIOC_S_CTRL`.
    fail_control: bool,
}

/// One thing the device asked of the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ev {
    Control(u32, i32),
    Payload(u32),
    Queue(usize),
}

#[derive(Clone, Default)]
pub(crate) struct Mock(pub(crate) Rc<RefCell<Driver>>);

impl Mock {
    pub(crate) fn with<T>(&self, f: impl FnOnce(&mut Driver) -> T) -> T {
        f(&mut self.0.borrow_mut())
    }
}

pub(crate) struct MockNode(pub(crate) Mock);

pub(crate) struct NoMap;

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

impl Node for MockNode {
    type Request = ();
    type Map = NoMap;

    fn capability(&self) -> io::Result<Capability> {
        let caps = if self.0.with(|d| d.single_planar) {
            CAP_VIDEO_M2M
        } else {
            CAP_VIDEO_M2M_MPLANE
        };
        Ok(Capability {
            driver: DRIVER.into(),
            card: DRIVER.into(),
            bus_info: "platform:mock".into(),
            version: 0,
            capabilities: caps | CAP_STREAMING | CAP_DEVICE_CAPS,
            device_caps: caps | CAP_STREAMING,
        })
    }

    fn set_format(&self, buf_type: BufType, format: &Format) -> io::Result<Format> {
        Ok(self.0.with(|d| {
            let kept = d.keeps.unwrap_or(*format);
            d.formats[side(buf_type)] = Some(kept);
            kept
        }))
    }

    fn format(&self, buf_type: BufType) -> io::Result<Format> {
        self.0
            .with(|d| d.formats[side(buf_type)])
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
    }

    fn set_selection(&self, _: BufType, _: u32, _: Rect) -> io::Result<Rect> {
        Err(enotty())
    }

    fn selection(&self, _: BufType, _: u32) -> io::Result<Rect> {
        Err(enotty())
    }

    fn request_buffers(&self, _: BufType, _: Memory, count: u32) -> io::Result<u32> {
        Ok(count.min(1))
    }

    fn query_buffer(&self, _: BufType, index: u32, planes: u32) -> io::Result<BufferInfo> {
        Ok(BufferInfo {
            index,
            memory: 4,
            flags: 0,
            planes: [PlaneInfo::default(); MAX_PLANES],
            num_planes: planes,
        })
    }

    fn map(&self, _: &PlaneInfo) -> io::Result<NoMap> {
        Err(enotty())
    }

    fn queue(&self, buffer: &Queued<'_, ()>) -> io::Result<()> {
        self.0.with(|d| {
            let s = side(buffer.buf_type);
            d.log.push(Ev::Queue(s));
            d.queued[s] = true;
            d.plane_counts[s] = buffer.planes.len();
            for (to, plane) in d.planes[s].iter_mut().zip(buffer.planes) {
                let fd = match plane.memory {
                    PlaneMemory::Dmabuf(fd) => fd.as_raw_fd(),
                    PlaneMemory::Mmap => -1,
                };
                *to = (fd, plane.length, plane.bytes_used);
            }
        });
        Ok(())
    }

    fn dequeue(&self, buf_type: BufType, _: Memory, planes: u32) -> io::Result<Option<Dequeued>> {
        Ok(self.0.with(|d| {
            let s = side(buf_type);
            if !d.queued[s] || d.hangs {
                return None;
            }
            d.queued[s] = false;
            Some(Dequeued {
                index: 0,
                flags: if s == 1 && d.errors {
                    BUF_FLAG_ERROR
                } else {
                    0
                },
                field: FIELD_NONE,
                timestamp: Duration::ZERO,
                sequence: 0,
                bytes_used: [0; MAX_PLANES],
                data_offset: [0; MAX_PLANES],
                num_planes: planes,
            })
        }))
    }

    fn stream_on(&self, _: BufType) -> io::Result<()> {
        self.0.with(|d| d.streaming += 1);
        Ok(())
    }

    fn stream_off(&self, _: BufType) -> io::Result<()> {
        Ok(())
    }

    fn stop_encoder(&self) -> io::Result<()> {
        Err(enotty())
    }

    fn stop_decoder(&self) -> io::Result<()> {
        Err(enotty())
    }

    fn subscribe(&self, _: u32, _: u32) -> io::Result<()> {
        Err(enotty())
    }

    fn event(&self) -> io::Result<Option<Event>> {
        Ok(None)
    }

    fn set_control(&self, id: u32, value: i32) -> io::Result<i32> {
        self.0.with(|d| {
            if d.fail_control {
                return Err(io::Error::from_raw_os_error(5));
            }
            d.log.push(Ev::Control(id, value));
            Ok(value)
        })
    }

    fn control(&self, _: u32) -> io::Result<i32> {
        Err(enotty())
    }

    fn poll(&self, events: i16, _: Option<Duration>) -> io::Result<i16> {
        let ([output, capture], hangs) = self.0.with(|d| (d.queued, d.hangs));
        if hangs {
            return Ok(0);
        }
        let mut ready = 0;
        if output {
            ready |= POLLOUT;
        }
        if capture {
            ready |= POLLIN;
        }
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

impl Payloads for MockNode {
    fn set_payload(&self, id: u32, payload: &mut [u8]) -> io::Result<()> {
        self.0.with(|d| {
            d.log.push(Ev::Payload(id));
            if let Ok(osd) = <[u8; OSD_CONTROL_BYTES]>::try_from(&*payload) {
                d.osd.push(osd);
            } else if let Ok(border) = <[u8; BORDER_BYTES]>::try_from(&*payload) {
                d.border_controls.push(border);
            }
        });
        Ok(())
    }
}

fn layout(format: PixelFormat) -> Layout {
    Layout::new(format, 64, 8).expect("layout")
}

fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"nonai2d-test", len).expect("memfd")
}

fn device(mock: &Mock, src: Layout, dst: Layout) -> io::Result<Device<MockNode>> {
    Device::new(MockNode(mock.clone()), src, dst)
}

#[test]
fn nv12_asks_for_two_planes_with_the_drivers_strides() {
    let Format::Multi(f) = format(layout(PixelFormat::Nv12)) else {
        panic!("multi-planar");
    };
    assert_eq!(f.num_planes, 2);
    assert_eq!(f.field, FIELD_NONE);
    assert_eq!(f.pixelformat, PixelFormat::Nv12.fourcc());
    assert_eq!(
        (f.planes[0].bytes_per_line, f.planes[0].size_image),
        (64, 512)
    );
    assert_eq!(
        (f.planes[1].bytes_per_line, f.planes[1].size_image),
        (64, 256)
    );
}

#[test]
fn a_format_the_driver_changed_is_refused() {
    let want = layout(PixelFormat::Nv12);
    assert!(kept(&format(want), want));
    assert!(!kept(&format(layout(PixelFormat::Argb32)), want));
    let clamped = Layout::new(PixelFormat::Nv12, 32, 8).expect("layout");
    assert!(!kept(&format(clamped), want));
    // The driver clamps a size: the open is refused as the caller's.
    let mock = Mock::default();
    let argb = layout(PixelFormat::Argb32);
    let narrow = Layout::new(PixelFormat::Argb32, 32, 8).expect("layout");
    mock.with(|d| d.keeps = Some(format(narrow)));
    let refused = device(&mock, argb, argb).expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(mock.with(|d| d.streaming), 0);
    // It falls back to its first format, of one plane, for two: refused too.
    let mock = Mock::default();
    mock.with(|d| d.keeps = Some(format(argb)));
    assert!(device(&mock, want, want).is_err());
    assert_eq!(mock.with(|d| d.streaming), 0);
}

#[test]
fn the_engine_does_not_scale() {
    let small = Layout::new(PixelFormat::Nv12, 32, 8).expect("layout");
    let mock = Mock::default();
    let refused = device(&mock, small, layout(PixelFormat::Nv12)).expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert!(
        mock.with(|d| d.formats == [None, None]),
        "nothing reached the node"
    );
    let refused = Device::open(
        Path::new("/nonexistent/video0"),
        small,
        layout(PixelFormat::Nv12),
    );
    assert_eq!(
        refused.expect_err("refused").kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn a_missing_or_single_planar_node_is_an_error() {
    let frame = layout(PixelFormat::Nv12);
    assert!(Device::open(Path::new("/nonexistent/video0"), frame, frame).is_err());
    let mock = Mock::default();
    mock.with(|d| d.single_planar = true);
    assert!(device(&mock, frame, frame).is_err());
}

#[test]
fn each_plane_is_queued_as_its_own_dma_buf_with_its_real_length() {
    let mock = Mock::default();
    let (src_layout, dst_layout) = (layout(PixelFormat::Nv12), layout(PixelFormat::Abgr32));
    let mut device = device(&mock, src_layout, dst_layout).expect("device");
    assert_eq!(device.layouts(), (src_layout, dst_layout));
    assert_eq!(mock.with(|d| d.streaming), 2);
    let (y, uv, out) = (buffer(512), buffer(4096), buffer(2048));
    let src = Frame::new(src_layout, &[y.as_fd(), uv.as_fd()]).expect("source");
    let dst = Frame::new(dst_layout, &[out.as_fd()]).expect("destination");
    device.convert(&src, &dst).expect("converted");
    let (planes, counts) = mock.with(|d| (d.planes, d.plane_counts));
    assert_eq!(counts, [2, 1]);
    assert_eq!(
        planes[0][..2],
        [(y.as_raw_fd(), 512, 512), (uv.as_raw_fd(), 4096, 256)]
    );
    assert_eq!(planes[1][0], (out.as_raw_fd(), 2048, 0));
}

#[test]
fn a_frame_in_another_layout_is_refused_before_anything_is_queued() {
    let mock = Mock::default();
    let nv12 = layout(PixelFormat::Nv12);
    let mut device = device(&mock, nv12, nv12).expect("device");
    let packed = buffer(4096);
    let argb = Frame::new(layout(PixelFormat::Argb32), &[packed.as_fd()]).expect("frame");
    let (y, uv) = (buffer(512), buffer(256));
    let ok = Frame::new(nv12, &[y.as_fd(), uv.as_fd()]).expect("frame");
    let refused = device.convert(&argb, &ok).expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    let refused = device.convert(&ok, &argb).expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(mock.with(|d| d.queued), [false, false]);
    device.convert(&ok, &ok).expect("the right layouts run");
}

#[test]
fn a_frame_the_device_never_finishes_times_out_and_a_failed_one_is_an_error() {
    let mock = Mock::default();
    let rgb = layout(PixelFormat::Rgb565);
    let (a, b) = (buffer(1024), buffer(1024));
    let src = Frame::new(rgb, &[a.as_fd()]).expect("frame");
    let dst = Frame::new(rgb, &[b.as_fd()]).expect("frame");
    let mut failing = device(&mock, rgb, rgb).expect("device");
    mock.with(|d| d.errors = true);
    assert_eq!(
        failing.convert(&src, &dst).expect_err("failed").kind(),
        io::ErrorKind::Other
    );
    let mock = Mock::default();
    let mut hung = device(&mock, rgb, rgb).expect("device");
    mock.with(|d| d.hangs = true);
    assert_eq!(
        hung.convert(&src, &dst).expect_err("timed out").kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn a_conversion_on_an_open_device_allocates_nothing() {
    let mock = Mock::default();
    let nv12 = layout(PixelFormat::Yuv420);
    let mut device = device(&mock, nv12, nv12).expect("device");
    let (y, u, v) = (buffer(512), buffer(128), buffer(128));
    let frame = Frame::new(nv12, &[y.as_fd(), u.as_fd(), v.as_fd()]).expect("frame");
    // The stand-in's own log, grown before measuring.
    mock.with(|d| d.log.reserve(1000));
    let (outcome, count) = allocations(|| {
        for _ in 0..100 {
            device.convert(&frame, &frame)?;
        }
        Ok::<_, io::Error>(())
    });
    outcome.expect("converted");
    assert_eq!(count, 0);
}

/// A 64x8 ARGB frame's overlay: one region and `borders` borders.
fn overlay<'a>(layout: Layout, image: &'a [u8], borders: usize) -> Overlay<'a> {
    let mut overlay = Overlay::new(layout).expect("overlay");
    overlay
        .add_region(Region {
            rect: Area {
                x: 2,
                y: 2,
                width: 4,
                height: 4,
            },
            format: OsdFormat::Argb32,
            image,
            osd_alpha: 0xff,
            video_alpha: 0,
            background_alpha: 0,
            background: 0,
            order: Order::VideoOsd,
        })
        .expect("region");
    for i in 0..borders {
        overlay
            .add_border(Border {
                rect: Area {
                    x: 2 + 2 * i as u16,
                    y: 2,
                    width: 4,
                    height: 4,
                },
                line_width: 2,
                color: 0xff,
            })
            .expect("border");
    }
    overlay
}

#[test]
fn a_draw_runs_the_engine_once_in_place_and_goes_back_to_converting() {
    let mock = Mock::default();
    let argb = layout(PixelFormat::Argb32);
    let mut device = device(&mock, argb, argb).expect("device");
    let out = buffer(2048);
    let frame = Frame::new(argb, &[out.as_fd()]).expect("frame");
    let image = [7u8; 4 * 4 * 4];
    device
        .draw(&frame, &overlay(argb, &image, 2))
        .expect("drawn");
    assert_eq!(
        mock.with(|d| d.log.clone()),
        [
            Ev::Payload(CID_OSD_ATTR),
            Ev::Payload(CID_BORDER_ATTR),
            Ev::Payload(CID_BORDER_ATTR),
            // Queued for capture while converting: nothing starts.
            Ev::Queue(1),
            Ev::Control(CID_MODE, 3),
            // Queued for output: the one start.
            Ev::Queue(0),
            Ev::Control(CID_MODE, 0),
        ]
    );
    // The same dma-buf on both sides, the source with its bytes used.
    let planes = mock.with(|d| d.planes);
    assert_eq!(planes[1][0], (out.as_raw_fd(), 2048, 0));
    assert_eq!(planes[0][0], (out.as_raw_fd(), 2048, 2048));
    let osd = mock.with(|d| d.osd[0]);
    assert_eq!(osd[1], 1, "region 0 valid");
    assert_eq!(
        osd[16..24],
        (image.as_ptr() as usize as u64).to_le_bytes(),
        "the image the overlay borrows"
    );
    assert_eq!(osd[41], 0, "region 1 invalid");
    let borders = mock.with(|d| d.border_controls.clone());
    assert_eq!(borders.iter().map(|b| b[0]).collect::<Vec<_>>(), [0, 1]);
    // And it converts again.
    device.convert(&frame, &frame).expect("converted");
}

#[test]
fn a_draw_is_refused_before_anything_reaches_the_node() {
    let mock = Mock::default();
    let argb = layout(PixelFormat::Argb32);
    let rgb = layout(PixelFormat::Rgb24);
    let image = [0u8; 64];
    let out = buffer(2048);
    let frame = Frame::new(argb, &[out.as_fd()]).expect("frame");

    // Two layouts: the engine draws in place.
    let mut converting = device(&mock, rgb, argb).expect("device");
    let refused = converting
        .draw(&frame, &overlay(argb, &image, 0))
        .expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);

    let mut device = device(&mock, argb, argb).expect("device");
    // An overlay for another layout.
    let other = Layout::new(PixelFormat::Argb32, 64, 16).unwrap();
    let refused = device
        .draw(&frame, &overlay(other, &image, 0))
        .expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    // Nothing to draw.
    let empty = Overlay::new(argb).unwrap();
    let refused = device.draw(&frame, &empty).expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    // A frame of another layout.
    let wide = buffer(4096);
    let rgb_frame = Frame::new(rgb, &[wide.as_fd()]).expect("frame");
    let refused = device
        .draw(&rgb_frame, &overlay(argb, &image, 0))
        .expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert!(mock.with(|d| d.log.is_empty()), "nothing set or queued");
}

#[test]
fn borders_given_to_an_open_node_cannot_be_taken_back() {
    let mock = Mock::default();
    let argb = layout(PixelFormat::Argb32);
    let mut device = device(&mock, argb, argb).expect("device");
    let out = buffer(2048);
    let frame = Frame::new(argb, &[out.as_fd()]).expect("frame");
    let image = [0u8; 64];
    device
        .draw(&frame, &overlay(argb, &image, 3))
        .expect("drawn");
    mock.with(|d| d.log.clear());
    // Fewer borders would leave the third drawn.
    let refused = device
        .draw(&frame, &overlay(argb, &image, 2))
        .expect_err("refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert!(mock.with(|d| d.log.is_empty()));
    // Regions alone don't draw borders; as many or more is fine.
    device
        .draw(&frame, &overlay(argb, &image, 0))
        .expect("drawn");
    device
        .draw(&frame, &overlay(argb, &image, 4))
        .expect("drawn");
}

#[test]
fn a_draw_that_fails_part_way_leaves_the_device_refusing_until_reopened() {
    let mock = Mock::default();
    let argb = layout(PixelFormat::Argb32);
    let mut device = device(&mock, argb, argb).expect("device");
    let out = buffer(2048);
    let frame = Frame::new(argb, &[out.as_fd()]).expect("frame");
    let image = [0u8; 64];
    mock.with(|d| d.fail_control = true);
    let failed = device
        .draw(&frame, &overlay(argb, &image, 0))
        .expect_err("failed");
    assert_ne!(failed.kind(), io::ErrorKind::InvalidInput, "a device error");
    mock.with(|d| {
        d.fail_control = false;
        d.log.clear();
    });
    // The mode may not be conversion: nothing more is queued on this open.
    assert_ne!(
        device.convert(&frame, &frame).expect_err("refused").kind(),
        io::ErrorKind::InvalidInput
    );
    assert_ne!(
        device
            .draw(&frame, &overlay(argb, &image, 0))
            .expect_err("refused")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(mock.with(|d| d.log.is_empty()));
}

#[test]
fn a_draw_on_an_open_device_allocates_nothing() {
    let mock = Mock::default();
    let nv12 = layout(PixelFormat::Nv12);
    let mut device = device(&mock, nv12, nv12).expect("device");
    let (y, uv) = (buffer(512), buffer(256));
    let frame = Frame::new(nv12, &[y.as_fd(), uv.as_fd()]).expect("frame");
    let image = [0u8; 64];
    let overlay = overlay(nv12, &image, 1);
    mock.with(|d| {
        d.log.reserve(1000);
        d.osd.reserve(100);
        d.border_controls.reserve(100);
    });
    let (outcome, count) = allocations(|| {
        for _ in 0..100 {
            device.draw(&frame, &overlay)?;
        }
        Ok::<_, io::Error>(())
    });
    outcome.expect("drawn");
    assert_eq!(count, 0);
}
