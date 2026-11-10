//! The mainline route on a node that plays a mem2mem converter back: formats, crops, controls,
//! queues, and what crosses between the caller's buffers and the device's. The device's pixels are
//! the test's (it fills each capture mapping with the frame the test expects), so these check the
//! route, not a unit's arithmetic; `tests/v4l2.rs` runs the same calls on the kernel's `vim2m`.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::{Arc, Mutex};

use lemnos_driver_m2m_decode::{
    BufType, Capability, Dequeued, Event, Format, MAX_PLANES, Memory as QueueMemory, Node,
    PlaneMap, PlaneMemory, Queued, Rect as V4l2Rect,
};
use lemnos_linux_sys::poll::{POLLIN, POLLOUT};
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    BufferInfo, CAP_DEVICE_CAPS, CAP_STREAMING, CAP_VIDEO_M2M, CAP_VIDEO_M2M_MPLANE, CID_ROTATE,
    PlaneInfo,
};
use lemnos_measure::test_alloc::allocations;

use super::*;

/// What the device saw last on each side (output, capture).
#[derive(Default)]
pub(crate) struct Device {
    pub(crate) multiplanar: bool,
    pub(crate) opens: u32,
    pub(crate) formats: [Option<Format>; 2],
    pub(crate) crop: Option<V4l2Rect>,
    /// The crop the device keeps instead of the one asked for.
    pub(crate) crop_kept: Option<V4l2Rect>,
    /// The control value the device keeps instead of the one asked for.
    pub(crate) control_kept: Option<i32>,
    pub(crate) controls: [(u32, i32); 4],
    pub(crate) control_count: usize,
    /// The frame a capture mapping is filled with.
    pub(crate) capture_fill: Vec<u8>,
    /// What the device reports it wrote, when not the capture frame's size.
    pub(crate) written: Option<u32>,
    pub(crate) queued: [bool; 2],
    /// The last queued plane on each side: bytes used, length, and the dma-buf descriptor.
    pub(crate) planes: [(u32, u32, Option<i32>); 2],
    /// Every queued buffer fails, as a device that has stopped answering.
    pub(crate) fail: bool,
    /// Buffers queued with a dma-buf, and with the device's own memory.
    pub(crate) queued_dmabuf: u32,
    pub(crate) queued_mmap: u32,
}

#[derive(Clone, Default)]
pub(crate) struct Mock(Arc<Mutex<Device>>);

impl Mock {
    pub(crate) fn open(&self) -> io::Result<MockNode> {
        self.with(|d| d.opens += 1);
        Ok(MockNode(self.clone()))
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&mut Device) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }

    pub(crate) fn rga(&self) -> M2mRga<MockNode> {
        let me = self.clone();
        M2mRga::with_opener(move || me.open())
    }
}

pub(crate) struct MockNode(Mock);

pub(crate) struct MockMap(Box<[u8]>);

impl PlaneMap for MockMap {
    fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

fn side(buf_type: BufType) -> usize {
    usize::from(!buf_type.is_output())
}

fn enotty() -> io::Error {
    io::Error::from_raw_os_error(25)
}

/// Capture mappings are told apart by their offset.
const CAPTURE_OFFSET: u32 = 1 << 20;

impl Node for MockNode {
    type Request = ();
    type Map = MockMap;

    fn capability(&self) -> io::Result<Capability> {
        let caps = if self.0.with(|d| d.multiplanar) {
            CAP_VIDEO_M2M_MPLANE
        } else {
            CAP_VIDEO_M2M
        };
        Ok(Capability {
            driver: "mock".into(),
            card: "mock".into(),
            bus_info: "platform:mock".into(),
            version: 0,
            capabilities: caps | CAP_STREAMING | CAP_DEVICE_CAPS,
            device_caps: caps | CAP_STREAMING,
        })
    }

    fn set_format(&self, buf_type: BufType, format: &Format) -> io::Result<Format> {
        self.0.with(|d| d.formats[side(buf_type)] = Some(*format));
        Ok(*format)
    }

    fn format(&self, buf_type: BufType) -> io::Result<Format> {
        self.0
            .with(|d| d.formats[side(buf_type)])
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
    }

    fn set_selection(&self, _: BufType, _: u32, rect: V4l2Rect) -> io::Result<V4l2Rect> {
        Ok(self.0.with(|d| {
            let kept = d.crop_kept.unwrap_or(rect);
            d.crop = Some(kept);
            kept
        }))
    }

    fn selection(&self, _: BufType, _: u32) -> io::Result<V4l2Rect> {
        Err(enotty())
    }

    fn request_buffers(&self, _: BufType, _: QueueMemory, count: u32) -> io::Result<u32> {
        Ok(count.min(1))
    }

    fn query_buffer(&self, buf_type: BufType, index: u32, planes: u32) -> io::Result<BufferInfo> {
        let format = self.format(buf_type)?;
        let length = match format {
            Format::Single(s) => s.size_image,
            Format::Multi(m) => m.planes[0].size_image,
        };
        let mut info = BufferInfo {
            index,
            memory: 1,
            flags: 0,
            planes: [PlaneInfo::default(); MAX_PLANES],
            num_planes: planes,
        };
        info.planes[0] = PlaneInfo {
            length,
            offset: if side(buf_type) == 1 {
                CAPTURE_OFFSET
            } else {
                0
            },
            ..PlaneInfo::default()
        };
        Ok(info)
    }

    fn map(&self, plane: &PlaneInfo) -> io::Result<MockMap> {
        let mut bytes = vec![0u8; plane.length as usize];
        if plane.offset == CAPTURE_OFFSET {
            self.0.with(|d| {
                let n = d.capture_fill.len().min(bytes.len());
                bytes[..n].copy_from_slice(&d.capture_fill[..n]);
            });
        }
        Ok(MockMap(bytes.into_boxed_slice()))
    }

    fn queue(&self, buffer: &Queued<'_, ()>) -> io::Result<()> {
        if self.0.with(|d| d.fail) {
            return Err(io::Error::from_raw_os_error(5));
        }
        let plane = buffer.planes[0];
        let fd = match plane.memory {
            PlaneMemory::Mmap => None,
            PlaneMemory::Dmabuf(fd) => Some(fd.as_raw_fd()),
        };
        self.0.with(|d| {
            let s = side(buffer.buf_type);
            d.queued[s] = true;
            d.planes[s] = (plane.bytes_used, plane.length, fd);
            match fd {
                Some(_) => d.queued_dmabuf += 1,
                None => d.queued_mmap += 1,
            }
        });
        Ok(())
    }

    fn dequeue(
        &self,
        buf_type: BufType,
        _: QueueMemory,
        planes: u32,
    ) -> io::Result<Option<Dequeued>> {
        self.0.with(|d| {
            let s = side(buf_type);
            if !d.queued[s] {
                return Ok(None);
            }
            d.queued[s] = false;
            let mut bytes_used = [0; MAX_PLANES];
            bytes_used[0] = if s == 1 {
                d.written.unwrap_or(d.capture_fill.len() as u32)
            } else {
                d.planes[0].0
            };
            Ok(Some(Dequeued {
                index: 0,
                flags: 0,
                field: 1,
                timestamp: std::time::Duration::ZERO,
                sequence: 0,
                bytes_used,
                data_offset: [0; MAX_PLANES],
                num_planes: planes,
            }))
        })
    }

    fn stream_on(&self, _: BufType) -> io::Result<()> {
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
        Ok(self.0.with(|d| {
            if let Some(slot) = d.controls.get_mut(d.control_count) {
                *slot = (id, value);
                d.control_count += 1;
            }
            d.control_kept.unwrap_or(value)
        }))
    }

    fn control(&self, _: u32) -> io::Result<i32> {
        Ok(0)
    }

    fn poll(&self, events: i16, _: Option<std::time::Duration>) -> io::Result<i16> {
        let [output, capture] = self.0.with(|d| d.queued);
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

    fn wait_request(&self, _: &(), _: Option<std::time::Duration>) -> io::Result<bool> {
        Err(enotty())
    }
}

fn rgb(w: u32, h: u32) -> Layout {
    Layout::packed(PixelFormat::Rgb24, w, h)
}

fn bgr(w: u32, h: u32) -> Layout {
    Layout::packed(PixelFormat::Bgr24, w, h)
}

/// A memfd of `len` bytes: it answers the size query as a dma-buf does.
fn dmabuf(len: u64) -> File {
    let file = File::from(lemnos_linux_sys::dma_buf::memfd(c"rga-v4l2").unwrap());
    file.set_len(len).unwrap();
    file
}

#[test]
fn a_byte_conversion_sets_both_formats_and_copies_the_frame_through() {
    for multiplanar in [false, true] {
        let mock = Mock::default();
        let want: Vec<u8> = (0..24).collect();
        mock.with(|d| {
            d.multiplanar = multiplanar;
            d.capture_fill = want.clone();
        });
        let mut rga = mock.rga();
        let src = vec![7u8; 24];
        let mut dst = vec![0u8; 24];
        assert_eq!(
            rga.convert(rgb(4, 2), &src, None, bgr(4, 2), &mut dst)
                .unwrap(),
            24
        );
        assert_eq!(dst, want);
        mock.with(|d| {
            let [Some(out), Some(cap)] = d.formats else {
                panic!("both formats set")
            };
            assert_eq!(out.is_multiplanar(), multiplanar);
            assert_eq!(kept(&out, rgb(4, 2)).unwrap(), 24);
            assert_eq!(kept(&cap, bgr(4, 2)).unwrap(), 24);
            // The whole source frame was queued, and nothing named a dma-buf.
            assert_eq!(d.planes[0], (24, 0, None));
            assert_eq!(d.crop, None);
        });
        // The same conversion reuses its session.
        rga.convert(rgb(4, 2), &src, None, bgr(4, 2), &mut dst)
            .unwrap();
        assert_eq!((mock.with(|d| d.opens), rga.sessions()), (1, 1));
    }
}

#[test]
fn the_device_never_writes_past_the_callers_buffer_or_its_mapping() {
    let mock = Mock::default();
    mock.with(|d| {
        d.capture_fill = vec![9; 24];
        // The device claims more than its plane holds.
        d.written = Some(1 << 20);
    });
    let mut rga = mock.rga();
    let mut dst = vec![0u8; 30];
    assert_eq!(
        rga.convert(rgb(4, 2), &[0; 24], None, bgr(4, 2), &mut dst)
            .unwrap(),
        24
    );
    assert_eq!(dst[24..], [0; 6]);
    let mut short = vec![0u8; 10];
    assert_eq!(
        rga.convert(rgb(4, 2), &[0; 24], None, bgr(4, 2), &mut short)
            .unwrap(),
        10
    );
}

#[test]
fn a_short_source_is_refused_before_the_device_sees_it() {
    let mock = Mock::default();
    let mut rga = mock.rga();
    let err = rga
        .convert(rgb(4, 2), &[0; 23], None, bgr(4, 2), &mut [0; 24])
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(!mock.with(|d| d.queued[0] || d.queued[1]));
}

#[test]
fn crops_go_to_the_output_queue_and_a_changed_crop_is_refused() {
    let mock = Mock::default();
    mock.with(|d| d.capture_fill = vec![1; 12]);
    let mut rga = mock.rga();
    let rect = Rect {
        left: 1,
        top: 0,
        width: 2,
        height: 2,
    };
    rga.crop_bytes(rgb(4, 2), &[0; 24], rect, bgr(2, 2), &mut [0; 12])
        .unwrap();
    assert_eq!(
        mock.with(|d| d.crop),
        Some(V4l2Rect {
            left: 1,
            top: 0,
            width: 2,
            height: 2
        })
    );
    // A full-frame crop is a copy, with no selection.
    let mock = Mock::default();
    let mut rga = mock.rga();
    let full = Rect {
        left: 0,
        top: 0,
        width: 4,
        height: 2,
    };
    rga.crop_bytes(rgb(4, 2), &[0; 24], full, bgr(4, 2), &mut [0; 24])
        .unwrap();
    assert_eq!(mock.with(|d| d.crop), None);
    // Outside the frame, or the wrong output size: refused before a node opens.
    let outside = Rect { left: 3, ..rect };
    assert!(
        rga.crop_bytes(rgb(4, 2), &[0; 24], outside, bgr(2, 2), &mut [0; 12])
            .is_err()
    );
    assert!(
        rga.crop_bytes(rgb(4, 2), &[0; 24], rect, bgr(3, 2), &mut [0; 18])
            .is_err()
    );
    // A device that keeps another rectangle is refused.
    let mock = Mock::default();
    mock.with(|d| {
        d.crop_kept = Some(V4l2Rect {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
        })
    });
    let mut rga = mock.rga();
    assert!(
        rga.crop_bytes(rgb(4, 2), &[0; 24], rect, bgr(2, 2), &mut [0; 12])
            .is_err()
    );
}

#[test]
fn turns_set_their_control_and_a_device_that_ignores_it_is_unsupported() {
    let mock = Mock::default();
    mock.with(|d| d.capture_fill = vec![0; 24]);
    let mut rga = mock.rga();
    rga.transform_bytes(
        rgb(4, 2),
        &[0; 24],
        rgb(2, 4),
        &mut [0; 24],
        Transform::Rotate90,
    )
    .unwrap();
    assert_eq!(mock.with(|d| d.controls[0]), (CID_ROTATE, 90));
    assert!(
        rga.transform_bytes(
            rgb(4, 2),
            &[0; 24],
            rgb(4, 2),
            &mut [0; 24],
            Transform::Rotate90
        )
        .is_err()
    );
    let mock = Mock::default();
    mock.with(|d| d.control_kept = Some(0));
    let err = mock
        .rga()
        .transform_bytes(
            rgb(4, 2),
            &[0; 24],
            rgb(4, 2),
            &mut [0; 24],
            Transform::FlipVertical,
        )
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn dma_bufs_go_by_their_real_size_and_short_ones_are_refused() {
    let mock = Mock::default();
    let mut rga = mock.rga();
    let (src, dst) = (dmabuf(4096), dmabuf(24));
    rga.blit(rgb(4, 2), src.as_fd(), None, bgr(4, 2), dst.as_fd())
        .unwrap();
    mock.with(|d| {
        assert_eq!(d.planes[0], (24, 4096, Some(src.as_raw_fd())));
        assert_eq!(d.planes[1], (0, 24, Some(dst.as_raw_fd())));
        d.queued = [false; 2];
    });
    let short = dmabuf(23);
    for (s, t) in [(&short, &dst), (&src, &short)] {
        let err = rga
            .blit(rgb(4, 2), s.as_fd(), None, bgr(4, 2), t.as_fd())
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
    assert!(!mock.with(|d| d.queued[0] || d.queued[1]));
}

#[test]
fn resizes_the_mainline_driver_cannot_do_never_reach_it() {
    let mock = Mock::default();
    let mut rga = mock.rga();
    for (dst, filter) in [
        (rgb(3, 2), Filter::Area),
        (rgb(2, 1), Filter::Bilinear),
        (rgb(4, 2), Filter::Bilinear),
    ] {
        let err = rga
            .resize_bytes(rgb(4, 2), &[0; 24], dst, &mut [0; 24], filter)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{dst:?} {filter:?}");
    }
    assert_eq!(mock.with(|d| d.opens), 0);
    // An area downscale that leaves a remainder reads the top-left part.
    mock.with(|d| d.capture_fill = vec![0; 6]);
    rga.resize_bytes(rgb(5, 3), &[0; 45], rgb(2, 1), &mut [0; 6], Filter::Area)
        .unwrap();
    assert_eq!(
        mock.with(|d| d.crop),
        Some(V4l2Rect {
            left: 0,
            top: 0,
            width: 4,
            height: 2
        })
    );
}

#[test]
fn sessions_are_bounded() {
    let mock = Mock::default();
    let mut rga = mock.rga();
    for w in 1..=(MAX_SESSIONS as u32 + 3) {
        mock.with(|d| d.capture_fill = vec![0; 3 * w as usize]);
        let src = vec![0u8; 3 * w as usize];
        let mut dst = vec![0u8; 3 * w as usize];
        rga.convert(rgb(w, 1), &src, None, bgr(w, 1), &mut dst)
            .unwrap();
        assert!(rga.sessions() <= MAX_SESSIONS);
    }
    assert_eq!(rga.sessions(), MAX_SESSIONS);
}

#[test]
fn a_conversion_on_an_open_session_allocates_nothing() {
    let mock = Mock::default();
    mock.with(|d| d.capture_fill = vec![3; 24]);
    let mut rga = mock.rga();
    let src = vec![7u8; 24];
    let mut dst = vec![0u8; 24];
    rga.convert(rgb(4, 2), &src, None, bgr(4, 2), &mut dst)
        .unwrap();
    let (result, count) = allocations(|| {
        for _ in 0..100 {
            rga.convert(rgb(4, 2), &src, None, bgr(4, 2), &mut dst)?;
        }
        Ok::<_, io::Error>(())
    });
    result.unwrap();
    assert_eq!(count, 0, "allocations in 100 conversions");
}
