//! Encoding and layout checks for the V4L2 wrappers (no device needed).

use super::*;
use std::os::fd::AsFd;

#[test]
fn fourcc_packs_little_endian() {
    assert_eq!(fourcc(b"NV12"), 0x3231_564e);
    assert_eq!(fourcc(b"RGB3"), 0x3342_4752);
}

#[test]
fn single_and_multi_formats_encode_into_the_kernel_layout() {
    let mut m = PixFormatMplane {
        width: 640,
        height: 480,
        pixelformat: fourcc(b"NV12"),
        field: 1,
        colorspace: 4,
        planes: [PlaneFormat::default(); MAX_PLANES],
        num_planes: 2,
    };
    m.planes[0] = PlaneFormat {
        size_image: 307_200,
        bytes_per_line: 640,
    };
    m.planes[1] = PlaneFormat {
        size_image: 153_600,
        bytes_per_line: 640,
    };
    let raw = encode(BufType::VideoOutputMplane, &Format::Multi(m)).unwrap();
    assert_eq!(raw.type_, BufType::VideoOutputMplane as u32);
    // Packed structures: copy each field out before comparing it.
    let mp = raw.pix_mp();
    let (pixelformat, size0, size1, planes) = (
        mp.pixelformat,
        mp.plane_fmt[0].sizeimage,
        mp.plane_fmt[1].sizeimage,
        mp.num_planes,
    );
    assert_eq!(pixelformat, fourcc(b"NV12"));
    assert_eq!((size0, size1, planes), (307_200, 153_600, 2));
    assert_eq!(decode(BufType::VideoOutputMplane, &raw), Format::Multi(m));

    let s = PixFormat {
        width: 64,
        height: 48,
        pixelformat: fourcc(b"RGB3"),
        field: 1,
        bytes_per_line: 192,
        size_image: 9216,
        colorspace: 0,
    };
    let raw = encode(BufType::VideoCapture, &Format::Single(s)).unwrap();
    assert_eq!(raw.pix().bytesperline, 192);
    assert_eq!(decode(BufType::VideoCapture, &raw), Format::Single(s));
}

#[test]
fn a_format_must_match_its_queue_layout() {
    let s = Format::Single(PixFormat::default());
    assert!(encode(BufType::VideoOutputMplane, &s).is_err());
    let m = Format::Multi(PixFormatMplane {
        num_planes: 0,
        ..Default::default()
    });
    assert!(encode(BufType::VideoOutputMplane, &m).is_err());
}

#[test]
fn timestamps_convert_both_ways() {
    let t = Duration::new(3, 250_000_000);
    let tv = timeval(t);
    assert_eq!((tv.tv_sec, tv.tv_usec), (3, 250_000));
    assert_eq!(duration_of(3, 250_000, 1_000), t);
    assert_eq!(duration_of(-1, -5, 1_000), Duration::ZERO);
}

#[test]
fn selection_calls_fail_cleanly_on_a_node_without_selection() {
    // A plain file is no V4L2 node: the calls fail, and the caller sees the errno.
    let file = test_file();
    let rect = Rect {
        left: 0,
        top: 0,
        width: 8,
        height: 8,
    };
    assert_eq!(
        s_selection(file.as_fd(), BufType::VideoOutput, SEL_TGT_CROP, rect)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOTTY)
    );
    assert_eq!(
        g_selection(file.as_fd(), BufType::VideoOutput, SEL_TGT_CROP)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOTTY)
    );
}

#[test]
fn node_caps_prefer_the_node_when_reported() {
    let c = Capability {
        driver: "vim2m".into(),
        card: String::new(),
        bus_info: String::new(),
        version: 0,
        capabilities: CAP_DEVICE_CAPS | CAP_VIDEO_M2M_MPLANE,
        device_caps: CAP_VIDEO_M2M,
    };
    assert_eq!(c.node_caps(), CAP_VIDEO_M2M);
    let plain = Capability {
        capabilities: CAP_VIDEO_M2M_MPLANE,
        ..c
    };
    assert_eq!(plain.node_caps(), CAP_VIDEO_M2M_MPLANE);
}

fn plane(bytesused: u32, length: u32, data_offset: u32, offset: u32) -> v4l2_plane {
    let mut p: v4l2_plane = zeroed();
    p.bytesused = bytesused;
    p.length = length;
    p.data_offset = data_offset;
    p.m = v4l2_m { offset };
    p
}

#[test]
fn buffer_info_decodes_only_the_planes_the_kernel_reported() {
    let mut raw: v4l2_buffer = zeroed();
    raw.index = 3;
    raw.memory = 1;
    raw.flags = 5;
    raw.length = 2;
    let planes = [
        plane(10, 4096, 0, 0x1000),
        plane(20, 8192, 2, 0x4000),
        plane(99, 99, 99, 99), // past the kernel's count: stays out
    ];
    let info = buffer_info(&raw, &planes, 2, true);
    assert_eq!((info.index, info.memory, info.flags), (3, 1, 5));
    assert_eq!(info.num_planes, 2);
    assert_eq!(info.planes[1].length, 8192);
    assert_eq!(info.planes[1].offset, 0x4000);
    assert_eq!(info.planes[2], PlaneInfo::default(), "beyond the count");

    let mut single: v4l2_buffer = zeroed();
    single.length = 4096;
    single.m = v4l2_m { offset: 0x2000 };
    single.bytesused = 12;
    let one = buffer_info(&single, &[], 1, false);
    assert_eq!(one.num_planes, 1);
    assert_eq!(one.planes[0].length, 4096);
    assert_eq!(one.planes[0].offset, 0x2000);
}

#[test]
fn dequeued_decodes_only_the_planes_the_kernel_reported() {
    let mut raw: v4l2_buffer = zeroed();
    raw.index = 1;
    raw.sequence = 7;
    raw.field = 1;
    raw.timestamp.tv_sec = 3;
    raw.timestamp.tv_usec = 250_000;
    raw.length = 1;
    let planes = [plane(100, 4096, 4, 0), plane(55, 55, 0, 0)];
    let d = dequeued_of(&raw, &planes, 1, true);
    assert_eq!(d.index, 1);
    assert_eq!(d.num_planes, 1);
    assert_eq!(d.bytes_used[0], 100);
    assert_eq!(d.data_offset[0], 4);
    assert_eq!(d.bytes_used[1], 0, "past the count");
    assert_eq!(d.timestamp, Duration::new(3, 250_000_000));
    assert_eq!((d.field, d.sequence), (1, 7));
}

#[test]
fn a_crafted_dequeued_index_is_refused_against_the_queue() {
    let mut d = dequeued_of(&zeroed::<v4l2_buffer>(), &[], 0, true);
    d.index = 3;
    assert_eq!(d.checked_index(4).unwrap(), 3);
    assert_eq!(
        d.checked_index(3).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    d.index = u32::MAX;
    assert!(d.checked_index(4).is_err());
}

#[test]
fn a_payload_range_stays_inside_the_mapped_plane() {
    assert_eq!(payload_range(100, 4, 4096).unwrap(), 4..100);
    assert_eq!(payload_range(4096, 0, 4096).unwrap(), 0..4096);
    assert_eq!(payload_range(0, 0, 0).unwrap(), 0..0);
    for (used, offset, capacity) in [
        (4097, 0, 4096),      // bytes_used past the plane
        (100, 101, 4096),     // data_offset past the data
        (u32::MAX, 0, 4096),  // a crafted huge count
        (10, u32::MAX, 4096), // a crafted offset
        (1, 0, 0),            // nothing mapped
    ] {
        let err = payload_range(used, offset, capacity).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "{used} {offset} {capacity}"
        );
    }
}

#[test]
fn a_crafted_timestamp_saturates_instead_of_panicking() {
    assert!(duration_of(i64::MAX, i64::MAX, 1_000) > Duration::ZERO);
    assert_eq!(
        duration_of(0, u32::MAX as i64, 1),
        Duration::from_nanos(u64::from(u32::MAX))
    );
}

fn dmabuf_plane(fd: BorrowedFd<'_>) -> QueuePlane<'_> {
    QueuePlane {
        memory: PlaneMemory::Dmabuf(fd),
        length: 4096,
        bytes_used: 0,
        data_offset: 0,
    }
}

const MMAP_PLANE: QueuePlane<'static> = QueuePlane {
    memory: PlaneMemory::Mmap,
    length: 0,
    bytes_used: 0,
    data_offset: 0,
};

#[test]
fn queueing_is_refused_before_the_kernel_when_the_plane_disagrees_with_the_queue() {
    let file = test_file();
    let fd = file.as_fd();
    let t = Duration::ZERO;
    let bad_input = |r: io::Result<()>| r.unwrap_err().kind() == io::ErrorKind::InvalidInput;
    // User-pointer memory is never accepted, whatever the plane says.
    assert!(bad_input(qbuf(
        fd,
        BufType::VideoOutput,
        Memory::UserPtr,
        0,
        &[MMAP_PLANE],
        t
    )));
    // A DMABUF queue with an MMAP plane would make the kernel read `m` as a descriptor.
    assert!(bad_input(qbuf(
        fd,
        BufType::VideoOutput,
        Memory::DmaBuf,
        0,
        &[MMAP_PLANE],
        t
    )));
    assert!(bad_input(qbuf(
        fd,
        BufType::VideoOutput,
        Memory::Mmap,
        0,
        &[dmabuf_plane(fd)],
        t,
    )));
    // bytes_used past the plane's length, data_offset past bytes_used.
    for (bytes_used, data_offset) in [(4097, 0), (10, 11)] {
        let plane = QueuePlane {
            bytes_used,
            data_offset,
            ..dmabuf_plane(fd)
        };
        assert!(bad_input(qbuf(
            fd,
            BufType::VideoOutputMplane,
            Memory::DmaBuf,
            0,
            &[plane],
            t
        )));
    }
    // A well-formed buffer reaches the kernel: a plain file answers ENOTTY.
    let ok = qbuf(
        fd,
        BufType::VideoOutput,
        Memory::DmaBuf,
        0,
        &[dmabuf_plane(fd)],
        t,
    );
    assert_eq!(ok.unwrap_err().raw_os_error(), Some(libc::ENOTTY));
}

#[test]
fn a_multi_planar_queue_needs_between_one_and_eight_planes() {
    let file = test_file();
    let fd = file.as_fd();
    let t = Duration::ZERO;
    let planes = [MMAP_PLANE; MAX_PLANES + 1];
    for count in [0, MAX_PLANES + 1] {
        let err = qbuf(
            fd,
            BufType::VideoOutputMplane,
            Memory::Mmap,
            0,
            &planes[..count],
            t,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{count}");
    }
    let err = dqbuf(fd, BufType::VideoCaptureMplane, Memory::Mmap, 0).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn a_dequeue_with_no_kernel_behind_it_reports_the_errno() {
    let file = test_file();
    let err = dqbuf(file.as_fd(), BufType::VideoCapture, Memory::Mmap, 1).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
}

#[test]
fn queue_and_decode_allocate_nothing() {
    use crate::test_alloc::allocations;
    let file = test_file();
    let fd = file.as_fd();
    let planes = [dmabuf_plane(fd), dmabuf_plane(fd)];
    let raw: v4l2_buffer = zeroed();
    let kernel = [zeroed::<v4l2_plane>(); MAX_PLANES];
    let (_, n) = allocations(|| {
        let _ = qbuf(
            fd,
            BufType::VideoOutputMplane,
            Memory::DmaBuf,
            0,
            &planes,
            Duration::ZERO,
        );
        let _ = dqbuf(fd, BufType::VideoCaptureMplane, Memory::Mmap, 2);
        dequeued_of(&raw, &kernel, 2, true)
    });
    assert_eq!(n, 0);
}

#[test]
fn a_controls_call_of_a_frames_size_allocates_nothing() {
    use crate::test_alloc::allocations;
    let file = test_file();
    let mut payload_a = [0u8; 40];
    let mut payload_b = [0u8; 64];
    let (_, n) = allocations(|| {
        let mut controls = [
            device::ExtControl::payload(1, &mut payload_a),
            device::ExtControl::payload(2, &mut payload_b),
            device::ExtControl::value(3, 0),
        ];
        let _ = s_ext_ctrls_in_place(file.as_fd(), None, &mut controls);
    });
    assert_eq!(n, 0);
}
