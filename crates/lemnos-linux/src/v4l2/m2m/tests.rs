//! Session tests. The bookkeeping checks need no device; the round trips use
//! vim2m (`modprobe vim2m`) and the system dma-heap, and are skipped where
//! either is missing.

use super::*;
#[cfg(feature = "dma")]
use crate::dma::{Access, DmaHeap};
use crate::v4l2::{PixFormat, PixFormatMplane, Rect, fourcc};
use std::os::fd::AsFd;

/// A queue with no device behind it, for the bookkeeping checks.
fn queue(count: u32, memory: Memory, num_planes: usize) -> Queue {
    let format = if num_planes == 1 {
        Format::Single(PixFormat::default())
    } else {
        Format::Multi(PixFormatMplane {
            num_planes: num_planes as u32,
            ..Default::default()
        })
    };
    Queue {
        buf_type: queue_type(&format, true),
        memory,
        format,
        count,
        queued: 0,
        maps: Vec::new(),
        crop: None,
    }
}

fn dmabuf_plane(fd: BorrowedFd<'_>, bytes: u32) -> QueuePlane<'_> {
    QueuePlane {
        memory: PlaneMemory::Dmabuf(fd),
        length: bytes,
        bytes_used: bytes,
        data_offset: 0,
    }
}

#[test]
fn queue_types_follow_the_format_layout_and_direction() {
    let single = Format::Single(PixFormat::default());
    let multi = Format::Multi(PixFormatMplane::default());
    assert_eq!(queue_type(&single, true), BufType::VideoOutput);
    assert_eq!(queue_type(&single, false), BufType::VideoCapture);
    assert_eq!(queue_type(&multi, true), BufType::VideoOutputMplane);
    assert_eq!(queue_type(&multi, false), BufType::VideoCaptureMplane);
}

#[test]
fn indices_and_planes_are_checked_before_the_device_sees_them() {
    let file = std::fs::File::open("/dev/null").unwrap();
    let fd = file.as_fd();
    let mut q = queue(2, Memory::Dmabuf, 1);
    let plane = [dmabuf_plane(fd, 16)];
    assert!(
        q.check_queueable(2, &plane).is_err(),
        "index past the count"
    );
    assert!(q.check_queueable(0, &[]).is_err(), "no planes");
    assert!(
        q.check_queueable(0, &[plane[0], plane[0]]).is_err(),
        "two planes for one"
    );
    let mmap = [QueuePlane {
        memory: PlaneMemory::Mmap,
        length: 0,
        bytes_used: 0,
        data_offset: 0,
    }];
    assert!(
        q.check_queueable(0, &mmap).is_err(),
        "MMAP planes on a DMA-buf queue"
    );
    assert!(q.check_queueable(0, &plane).is_ok());
    q.queued |= 1;
    assert!(q.check_queueable(0, &plane).is_err(), "already queued");
    assert_eq!(q.free(), Some(1));
    q.queued |= 2;
    assert_eq!(q.free(), None);
}

#[test]
fn a_session_refuses_a_node_that_is_not_m2m() {
    // /dev/null is not a V4L2 node at all: QUERYCAP fails.
    let setup = QueueSetup {
        format: Format::Single(PixFormat::default()),
        memory: Memory::Mmap,
        buffers: 1,
        crop: None,
    };
    assert!(M2mSession::open("/dev/null", setup, setup).is_err());
}

/// The first memory-to-memory node whose driver is `driver`.
fn find_m2m(driver: &str) -> Option<std::path::PathBuf> {
    crate::v4l2::find_m2m(driver)
        .ok()
        .flatten()
        .map(|(path, _)| path)
}

fn rgb_setup(memory: Memory) -> QueueSetup {
    QueueSetup {
        format: Format::Single(PixFormat {
            width: 64,
            height: 48,
            pixelformat: fourcc(b"RGB3"),
            bytes_per_line: 192,
            size_image: 64 * 48 * 3,
            ..Default::default()
        }),
        memory,
        buffers: 2,
        crop: None,
    }
}

const FRAME: u32 = 64 * 48 * 3;

/// vim2m with MMAP on both queues: frames come back identical, stamped with
/// their source frame's time.
#[test]
fn vim2m_round_trips_mmap_frames() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    let mut s = M2mSession::open(&path, rgb_setup(Memory::Mmap), rgb_setup(Memory::Mmap)).unwrap();
    assert_eq!(s.output_format().num_planes(), 1);
    let mmap = |bytes: u32| {
        [QueuePlane {
            memory: PlaneMemory::Mmap,
            length: 0,
            bytes_used: bytes,
            data_offset: 0,
        }]
    };
    for index in 0..s.capture_buffers() {
        s.queue_capture(index, &mmap(0)).unwrap();
    }
    s.stream_on().unwrap();
    for k in 0..4u64 {
        let out = s.free_output().expect("a free source buffer");
        let pattern: Vec<u8> = (0..FRAME as u64)
            .map(|i| ((i + k * 7) % 251) as u8)
            .collect();
        s.output_plane_mut(out, 0).unwrap()[..FRAME as usize].copy_from_slice(&pattern);
        s.queue_output(out, &mmap(FRAME), Duration::from_millis(k))
            .unwrap();
        let ready = s
            .wait(
                Ready {
                    capture: true,
                    ..Ready::default()
                },
                Some(Duration::from_secs(5)),
            )
            .unwrap();
        assert!(ready.capture, "frame {k} came out");
        let got = s.dequeue_capture().unwrap().expect("a frame");
        assert_eq!(got.timestamp, Duration::from_millis(k));
        assert_eq!(got.bytes_used[0], FRAME);
        let data = s.capture_plane(got.index, 0).unwrap();
        assert_eq!(&data[..FRAME as usize], &pattern[..], "frame {k}");
        s.queue_capture(got.index, &mmap(0)).unwrap();
        while s.dequeue_output().unwrap().is_some() {}
    }
    s.stream_off().unwrap();
    assert_eq!(s.free_capture(), Some(0), "stream_off returns every buffer");
}

#[cfg(feature = "dma")]
/// vim2m reading dma-bufs from the system heap in place: each frame comes out
/// the same, and the destination stays MMAP.
#[test]
fn vim2m_reads_dmabufs_in_place() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    let Ok(heap) = DmaHeap::open("system") else {
        crate::tests::skip("no system dma-heap");
        return;
    };
    let mut s =
        M2mSession::open(&path, rgb_setup(Memory::Dmabuf), rgb_setup(Memory::Mmap)).unwrap();
    assert_eq!(s.output_memory(), Memory::Dmabuf);
    let mmap = [QueuePlane {
        memory: PlaneMemory::Mmap,
        length: 0,
        bytes_used: 0,
        data_offset: 0,
    }];
    for index in 0..s.capture_buffers() {
        s.queue_capture(index, &mmap).unwrap();
    }
    s.stream_on().unwrap();
    let bufs: Vec<_> = (0..2)
        .map(|_| heap.allocate(FRAME as usize).unwrap())
        .collect();
    for k in 0..6u64 {
        let index = s.free_output().expect("a free source buffer");
        let buf = &bufs[k as usize % 2];
        let mut map = buf.map().unwrap();
        buf.begin_cpu_access(Access::Write).unwrap();
        for (i, b) in map.as_mut_slice()[..FRAME as usize].iter_mut().enumerate() {
            *b = ((i as u64 + k * 3) % 251) as u8;
        }
        buf.end_cpu_access(Access::Write).unwrap();
        let plane = [dmabuf_plane(buf.as_fd(), FRAME)];
        s.queue_output(index, &plane, Duration::from_millis(k))
            .unwrap();
        s.wait(
            Ready {
                capture: true,
                ..Ready::default()
            },
            Some(Duration::from_secs(5)),
        )
        .unwrap();
        let got = s.dequeue_capture().unwrap().expect("a frame");
        buf.begin_cpu_access(Access::Read).unwrap();
        let data = s.capture_plane(got.index, 0).unwrap();
        assert_eq!(
            &data[..FRAME as usize],
            &map.as_slice()[..FRAME as usize],
            "frame {k}"
        );
        buf.end_cpu_access(Access::Read).unwrap();
        s.queue_capture(got.index, &mmap).unwrap();
        // Wait for the source to come back before the next frame reuses it.
        s.wait(
            Ready {
                output: true,
                ..Ready::default()
            },
            Some(Duration::from_secs(5)),
        )
        .unwrap();
        while s.dequeue_output().unwrap().is_some() {}
    }
    s.stream_off().unwrap();
}

/// Every memory-to-memory node of `driver`, in name order.
fn m2m_nodes(driver: &str) -> Vec<std::path::PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev")
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    nodes.retain(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("video"))
    });
    nodes.sort();
    nodes.retain(|path| {
        sys::open_node(path)
            .and_then(|f| sys::querycap(f.as_fd()))
            .is_ok_and(|caps| {
                caps.driver == driver
                    && caps.node_caps() & (sys::CAP_VIDEO_M2M | sys::CAP_VIDEO_M2M_MPLANE) != 0
            })
    });
    nodes
}

/// A session on the first node of `driver` that keeps this pair of pixel
/// formats (vicodec has an encoder and a decoder node: each takes one
/// direction, and `S_FMT` adjusts a format it does not take rather than
/// failing, so the formats the driver kept decide).
fn codec(driver: &str, output: Format, capture: Format, memory: Memory) -> Option<M2mSession> {
    m2m_nodes(driver).into_iter().find_map(|path| {
        let out = QueueSetup {
            format: output,
            memory,
            buffers: 2,
            crop: None,
        };
        let cap = QueueSetup {
            format: capture,
            memory,
            buffers: 2,
            crop: None,
        };
        let s = M2mSession::open(&path, out, cap).ok()?;
        let kept = |f: &Format| match f {
            Format::Single(p) => p.pixelformat,
            Format::Multi(p) => p.pixelformat,
        };
        (kept(s.output_format()) == kept(&output) && kept(s.capture_format()) == kept(&capture))
            .then_some(s)
    })
}

fn plane(bytes: u32) -> [QueuePlane<'static>; 1] {
    [QueuePlane {
        memory: PlaneMemory::Mmap,
        length: 0,
        bytes_used: bytes,
        data_offset: 0,
    }]
}

/// vicodec: an encoder turns I420 frames into FWHT, a decoder turns them back
/// into I420. Each side is drained with its command, and the last buffer
/// comes back flagged LAST. Skipped without the module.
#[test]
fn vicodec_encodes_decodes_and_drains() {
    const W: u32 = 64;
    const H: u32 = 48;
    let yuv = fourcc(b"YU12");
    let fwht = fourcc(b"FWHT");
    let yuv_format = Format::Single(PixFormat {
        width: W,
        height: H,
        pixelformat: yuv,
        bytes_per_line: W,
        size_image: W * H * 3 / 2,
        ..Default::default()
    });
    let Some(mut enc) = codec(
        "vicodec",
        yuv_format,
        Format::Single(PixFormat {
            width: W,
            height: H,
            pixelformat: fwht,
            size_image: W * H * 3,
            ..Default::default()
        }),
        Memory::Mmap,
    ) else {
        crate::tests::skip("no vicodec encoder (modprobe vicodec)");
        return;
    };
    let frame_bytes = W * H * 3 / 2;
    for i in 0..enc.capture_buffers() {
        enc.queue_capture(i, &plane(0)).unwrap();
    }
    enc.stream_on().unwrap();
    for k in 0..2u64 {
        let index = enc.free_output().expect("a free source buffer");
        let frame: Vec<u8> = (0..frame_bytes as u64)
            .map(|i| ((i * 3 + k * 11) % 251) as u8)
            .collect();
        enc.output_plane_mut(index, 0).unwrap()[..frame_bytes as usize].copy_from_slice(&frame);
        enc.queue_output(index, &plane(frame_bytes), Duration::from_millis(k))
            .unwrap();
    }
    enc.drain(Drain::Encoder).unwrap();

    // Collect the coded frames until the drained stream's LAST buffer.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut coded: Vec<Vec<u8>> = Vec::new();
    let mut last = false;
    while !last && std::time::Instant::now() < deadline {
        enc.wait(
            Ready {
                capture: true,
                ..Ready::default()
            },
            Some(Duration::from_millis(500)),
        )
        .unwrap();
        while let Some(d) = enc.dequeue_capture().unwrap() {
            let n = d.bytes_used[0] as usize;
            if n > 0 {
                coded.push(enc.capture_plane(d.index, 0).unwrap()[..n].to_vec());
            }
            last |= d.flags & sys::BUF_FLAG_LAST != 0;
            enc.queue_capture(d.index, &plane(0)).unwrap();
        }
    }
    assert!(last, "the encoder's drain ended with a LAST buffer");
    assert!(!coded.is_empty(), "the encoder produced coded frames");
    assert!(coded.iter().all(|c| !c.is_empty()));
    enc.stream_off().unwrap();
    drop(enc);

    // The decoder takes the coded frames back and drains to LAST too.
    let coded_format = Format::Single(PixFormat {
        width: W,
        height: H,
        pixelformat: fwht,
        size_image: W * H * 3,
        ..Default::default()
    });
    let Some(mut dec) = codec(
        "vicodec",
        coded_format,
        Format::Single(PixFormat {
            width: W,
            height: H,
            pixelformat: yuv,
            bytes_per_line: W,
            size_image: frame_bytes,
            ..Default::default()
        }),
        Memory::Mmap,
    ) else {
        crate::tests::skip("no vicodec decoder node");
        return;
    };
    for i in 0..dec.capture_buffers() {
        dec.queue_capture(i, &plane(0)).unwrap();
    }
    dec.stream_on().unwrap();
    let mut decoded = 0usize;
    let mut last = false;
    let mut pending = coded.into_iter();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut drained = false;
    while !last && std::time::Instant::now() < deadline {
        while let Some(index) = dec.free_output() {
            let Some(data) = pending.next() else {
                if !drained {
                    dec.drain(Drain::Decoder).unwrap();
                    drained = true;
                }
                break;
            };
            dec.output_plane_mut(index, 0).unwrap()[..data.len()].copy_from_slice(&data);
            dec.queue_output(index, &plane(data.len() as u32), Duration::ZERO)
                .unwrap();
        }
        dec.wait(
            Ready {
                capture: true,
                ..Ready::default()
            },
            Some(Duration::from_millis(500)),
        )
        .unwrap();
        while let Some(d) = dec.dequeue_capture().unwrap() {
            if d.bytes_used[0] == frame_bytes {
                decoded += 1;
            }
            last |= d.flags & sys::BUF_FLAG_LAST != 0;
            dec.queue_capture(d.index, &plane(0)).unwrap();
        }
        while dec.dequeue_output().unwrap().is_some() {}
    }
    assert!(decoded >= 1, "the decoder produced frames");
    assert!(last, "the decoder's drain ended with a LAST buffer");
    dec.stream_off().unwrap();
}

/// Events: an idle device has none pending. vim2m takes control events for a
/// control's id (its horizontal-flip control here); a driver that refuses
/// the subscription leaves the event path unexercised, which the test says.
#[test]
fn an_idle_device_has_no_pending_events() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    const CID_HFLIP: u32 = 0x0098_0914; // V4L2_CID_HFLIP
    let s = M2mSession::open(&path, rgb_setup(Memory::Mmap), rgb_setup(Memory::Mmap)).unwrap();
    match s.subscribe(sys::EVENT_CTRL, CID_HFLIP) {
        Ok(()) => assert!(s.dequeue_event().unwrap().is_none()),
        Err(e) => crate::tests::skip(&format!(
            "vim2m refused the control subscription ({e}); events not exercised"
        )),
    }
    let ready = s
        .wait(
            Ready {
                event: true,
                ..Ready::default()
            },
            Some(Duration::from_millis(5)),
        )
        .unwrap();
    assert!(!ready.event);
}

#[test]
fn query_reads_the_driver_and_the_queue_layout_without_a_session() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    let cap = crate::v4l2::query(&path).unwrap();
    assert_eq!(cap.driver, "vim2m");
    assert!(cap.is_m2m());
    assert!(!cap.is_m2m_multiplanar(), "vim2m is single-planar");
    assert!(crate::v4l2::query("/dev/null").is_err(), "not a V4L2 node");
}

#[test]
fn find_m2m_finds_the_driver_or_nothing() {
    let Some((path, cap)) = crate::v4l2::find_m2m("vim2m").unwrap() else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    assert!(path.starts_with("/dev"));
    assert_eq!(cap.driver, "vim2m");
    assert!(
        crate::v4l2::find_m2m("no-such-driver").unwrap().is_none(),
        "an unknown driver finds no node"
    );
}

/// vim2m has no selection operation on this kernel: `S_SELECTION` is refused
/// (`ENOTTY`). A session asked for a crop therefore fails to open, with the
/// driver's error and no half-open session. The crop's success path needs a
/// driver that crops (the RGA, on a board); its ioctl layout is checked in
/// `lemnos-linux-sys`.
#[test]
fn a_crop_the_driver_refuses_fails_the_open() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    let out = QueueSetup {
        crop: Some(Rect {
            left: 0,
            top: 0,
            width: 32,
            height: 24,
        }),
        ..rgb_setup(Memory::Mmap)
    };
    let err = M2mSession::open(&path, out, rgb_setup(Memory::Mmap))
        .err()
        .expect("vim2m refuses S_SELECTION");
    assert!(err.raw_os_error().is_some(), "the driver's errno: {err}");

    // Without a crop the same device opens, and reports none.
    let s = M2mSession::open(&path, rgb_setup(Memory::Mmap), rgb_setup(Memory::Mmap)).unwrap();
    assert_eq!(s.output_crop(), None);
    assert_eq!(s.capture_crop(), None);
}

#[test]
fn set_output_crop_is_refused_where_the_driver_has_no_selection() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    let mut s = M2mSession::open(&path, rgb_setup(Memory::Mmap), rgb_setup(Memory::Mmap)).unwrap();
    let rect = Rect {
        left: 0,
        top: 0,
        width: 16,
        height: 12,
    };
    assert!(s.set_output_crop(rect).is_err());
    assert_eq!(s.output_crop(), None, "a refused change keeps no crop");
}
