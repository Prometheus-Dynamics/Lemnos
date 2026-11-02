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
fn vicodec_encodes_and_drains() {
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
}

/// A decoder session on vicodec: the output (coded FWHT) format is fixed, the
/// capture (I420) buffers are deferred until the stream says its size. The
/// coded buffers take the size given here, so give the largest stream.
fn vicodec_decoder(w: u32, h: u32) -> Option<M2mSession> {
    let fwht = Format::Single(PixFormat {
        width: w,
        height: h,
        pixelformat: fourcc(b"FWHT"),
        size_image: w * h * 3,
        ..Default::default()
    });
    let yuv = Format::Single(PixFormat {
        width: w,
        height: h,
        pixelformat: fourcc(b"YU12"),
        bytes_per_line: w,
        size_image: w * h * 3 / 2,
        ..Default::default()
    });
    m2m_nodes("vicodec").into_iter().find_map(|path| {
        let out = QueueSetup {
            format: fwht,
            memory: Memory::Mmap,
            buffers: 4,
            crop: None,
        };
        let cap = QueueSetup {
            format: yuv,
            memory: Memory::Mmap,
            buffers: 0,
            crop: None,
        };
        let s = M2mSession::open(&path, out, cap).ok()?;
        let kept = |f: &Format, code: &[u8; 4]| match f {
            Format::Single(p) => p.pixelformat == fourcc(code),
            Format::Multi(p) => p.pixelformat == fourcc(code),
        };
        (kept(s.output_format(), b"FWHT") && kept(s.capture_format(), b"YU12")).then_some(s)
    })
}

/// The coded frames vicodec's encoder makes from `frames` I420 frames of `w`x`h`.
fn vicodec_encode(w: u32, h: u32, frames: u64) -> Option<Vec<Vec<u8>>> {
    let yuv = Format::Single(PixFormat {
        width: w,
        height: h,
        pixelformat: fourcc(b"YU12"),
        bytes_per_line: w,
        size_image: w * h * 3 / 2,
        ..Default::default()
    });
    let fwht = Format::Single(PixFormat {
        width: w,
        height: h,
        pixelformat: fourcc(b"FWHT"),
        size_image: w * h * 3,
        ..Default::default()
    });
    let mut enc = codec("vicodec", yuv, fwht, Memory::Mmap)?;
    // The frames are the size the driver kept: vicodec takes most requests as
    // 640x368 and keeps 1280x720 as asked, so the requested size is not the frame.
    let yuv_len = match enc.output_format() {
        Format::Single(p) => p.size_image as usize,
        Format::Multi(p) => p.planes[0].size_image as usize,
    };
    for i in 0..enc.capture_buffers() {
        enc.queue_capture(i, &plane(0)).unwrap();
    }
    enc.stream_on().unwrap();
    for k in 0..frames {
        let index = enc.free_output().expect("a free source buffer");
        let frame: Vec<u8> = (0..yuv_len as u64)
            .map(|i| ((i * 3 + k * 11) % 251) as u8)
            .collect();
        enc.output_plane_mut(index, 0).unwrap()[..yuv_len].copy_from_slice(&frame);
        enc.queue_output(index, &plane(yuv_len as u32), Duration::from_millis(k))
            .unwrap();
    }
    enc.drain(Drain::Encoder).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut coded = Vec::new();
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
    Some(coded)
}

/// Feeds `coded` to a decoder and returns the size (bytes used) of each decoded
/// frame, in order, and how many source changes arrived. The first change
/// allocates the capture buffers (`adopt_capture`); a later resolution change
/// is reconfigured after a drain (`drain`, the LAST buffer, `reconfigure_capture`)
/// without touching the output queue, then the stream resumes.
fn decode_stream(
    dec: &mut M2mSession,
    coded: &[Vec<u8>],
    buffers: u32,
    expected_changes: usize,
) -> (Vec<u32>, usize) {
    // The output queue starts now; the capture queue starts once its buffers exist.
    dec.stream_on().unwrap();
    let mut pending = coded.iter();
    let mut sizes = Vec::new();
    let mut changes = 0usize;
    let mut want_reconfigure = false;
    let mut end_drain_sent = false;
    let mut end = false;
    // The capture queue is polled only once it streams: a driver reports an
    // error for a queue that is not streaming.
    let mut capture_live = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !end && std::time::Instant::now() < deadline {
        // Once every coded frame is queued and every expected source change is
        // handled, drain: the LAST buffer ends the loop. A drain sent earlier would
        // collide with the drain a resolution change needs (EBUSY).
        if !end_drain_sent
            && pending.as_slice().is_empty()
            && changes >= expected_changes
            && !want_reconfigure
        {
            dec.drain(Drain::Decoder).unwrap();
            end_drain_sent = true;
        }
        while let Some(index) = dec.free_output() {
            let Some(data) = pending.next() else {
                break;
            };
            dec.output_plane_mut(index, 0).unwrap()[..data.len()].copy_from_slice(data);
            dec.queue_output(index, &plane(data.len() as u32), Duration::ZERO)
                .unwrap();
        }
        dec.wait(
            Ready {
                capture: capture_live,
                event: true,
                ..Ready::default()
            },
            Some(Duration::from_millis(500)),
        )
        .unwrap();
        if let Some(change) = dec.take_source_change().unwrap() {
            changes += 1;
            assert!(
                change.resolution(),
                "a decoder's source change is a resolution change"
            );
            if dec.capture_pending() {
                dec.adopt_capture(buffers).unwrap();
                for i in 0..dec.capture_buffers() {
                    dec.queue_capture(i, &plane(0)).unwrap();
                }
                dec.start_capture().unwrap();
                capture_live = true;
            } else {
                // Mid-stream: the stream is drained first, its LAST taken below.
                dec.drain(Drain::Decoder).unwrap();
                want_reconfigure = true;
            }
        }
        // A queue that is not streaming has nothing to dequeue (EINVAL).
        while let Some(d) = if capture_live {
            dec.dequeue_capture().unwrap()
        } else {
            None
        } {
            // A drain's LAST buffer may be empty: it is the end marker, not a frame.
            if d.bytes_used[0] > 0 {
                sizes.push(d.bytes_used[0]);
            }
            if end_drain_sent && d.flags & sys::BUF_FLAG_LAST != 0 {
                end = true;
            }
            if !(want_reconfigure && dec.capture_drained() && d.flags & sys::BUF_FLAG_LAST != 0) {
                dec.queue_capture(d.index, &plane(0)).unwrap();
            }
        }
        if want_reconfigure && dec.capture_drained() {
            dec.reconfigure_capture(buffers).unwrap();
            for i in 0..dec.capture_buffers() {
                dec.queue_capture(i, &plane(0)).unwrap();
            }
            dec.start_capture().unwrap();
            want_reconfigure = false;
            capture_live = true;
        }
    }
    assert!(
        end,
        "the decoder's drain ended with a LAST buffer before the deadline: {sizes:?}"
    );
    (sizes, changes)
}

/// vicodec's frame size: the driver takes most requests as 640x368 (I420,
/// 353280 bytes).
const VICODEC_FRAME: u32 = 640 * 368 * 3 / 2;

/// The decoder round trip: vicodec's encoder makes coded frames, its decoder
/// takes them back. The capture size follows the stream: the first source
/// change allocates the capture buffers, and the decoded frames are the coded
/// frames' size.
#[test]
fn vicodec_decodes_the_encoders_frames() {
    let Some(coded) = vicodec_encode(64, 48, 2) else {
        crate::tests::skip("no vicodec encoder (modprobe vicodec)");
        return;
    };
    let Some(mut dec) = vicodec_decoder(1280, 720) else {
        crate::tests::skip("no vicodec decoder node");
        return;
    };
    assert!(
        dec.capture_pending(),
        "the capture buffers wait for the stream's size"
    );
    let (sizes, changes) = decode_stream(&mut dec, &coded, 4, 1);
    assert_eq!(changes, 1, "one source change: the first size");
    assert!(!sizes.is_empty(), "the decoder produced frames");
    assert!(
        sizes.iter().all(|&n| n == VICODEC_FRAME),
        "frames are the stream's size: {sizes:?}"
    );
}

/// A mid-stream resolution change: a 640x368 stream, then a 1280x720 stream on
/// the same decoder. The second change is reconfigured after a drain; the
/// output queue is not torn down. Frames come back at each size.
#[test]
fn vicodec_decoder_follows_a_mid_stream_resolution_change() {
    let (Some(first), Some(second)) = (vicodec_encode(64, 48, 2), vicodec_encode(1280, 720, 2))
    else {
        crate::tests::skip("no vicodec encoder (modprobe vicodec)");
        return;
    };
    let Some(mut dec) = vicodec_decoder(1280, 720) else {
        crate::tests::skip("no vicodec decoder node");
        return;
    };
    let coded: Vec<Vec<u8>> = first.into_iter().chain(second).collect();
    let (sizes, changes) = decode_stream(&mut dec, &coded, 4, 2);
    assert_eq!(
        changes, 2,
        "the first size and the mid-stream change: {sizes:?}"
    );
    assert!(sizes.contains(&VICODEC_FRAME), "640x368 frames: {sizes:?}");
    assert!(
        sizes.contains(&(1280 * 720 * 3 / 2)),
        "1280x720 frames after the change: {sizes:?}"
    );
}

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

/// A session opened from a descriptor the caller made: the same queues as
/// the path form, the descriptor kept, and controls reachable through it.
#[test]
fn a_session_opens_from_a_file_descriptor_and_keeps_it() {
    let Some(path) = find_m2m("vim2m") else {
        crate::tests::skip("no vim2m node (modprobe vim2m)");
        return;
    };
    let by_path =
        M2mSession::open(&path, rgb_setup(Memory::Mmap), rgb_setup(Memory::Mmap)).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let by_file =
        M2mSession::from_file(file, rgb_setup(Memory::Mmap), rgb_setup(Memory::Mmap)).unwrap();
    assert_eq!(by_file.output_format(), by_path.output_format());
    assert_eq!(by_file.capture_format(), by_path.capture_format());
    assert_eq!(by_file.output_buffers(), by_path.output_buffers());
    // The session's descriptor is the one it was given: querycap through it works.
    let cap = sys::querycap(by_file.as_fd()).unwrap();
    assert_eq!(cap.driver, "vim2m");

    let owned = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let from_owned = M2mSession::from_file(
        std::os::fd::OwnedFd::from(owned),
        rgb_setup(Memory::Mmap),
        rgb_setup(Memory::Mmap),
    )
    .unwrap();
    assert_eq!(from_owned.capture_format(), by_path.capture_format());
}

/// The capture format a session reports is the driver's own: read back after
/// the source format is set, so an encoder that resized its coded buffers
/// (`sizeimage`) from the source is not reported stale.
#[test]
fn the_capture_format_is_read_back_after_the_source_is_set() {
    const W: u32 = 64;
    const H: u32 = 48;
    let yuv = Format::Single(PixFormat {
        width: W,
        height: H,
        pixelformat: fourcc(b"YU12"),
        bytes_per_line: W,
        size_image: W * H * 3 / 2,
        ..Default::default()
    });
    let fwht = Format::Single(PixFormat {
        width: W,
        height: H,
        pixelformat: fourcc(b"FWHT"),
        size_image: W * H * 3,
        ..Default::default()
    });
    let Some(enc) = codec("vicodec", yuv, fwht, Memory::Mmap) else {
        crate::tests::skip("no vicodec encoder (modprobe vicodec)");
        return;
    };
    // What the driver reports now for the capture queue, after both formats
    // were set, must be what the session stores.
    let buf_type = if enc.capture_format().is_multiplanar() {
        sys::BufType::VideoCaptureMplane
    } else {
        sys::BufType::VideoCapture
    };
    let driver = sys::g_fmt(enc.as_fd(), buf_type).unwrap();
    assert_eq!(
        *enc.capture_format(),
        driver,
        "stored capture format is current"
    );
}

/// An encoder's coded size follows the source format. The session sets the
/// source first, then reads the coded format back: the stored capture format
/// must equal what the driver reports for that sequence. The raw sequence in
/// the other order (coded format first) shows the coded size changes once the
/// source is set, which is why the session reads the format back.
#[test]
fn the_capture_size_follows_the_source_and_the_session_reports_that() {
    use std::os::fd::AsFd;
    const W: u32 = 64;
    const H: u32 = 48;
    let fwht = Format::Single(PixFormat {
        width: W,
        height: H,
        pixelformat: fourcc(b"FWHT"),
        size_image: W * H * 3,
        ..Default::default()
    });
    let yuv = Format::Single(PixFormat {
        width: W,
        height: H,
        pixelformat: fourcc(b"YU12"),
        bytes_per_line: W,
        size_image: W * H * 3 / 2,
        ..Default::default()
    });
    let size = |f: &Format| match f {
        Format::Single(p) => p.size_image,
        Format::Multi(p) => p.planes[0].size_image,
    };
    let Some(path) = m2m_nodes("vicodec").into_iter().find(|path| {
        let Ok(file) = sys::open_node(path) else {
            return false;
        };
        sys::s_fmt(file.as_fd(), sys::BufType::VideoOutput, &yuv)
            .is_ok_and(|f| matches!(f, Format::Single(p) if p.pixelformat == fourcc(b"YU12")))
            && sys::s_fmt(file.as_fd(), sys::BufType::VideoCapture, &fwht)
                .is_ok_and(|f| matches!(f, Format::Single(p) if p.pixelformat == fourcc(b"FWHT")))
    }) else {
        crate::tests::skip("no vicodec encoder node (modprobe vicodec)");
        return;
    };

    // The coded format in the other order: set first, then the source. The
    // size changes, so reading the format back is not a no-op.
    let raw = sys::open_node(&path).unwrap();
    let coded_first = sys::s_fmt(raw.as_fd(), sys::BufType::VideoCapture, &fwht).unwrap();
    sys::s_fmt(raw.as_fd(), sys::BufType::VideoOutput, &yuv).unwrap();
    let coded_after = sys::g_fmt(raw.as_fd(), sys::BufType::VideoCapture).unwrap();
    assert_ne!(
        size(&coded_first),
        size(&coded_after),
        "the driver resizes the coded buffer"
    );
    drop(raw);

    // The session's order: the source, then the coded format. Its stored
    // format must be the driver's answer for that sequence.
    let session = M2mSession::open(
        &path,
        QueueSetup {
            format: yuv,
            memory: Memory::Mmap,
            buffers: 2,
            crop: None,
        },
        QueueSetup {
            format: fwht,
            memory: Memory::Mmap,
            buffers: 2,
            crop: None,
        },
    )
    .unwrap();
    let driver = sys::g_fmt(session.as_fd(), sys::BufType::VideoCapture).unwrap();
    assert_eq!(
        *session.capture_format(),
        driver,
        "the stored format is the driver's now"
    );
    assert!(size(&driver) > 0);
}
