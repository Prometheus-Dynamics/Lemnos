//! Layout and encoding checks for the V4L2 uAPI (no device needed): the numbers
//! are the kernel headers', printed by a C program on x86_64 and aarch64.

use std::mem::offset_of;
use std::os::fd::AsFd;

use super::uapi::{v4l2_ext_control, v4l2_ext_controls};
use super::*;

#[test]
fn structure_sizes_match_the_kernel_header() {
    assert_eq!(size_of::<v4l2_capability>(), 104);
    assert_eq!(size_of::<v4l2_format>(), 208);
    assert_eq!(size_of::<v4l2_fmtdesc>(), 64);
    assert_eq!(size_of::<v4l2_frmsizeenum>(), 44);
    assert_eq!(size_of::<v4l2_frmivalenum>(), 52);
    assert_eq!(size_of::<v4l2_pix_format>(), 48);
    assert_eq!(size_of::<v4l2_pix_format_mplane>(), 192);
    assert_eq!(size_of::<v4l2_plane_pix_format>(), 20);
    assert_eq!(size_of::<v4l2_meta_format>(), 20);
    assert_eq!(size_of::<v4l2_streamparm>(), 204);
    assert_eq!(size_of::<v4l2_requestbuffers>(), 20);
    assert_eq!(size_of::<v4l2_plane>(), 64);
    assert_eq!(size_of::<v4l2_timecode>(), 16);
    assert_eq!(size_of::<v4l2_buffer>(), 88);
    assert_eq!(size_of::<v4l2_exportbuffer>(), 64);
    assert_eq!(size_of::<v4l2_encoder_cmd>(), 40);
    assert_eq!(size_of::<v4l2_decoder_cmd>(), 72);
    assert_eq!(size_of::<v4l2_control>(), 8);
    assert_eq!(size_of::<v4l2_queryctrl>(), 68);
    assert_eq!(size_of::<v4l2_query_ext_ctrl>(), 232);
    assert_eq!(size_of::<v4l2_querymenu>(), 44);
    assert_eq!(size_of::<v4l2_ext_control>(), 20);
    assert_eq!(size_of::<v4l2_ext_controls>(), 32);
    assert_eq!(size_of::<ExtControl<'_>>(), 20);
    assert_eq!(size_of::<v4l2_event_subscription>(), 32);
    assert_eq!(size_of::<v4l2_event>(), 136);
    assert_eq!(size_of::<v4l2_selection>(), 64);
}

#[test]
fn field_offsets_match_the_kernel_header() {
    assert_eq!(offset_of!(v4l2_capability, card), 16);
    assert_eq!(offset_of!(v4l2_capability, bus_info), 48);
    assert_eq!(offset_of!(v4l2_capability, device_caps), 88);
    assert_eq!(offset_of!(v4l2_format, fmt), 8);
    assert_eq!(offset_of!(v4l2_pix_format_mplane, num_planes), 180);
    assert_eq!(offset_of!(v4l2_buffer, timestamp), 24);
    assert_eq!(offset_of!(v4l2_buffer, timecode), 40);
    assert_eq!(offset_of!(v4l2_buffer, sequence), 56);
    assert_eq!(offset_of!(v4l2_buffer, memory), 60);
    assert_eq!(offset_of!(v4l2_buffer, m), 64);
    assert_eq!(offset_of!(v4l2_buffer, length), 72);
    assert_eq!(offset_of!(v4l2_buffer, request_fd), 80);
    assert_eq!(offset_of!(v4l2_plane, m), 8);
    assert_eq!(offset_of!(v4l2_plane, data_offset), 16);
    assert_eq!(offset_of!(v4l2_exportbuffer, fd), 16);
    assert_eq!(offset_of!(v4l2_decoder_cmd, data), 8);
    assert_eq!(offset_of!(v4l2_query_ext_ctrl, minimum), 40);
    assert_eq!(offset_of!(v4l2_query_ext_ctrl, dims), 88);
    assert_eq!(offset_of!(v4l2_ext_controls, controls), 24);
    assert_eq!(offset_of!(v4l2_event, u), 8);
    assert_eq!(offset_of!(v4l2_event, pending), 72);
    assert_eq!(offset_of!(v4l2_event, sequence), 76);
    assert_eq!(offset_of!(v4l2_event, timestamp), 80);
    assert_eq!(offset_of!(v4l2_event, id), 96);
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn request_numbers_match_the_kernel_header() {
    assert_eq!(VIDIOC_QUERYCAP.nr(), 0x8068_5600);
    assert_eq!(VIDIOC_ENUM_FMT.nr(), 0xc040_5602);
    assert_eq!(VIDIOC_G_FMT.nr(), 0xc0d0_5604);
    assert_eq!(VIDIOC_S_FMT.nr(), 0xc0d0_5605);
    assert_eq!(VIDIOC_TRY_FMT.nr(), 0xc0d0_5640);
    assert_eq!(VIDIOC_REQBUFS.nr(), 0xc014_5608);
    assert_eq!(VIDIOC_QUERYBUF.nr(), 0xc058_5609);
    assert_eq!(VIDIOC_QBUF.nr(), 0xc058_560f);
    assert_eq!(VIDIOC_PREPARE_BUF.nr(), 0xc058_565d);
    assert_eq!(VIDIOC_DQBUF.nr(), 0xc058_5611);
    assert_eq!(VIDIOC_STREAMON.nr(), 0x4004_5612);
    assert_eq!(VIDIOC_STREAMOFF.nr(), 0x4004_5613);
    assert_eq!(VIDIOC_G_PARM.nr(), 0xc0cc_5615);
    assert_eq!(VIDIOC_S_PARM.nr(), 0xc0cc_5616);
    assert_eq!(VIDIOC_G_CTRL.nr(), 0xc008_561b);
    assert_eq!(VIDIOC_S_CTRL.nr(), 0xc008_561c);
    assert_eq!(VIDIOC_QUERYMENU.nr(), 0xc02c_5625);
    assert_eq!(VIDIOC_G_EXT_CTRLS.nr(), 0xc020_5647);
    assert_eq!(VIDIOC_S_EXT_CTRLS.nr(), 0xc020_5648);
    assert_eq!(VIDIOC_TRY_EXT_CTRLS.nr(), 0xc020_5649);
    assert_eq!(VIDIOC_ENUM_FRAMESIZES.nr(), 0xc02c_564a);
    assert_eq!(VIDIOC_ENUM_FRAMEINTERVALS.nr(), 0xc034_564b);
    assert_eq!(VIDIOC_ENCODER_CMD.nr(), 0xc028_564d);
    assert_eq!(VIDIOC_DECODER_CMD.nr(), 0xc048_5660);
    assert_eq!(VIDIOC_DQEVENT.nr(), 0x8088_5659);
    assert_eq!(VIDIOC_SUBSCRIBE_EVENT.nr(), 0x4020_565a);
    assert_eq!(VIDIOC_UNSUBSCRIBE_EVENT.nr(), 0x4020_565b);
    assert_eq!(VIDIOC_G_SELECTION.nr(), 0xc040_565e);
    assert_eq!(VIDIOC_S_SELECTION.nr(), 0xc040_565f);
    assert_eq!(VIDIOC_QUERY_EXT_CTRL.nr(), 0xc0e8_5667);
}

#[test]
fn overlay_formats_never_reach_the_kernel() {
    let file = std::fs::File::open("/dev/null").unwrap();
    for type_ in [3, 8] {
        let mut raw: v4l2_format = zeroed();
        raw.type_ = type_;
        let e = VIDIOC_G_FMT.call(file.as_fd(), &mut raw).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EINVAL), "{type_}");
    }
    let mut raw: v4l2_format = zeroed();
    raw.type_ = 1;
    let e = VIDIOC_G_FMT.call(file.as_fd(), &mut raw).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::ENOTTY), "not a V4L2 node");
}

#[test]
fn union_members_read_back() {
    let mut raw: v4l2_format = zeroed();
    raw.fmt.pix = v4l2_pix_format {
        width: 64,
        ..Default::default()
    };
    assert_eq!(raw.pix().width, 64);
    assert_eq!({ raw.pix_mp().width }, 64);
    let mut parm: v4l2_streamparm = zeroed();
    parm.parm.capture.timeperframe = Fraction::new(1, 30);
    assert_eq!(parm.capture().timeperframe, Fraction::new(1, 30));
}

#[test]
fn a_buffer_call_refuses_user_pointers_and_keeps_no_plane_pointer() {
    let file = std::fs::File::open("/dev/null").unwrap();
    let fd = file.as_fd();
    let mut planes: [v4l2_plane; 2] = zeroed();
    let mut buf: v4l2_buffer = zeroed();
    buf.type_ = 9;
    buf.memory = 2;
    let e = VIDIOC_QBUF.call(fd, &mut buf, &mut planes).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EINVAL), "user pointers");
    buf.memory = 1;
    let e = VIDIOC_QBUF.call(fd, &mut buf, &mut planes).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::ENOTTY), "reached the kernel");
    assert_eq!(buf.length, 2);
    assert_eq!(buf.offset(), 0, "the plane pointer is gone");
    // A single-planar queue has no plane array.
    buf.type_ = 1;
    let e = VIDIOC_QBUF.call(fd, &mut buf, &mut planes).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EINVAL));
    // Nor can more planes than the kernel's limit go in.
    buf.type_ = 9;
    let mut many: [v4l2_plane; VIDEO_MAX_PLANES + 1] = zeroed();
    let e = VIDIOC_QBUF.call(fd, &mut buf, &mut many).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EINVAL));
}

#[test]
fn ext_controls_hold_their_values() {
    let negative = ExtControl::value(7, -5);
    assert_eq!((negative.id(), negative.get_value()), (7, -5));
    assert_eq!(negative.size(), 0);
    let wide = ExtControl::value64(8, 1 << 40);
    assert_eq!(wide.get_value64(), 1 << 40);
    let mut bytes = [0u8; 12];
    let payload = ExtControl::payload(9, &mut bytes);
    assert_eq!(payload.size(), 12);
}

#[test]
fn a_refused_controls_call_keeps_the_error() {
    let file = std::fs::File::open("/dev/null").unwrap();
    let mut controls = [ExtControl::value(7, 1)];
    let e = VIDIOC_S_EXT_CTRLS
        .call(file.as_fd(), 0, None, &mut controls)
        .unwrap_err();
    assert_eq!(e.source.raw_os_error(), Some(libc::ENOTTY));
}

#[test]
fn nonblocking_goes_both_ways() {
    let (reader, _writer) = std::io::pipe().unwrap();
    let fd = reader.as_fd();
    let flags = || {
        std::fs::read_to_string(format!(
            "/proc/self/fdinfo/{}",
            std::os::fd::AsRawFd::as_raw_fd(&fd)
        ))
        .unwrap()
    };
    let nonblock = |text: String| {
        let line = text.lines().find(|l| l.starts_with("flags:")).unwrap();
        let bits = i32::from_str_radix(line.split_whitespace().nth(1).unwrap(), 8).unwrap();
        bits & libc::O_NONBLOCK != 0
    };
    set_nonblocking(fd, true).unwrap();
    assert!(nonblock(flags()));
    set_nonblocking(fd, true).unwrap();
    set_nonblocking(fd, false).unwrap();
    assert!(!nonblock(flags()));
}

#[test]
fn from_bytes_fills_and_zeroes() {
    let raw: v4l2_control = from_bytes(&7u32.to_ne_bytes());
    assert_eq!((raw.id, raw.value), (7, 0));
}
