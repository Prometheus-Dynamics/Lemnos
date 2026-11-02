//! Encoding and layout checks for the V4L2 wrappers (no device needed).

use super::*;
use std::os::fd::AsFd;

#[test]
fn fourcc_packs_little_endian() {
    assert_eq!(fourcc(b"NV12"), 0x3231_564e);
    assert_eq!(fourcc(b"RGB3"), 0x3342_4752);
}

#[test]
fn single_and_multi_formats_encode_at_the_kernel_offsets() {
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
    assert_eq!(uapi::get_u32(&raw.fmt.0, 8), fourcc(b"NV12"));
    assert_eq!(uapi::get_u32(&raw.fmt.0, 20), 307_200);
    assert_eq!(uapi::get_u32(&raw.fmt.0, 24), 640);
    assert_eq!(uapi::get_u32(&raw.fmt.0, 40), 153_600);
    assert_eq!(raw.fmt.0[180], 2);
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
    assert_eq!(uapi::get_u32(&raw.fmt.0, 16), 192);
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
    // /dev/null is no V4L2 node: the calls fail, and the caller sees the errno.
    let file = std::fs::File::open("/dev/null").unwrap();
    let rect = Rect {
        left: 0,
        top: 0,
        width: 8,
        height: 8,
    };
    assert!(s_selection(file.as_fd(), BufType::VideoOutput, SEL_TGT_CROP, rect).is_err());
    assert!(g_selection(file.as_fd(), BufType::VideoOutput, SEL_TGT_CROP).is_err());
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
