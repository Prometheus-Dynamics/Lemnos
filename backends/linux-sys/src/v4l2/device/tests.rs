//! Layout and encoding checks for the V4L2 uAPI (no device needed): the numbers
//! are the kernel headers', printed by a C program on x86_64 and aarch64.

use std::mem::offset_of;
use std::os::fd::AsFd;

use super::calls::returned_planes;
use super::uapi::{v4l2_ext_control, v4l2_ext_controls};
use super::*;

#[test]
fn structure_sizes_match_the_kernel_header() {
    assert_eq!(size_of::<v4l2_capability>(), 104);
    assert_eq!(size_of::<v4l2_format>(), 208);
    assert_eq!(size_of::<v4l2_create_buffers>(), 256);
    assert_eq!(core::mem::offset_of!(v4l2_create_buffers, format), 16);
    assert_eq!(
        core::mem::offset_of!(v4l2_create_buffers, capabilities),
        224
    );
    assert_eq!(size_of::<v4l2_fmtdesc>(), 64);
    // The C compiler's layout of `struct v4l2_input` (80 bytes, `std` at 48).
    assert_eq!(size_of::<v4l2_input>(), 80);
    assert_eq!(core::mem::offset_of!(v4l2_input, name), 4);
    assert_eq!(core::mem::offset_of!(v4l2_input, type_), 36);
    assert_eq!(core::mem::offset_of!(v4l2_input, std_), 48);
    assert_eq!(core::mem::offset_of!(v4l2_input, status), 56);
    assert_eq!(core::mem::offset_of!(v4l2_input, reserved), 64);
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
    assert_eq!(VIDIOC_EXPBUF.nr(), 0xc040_5610);
    assert_eq!(VIDIOC_QUERYCAP.nr(), 0x8068_5600);
    assert_eq!(VIDIOC_ENUM_FMT.nr(), 0xc040_5602);
    assert_eq!(VIDIOC_G_FMT.nr(), 0xc0d0_5604);
    #[cfg(target_pointer_width = "64")]
    assert_eq!(VIDIOC_CREATE_BUFS.nr(), 0xc100_565c);
    #[cfg(target_pointer_width = "32")]
    assert_eq!(VIDIOC_CREATE_BUFS.nr(), 0xc0f8_565c);
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
    assert_eq!(VIDIOC_G_INPUT.nr(), 0x8004_5626);
    assert_eq!(VIDIOC_S_INPUT.nr(), 0xc004_5627);
    assert_eq!(VIDIOC_ENUMINPUT.nr(), 0xc050_561a);
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
    let file = crate::v4l2::test_file();
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
    let file = crate::v4l2::test_file();
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
    // No plane array at all is refused, and so is one past the kernel's limit.
    let e = VIDIOC_QBUF.call(fd, &mut buf, &mut []).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EINVAL), "no planes");
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

fn raw_ext_control(size: u32, value64: i64) -> v4l2_ext_control {
    let mut raw: v4l2_ext_control = zeroed();
    raw.size = size;
    raw.u = uapi::v4l2_ext_control_value { value64 };
    raw
}

#[test]
fn a_payload_control_keeps_its_own_length_and_pointer_after_the_kernel_writes() {
    let mut bytes = [0u8; 24];
    let address = bytes.as_mut_ptr() as usize as i64;
    let mut control = ExtControl::payload(9, &mut bytes);
    // A kernel reporting a huge size and a different address (as if `size` said
    // ENOSPC and the union were rewritten): neither may reach the next call.
    control.absorb(&raw_ext_control(u32::MAX, 0x4141_4141_4141));
    assert_eq!(control.reported_size(), u32::MAX);
    assert_eq!(
        control.size(),
        24,
        "the next call still passes the borrowed length"
    );
    assert_eq!(
        control.get_value64(),
        address,
        "the next call still passes the borrowed address"
    );
}

#[test]
fn a_value_control_takes_the_value_the_kernel_wrote() {
    let mut control = ExtControl::value64(8, 1);
    control.absorb(&raw_ext_control(8, -77));
    assert_eq!(control.get_value64(), -77);
    assert_eq!(control.size(), 0, "a value control never claims a payload");
}

#[test]
fn an_empty_payload_hands_the_kernel_no_address() {
    let mut bytes: [u8; 0] = [];
    let control = ExtControl::payload(9, &mut bytes);
    assert_eq!(control.size(), 0);
    assert_eq!(control.get_value64(), 0, "null, not a dangling pointer");
}

#[test]
fn plane_counts_the_kernel_returns_are_bounded() {
    assert_eq!(returned_planes(0, 3).unwrap(), 0);
    assert_eq!(returned_planes(3, 3).unwrap(), 3);
    for crafted in [4, VIDEO_MAX_PLANES as u32 + 1, u32::MAX] {
        let err = returned_planes(crafted, 3).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EIO), "{crafted}");
    }
}

#[test]
fn a_refused_controls_call_keeps_the_error() {
    let file = crate::v4l2::test_file();
    let mut controls = [ExtControl::value(7, 1)];
    let e = VIDIOC_S_EXT_CTRLS
        .call(file.as_fd(), 0, None, &mut controls)
        .unwrap_err();
    assert_eq!(e.source.raw_os_error(), Some(libc::ENOTTY));
}

#[test]
fn a_controls_call_is_refused_when_which_and_the_request_disagree() {
    let file = crate::v4l2::test_file();
    let mut controls = [ExtControl::value(7, 1)];
    let needs_request = crate::v4l2::CTRL_WHICH_REQUEST_VAL;
    let e = VIDIOC_S_EXT_CTRLS
        .call(file.as_fd(), needs_request, None, &mut controls)
        .unwrap_err();
    assert_eq!(e.source.raw_os_error(), Some(libc::EINVAL));
    let e = VIDIOC_S_EXT_CTRLS
        .call(file.as_fd(), 0, Some(file.as_fd()), &mut controls)
        .unwrap_err();
    assert_eq!(e.source.raw_os_error(), Some(libc::EINVAL));
}

#[test]
fn a_controls_call_past_the_kernel_limit_is_refused_before_the_call() {
    let file = crate::v4l2::test_file();
    let mut controls: Vec<ExtControl<'_>> = (0..=MAX_EXT_CONTROLS as u32)
        .map(|id| ExtControl::value(id, 0))
        .collect();
    let e = VIDIOC_S_EXT_CTRLS
        .call(file.as_fd(), 0, None, &mut controls)
        .unwrap_err();
    assert_eq!(e.source.raw_os_error(), Some(libc::E2BIG));
    controls.truncate(MAX_EXT_CONTROLS);
    let e = VIDIOC_S_EXT_CTRLS
        .call(file.as_fd(), 0, None, &mut controls)
        .unwrap_err();
    assert_eq!(
        e.source.raw_os_error(),
        Some(libc::ENOTTY),
        "reached the kernel"
    );
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

#[test]
fn metadata_capture_matches_the_kernel_header() {
    // `V4L2_BUF_TYPE_META_CAPTURE` and the `v4l2_meta_format` fields, from `linux/videodev2.h`.
    assert_eq!(BufType::MetaCapture as u32, 13);
    assert_eq!(BufType::from_raw(13), Some(BufType::MetaCapture));
    assert!(BufType::MetaCapture.is_meta());
    assert!(!BufType::MetaCapture.is_multiplanar());
    assert_eq!(size_of::<v4l2_meta_format>(), 20);
    assert_eq!(offset_of!(v4l2_meta_format, dataformat), 0);
    assert_eq!(offset_of!(v4l2_meta_format, buffersize), 4);
    assert_eq!(offset_of!(v4l2_meta_format, width), 8);
    assert_eq!(offset_of!(v4l2_meta_format, height), 12);
    assert_eq!(offset_of!(v4l2_meta_format, bytesperline), 16);
    // The format's union carries the meta member at the same place as the others.
    assert_eq!(offset_of!(v4l2_format, fmt), 8);
}
