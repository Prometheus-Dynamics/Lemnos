//! `linux/videodev2.h` structures and request numbers, as `#[repr(C)]` types.
//!
//! Field names are the kernel's and mean what the header says, so the fields
//! carry no documentation of their own. Only the fields and union members in use
//! are named; unions keep their full size and alignment, so the layouts match
//! the kernel's exactly. The sizes are checked at compile time, and against the
//! header in the tests.
//!
//! Members the kernel follows as pointers stay private to this crate: the safe
//! calls in [`super::calls`] set them. Reading a union takes `unsafe`, so the
//! unions with plain members have accessors here.

#![allow(non_camel_case_types, missing_docs)]

use std::mem::offset_of;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::{fmt, io};

use super::geometry::{Fraction, Rect};
use crate::ioctl::{Ioctl, ioctls, iowr, pod};

/// `VIDEO_MAX_PLANES`: the most planes one buffer has.
pub const VIDEO_MAX_PLANES: usize = 8;

/// `struct v4l2_capability`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

/// `struct v4l2_fmtdesc`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_fmtdesc {
    pub index: u32,
    pub type_: u32,
    pub flags: u32,
    pub description: [u8; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    pub reserved: [u32; 3],
}

/// `struct v4l2_frmsizeenum`; the discrete/stepwise union is `u` (discrete uses `u[0..2]`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_frmsizeenum {
    pub index: u32,
    pub pixel_format: u32,
    pub type_: u32,
    pub u: [u32; 6],
    pub reserved: [u32; 2],
}

/// `struct v4l2_frmivalenum`; the union holds one fraction (discrete) or three (stepwise).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_frmivalenum {
    pub index: u32,
    pub pixel_format: u32,
    pub width: u32,
    pub height: u32,
    pub type_: u32,
    pub u: [Fraction; 3],
    pub reserved: [u32; 2],
}

/// `struct v4l2_pix_format`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_pix_format {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

/// `struct v4l2_plane_pix_format`.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_plane_pix_format {
    pub sizeimage: u32,
    pub bytesperline: u32,
    pub reserved: [u16; 6],
}

/// `struct v4l2_pix_format_mplane`.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_pix_format_mplane {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub colorspace: u32,
    pub plane_fmt: [v4l2_plane_pix_format; VIDEO_MAX_PLANES],
    pub num_planes: u8,
    pub flags: u8,
    pub ycbcr_enc: u8,
    pub quantization: u8,
    pub xfer_func: u8,
    pub reserved: [u8; 7],
}

/// `struct v4l2_meta_format`.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_meta_format {
    pub dataformat: u32,
    pub buffersize: u32,
    pub width: u32,
    pub height: u32,
    pub bytesperline: u32,
}

/// The `fmt` union of `struct v4l2_format`. `v4l2_window` (not used here) contains pointers,
/// which gives the union pointer alignment; `_align` reproduces that.
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_format_union {
    pub pix: v4l2_pix_format,
    pub pix_mp: v4l2_pix_format_mplane,
    pub meta: v4l2_meta_format,
    pub raw_data: [u8; 200],
    pub _align: [*mut libc::c_void; 0],
}

/// `struct v4l2_format`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_format {
    pub type_: u32,
    pub fmt: v4l2_format_union,
}

/// `struct v4l2_captureparm`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_captureparm {
    pub capability: u32,
    pub capturemode: u32,
    pub timeperframe: Fraction,
    pub extendedmode: u32,
    pub readbuffers: u32,
    pub reserved: [u32; 4],
}

/// `struct v4l2_streamparm`; `v4l2_outputparm` has the same layout as `v4l2_captureparm`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_streamparm_union {
    pub capture: v4l2_captureparm,
    pub raw_data: [u8; 200],
}

/// `struct v4l2_streamparm`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_streamparm {
    pub type_: u32,
    pub parm: v4l2_streamparm_union,
}

/// `struct v4l2_requestbuffers`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_requestbuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

/// The `m` union of `v4l2_plane` / `v4l2_buffer`. Private: the kernel follows
/// `planes` and `userptr`, so only this crate writes them (see `BufferIoctl`).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union v4l2_m {
    pub(crate) offset: u32,
    pub(crate) userptr: libc::c_ulong,
    pub(crate) planes: *mut v4l2_plane,
    pub(crate) fd: i32,
}

/// `struct v4l2_plane`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_plane {
    pub bytesused: u32,
    pub length: u32,
    pub(crate) m: v4l2_m,
    pub data_offset: u32,
    pub reserved: [u32; 11],
}

/// `struct v4l2_timecode`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_timecode {
    pub type_: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}

/// `struct v4l2_buffer`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_buffer {
    pub index: u32,
    pub type_: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub timestamp: libc::timeval,
    pub timecode: v4l2_timecode,
    pub sequence: u32,
    pub memory: u32,
    pub(crate) m: v4l2_m,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
}

/// `struct v4l2_exportbuffer`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_exportbuffer {
    pub type_: u32,
    pub index: u32,
    pub plane: u32,
    pub flags: u32,
    pub fd: i32,
    pub reserved: [u32; 11],
}

/// `struct v4l2_queryctrl`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_queryctrl {
    pub id: u32,
    pub type_: u32,
    pub name: [u8; 32],
    pub minimum: i32,
    pub maximum: i32,
    pub step: i32,
    pub default_value: i32,
    pub flags: u32,
    pub reserved: [u32; 2],
}

/// `V4L2_CTRL_MAX_DIMS`.
pub const V4L2_CTRL_MAX_DIMS: usize = 4;

/// `struct v4l2_query_ext_ctrl`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_query_ext_ctrl {
    pub id: u32,
    pub type_: u32,
    pub name: [u8; 32],
    pub minimum: i64,
    pub maximum: i64,
    pub step: u64,
    pub default_value: i64,
    pub flags: u32,
    pub elem_size: u32,
    pub elems: u32,
    pub nr_of_dims: u32,
    pub dims: [u32; V4L2_CTRL_MAX_DIMS],
    pub reserved: [u32; 32],
}

/// `struct v4l2_querymenu`: `name` overlays the `__s64 value` of integer menus.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct v4l2_querymenu {
    pub id: u32,
    pub index: u32,
    pub name: [u8; 32],
    pub reserved: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub(crate) union v4l2_ext_control_value {
    pub(crate) value: i32,
    pub(crate) value64: i64,
    pub(crate) ptr: *mut libc::c_void,
}

/// `struct v4l2_ext_control`. Crate-private: `ExtControl` is its safe form.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub(crate) struct v4l2_ext_control {
    pub(crate) id: u32,
    pub(crate) size: u32,
    pub(crate) reserved2: [u32; 1],
    pub(crate) u: v4l2_ext_control_value,
}

/// `struct v4l2_ext_controls`. Crate-private: the kernel follows `controls`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct v4l2_ext_controls {
    pub(crate) which: u32,
    pub(crate) count: u32,
    pub(crate) error_idx: u32,
    pub(crate) request_fd: i32,
    pub(crate) reserved: [u32; 1],
    pub(crate) controls: *mut v4l2_ext_control,
}

/// `struct v4l2_selection`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_selection {
    pub type_: u32,
    pub target: u32,
    pub flags: u32,
    pub r: Rect,
    pub reserved: [u32; 9],
}

/// The 64-byte event payload union of `struct v4l2_event` (8-byte aligned: `v4l2_event_ctrl`
/// has an `__s64`).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct v4l2_event_data(pub [u8; 64]);

/// `struct v4l2_event`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_event {
    pub type_: u32,
    pub u: v4l2_event_data,
    pub pending: u32,
    pub sequence: u32,
    pub timestamp: libc::timespec,
    pub id: u32,
    pub reserved: [u32; 8],
}

/// `struct v4l2_event_subscription`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_event_subscription {
    pub type_: u32,
    pub id: u32,
    pub flags: u32,
    pub reserved: [u32; 5],
}

/// `struct v4l2_encoder_cmd`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_encoder_cmd {
    pub cmd: u32,
    pub flags: u32,
    pub data: [u32; 8],
}

/// `struct v4l2_decoder_cmd`: a union of 64 bytes after the command (`data`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_decoder_cmd {
    pub cmd: u32,
    pub flags: u32,
    pub data: [u64; 8],
}

/// `struct v4l2_control`: a control id and its value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_control {
    pub id: u32,
    pub value: i32,
}

pod!(
    v4l2_capability,
    v4l2_fmtdesc,
    v4l2_frmsizeenum,
    v4l2_frmivalenum,
    v4l2_pix_format,
    v4l2_plane_pix_format,
    v4l2_pix_format_mplane,
    v4l2_meta_format,
    v4l2_format_union,
    v4l2_format,
    v4l2_captureparm,
    v4l2_streamparm_union,
    v4l2_streamparm,
    v4l2_requestbuffers,
    v4l2_m,
    v4l2_plane,
    v4l2_timecode,
    v4l2_buffer,
    v4l2_exportbuffer,
    v4l2_queryctrl,
    v4l2_query_ext_ctrl,
    v4l2_querymenu,
    v4l2_ext_control_value,
    v4l2_ext_control,
    v4l2_selection,
    v4l2_event_data,
    v4l2_event,
    v4l2_event_subscription,
    v4l2_encoder_cmd,
    v4l2_decoder_cmd,
    v4l2_control,
);

/// `V4L2_MEMORY_USERPTR`: the kernel would follow the buffer's pointer, so the
/// calls here refuse it.
pub(crate) const MEMORY_USERPTR: u32 = 2;
/// `V4L2_BUF_TYPE_VIDEO_OVERLAY`.
const BUF_TYPE_VIDEO_OVERLAY: u32 = 3;
/// `V4L2_BUF_TYPE_VIDEO_OUTPUT_OVERLAY`.
const BUF_TYPE_VIDEO_OUTPUT_OVERLAY: u32 = 8;
/// `V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE`.
const BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
/// `V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE`.
const BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;

impl v4l2_format {
    /// The single-planar member.
    pub fn pix(&self) -> v4l2_pix_format {
        // SAFETY: the member is plain integers, so any bytes are a value.
        unsafe { self.fmt.pix }
    }

    /// The multi-planar member.
    pub fn pix_mp(&self) -> v4l2_pix_format_mplane {
        // SAFETY: as for `pix`.
        unsafe { self.fmt.pix_mp }
    }

    /// The metadata member.
    pub fn meta(&self) -> v4l2_meta_format {
        // SAFETY: as for `pix`.
        unsafe { self.fmt.meta }
    }
}

/// Refuses the overlay queues: their format is a `struct v4l2_window`, which holds
/// pointers the kernel may follow.
fn no_overlay(format: &v4l2_format) -> io::Result<()> {
    match format.type_ {
        BUF_TYPE_VIDEO_OVERLAY | BUF_TYPE_VIDEO_OUTPUT_OVERLAY => {
            Err(io::Error::from_raw_os_error(libc::EINVAL))
        }
        _ => Ok(()),
    }
}

impl v4l2_streamparm {
    /// The capture member (output parameters have the same layout).
    pub fn capture(&self) -> v4l2_captureparm {
        // SAFETY: the member is plain integers, so any bytes are a value.
        unsafe { self.parm.capture }
    }
}

impl v4l2_plane {
    /// The `mmap` offset of an MMAP plane.
    pub fn offset(&self) -> u32 {
        // SAFETY: the union's first four bytes are a plain integer.
        unsafe { self.m.offset }
    }

    /// Makes this a DMABUF plane on `fd`.
    pub fn set_dmabuf(&mut self, fd: BorrowedFd<'_>) {
        self.m = v4l2_m { fd: fd.as_raw_fd() };
    }
}

impl v4l2_buffer {
    /// The `mmap` offset of a single-planar MMAP buffer.
    pub fn offset(&self) -> u32 {
        // SAFETY: the union's first four bytes are a plain integer.
        unsafe { self.m.offset }
    }

    /// Makes this a single-planar DMABUF buffer on `fd`.
    pub fn set_dmabuf(&mut self, fd: BorrowedFd<'_>) {
        self.m = v4l2_m { fd: fd.as_raw_fd() };
    }

    /// Whether the buffer's type is a multi-planar one: the kernel then reads
    /// `m` as a plane array.
    pub(crate) fn is_multiplanar(&self) -> bool {
        matches!(
            self.type_,
            BUF_TYPE_VIDEO_CAPTURE_MPLANE | BUF_TYPE_VIDEO_OUTPUT_MPLANE
        )
    }
}

impl fmt::Debug for v4l2_buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v4l2_buffer({}, type {})", self.index, self.type_)
    }
}

ioctls! {
    VIDIOC_QUERYCAP: v4l2_capability = ior(b'V', 0);
    VIDIOC_ENUM_FMT: v4l2_fmtdesc = iowr(b'V', 2);
    VIDIOC_REQBUFS: v4l2_requestbuffers = iowr(b'V', 8);
    VIDIOC_STREAMON: libc::c_int = iow(b'V', 18);
    VIDIOC_STREAMOFF: libc::c_int = iow(b'V', 19);
    VIDIOC_G_PARM: v4l2_streamparm = iowr(b'V', 21);
    VIDIOC_S_PARM: v4l2_streamparm = iowr(b'V', 22);
    VIDIOC_G_CTRL: v4l2_control = iowr(b'V', 27);
    VIDIOC_S_CTRL: v4l2_control = iowr(b'V', 28);
    VIDIOC_QUERYCTRL: v4l2_queryctrl = iowr(b'V', 36);
    VIDIOC_QUERYMENU: v4l2_querymenu = iowr(b'V', 37);
    VIDIOC_ENUM_FRAMESIZES: v4l2_frmsizeenum = iowr(b'V', 74);
    VIDIOC_ENUM_FRAMEINTERVALS: v4l2_frmivalenum = iowr(b'V', 75);
    VIDIOC_ENCODER_CMD: v4l2_encoder_cmd = iowr(b'V', 77);
    VIDIOC_DQEVENT: v4l2_event = ior(b'V', 89);
    VIDIOC_SUBSCRIBE_EVENT: v4l2_event_subscription = iow(b'V', 90);
    VIDIOC_UNSUBSCRIBE_EVENT: v4l2_event_subscription = iow(b'V', 91);
    VIDIOC_G_SELECTION: v4l2_selection = iowr(b'V', 94);
    VIDIOC_S_SELECTION: v4l2_selection = iowr(b'V', 95);
    VIDIOC_DECODER_CMD: v4l2_decoder_cmd = iowr(b'V', 96);
    VIDIOC_QUERY_EXT_CTRL: v4l2_query_ext_ctrl = iowr(b'V', 103);
}

/// `VIDIOC_G_FMT`. Refuses the overlay queues.
pub const VIDIOC_G_FMT: Ioctl<v4l2_format> =
    Ioctl::checked(iowr::<v4l2_format>(b'V', 4), "VIDIOC_G_FMT", no_overlay);
/// `VIDIOC_S_FMT`. Refuses the overlay queues.
pub const VIDIOC_S_FMT: Ioctl<v4l2_format> =
    Ioctl::checked(iowr::<v4l2_format>(b'V', 5), "VIDIOC_S_FMT", no_overlay);
/// `VIDIOC_TRY_FMT`. Refuses the overlay queues.
pub const VIDIOC_TRY_FMT: Ioctl<v4l2_format> =
    Ioctl::checked(iowr::<v4l2_format>(b'V', 64), "VIDIOC_TRY_FMT", no_overlay);

// Layouts common to all targets.
const _: () = {
    assert!(size_of::<v4l2_capability>() == 104);
    assert!(size_of::<v4l2_fmtdesc>() == 64);
    assert!(size_of::<v4l2_encoder_cmd>() == 40);
    assert!(size_of::<v4l2_control>() == 8);
    assert!(size_of::<v4l2_frmsizeenum>() == 44);
    assert!(size_of::<v4l2_frmivalenum>() == 52);
    assert!(size_of::<v4l2_pix_format>() == 48);
    assert!(size_of::<v4l2_plane_pix_format>() == 20);
    assert!(size_of::<v4l2_pix_format_mplane>() == 192);
    assert!(offset_of!(v4l2_pix_format_mplane, num_planes) == 180);
    assert!(size_of::<v4l2_meta_format>() == 20);
    assert!(size_of::<v4l2_captureparm>() == 40);
    assert!(size_of::<v4l2_streamparm>() == 204);
    assert!(size_of::<v4l2_requestbuffers>() == 20);
    assert!(size_of::<v4l2_timecode>() == 16);
    assert!(size_of::<v4l2_exportbuffer>() == 64);
    assert!(size_of::<v4l2_queryctrl>() == 68);
    assert!(size_of::<v4l2_query_ext_ctrl>() == 232);
    assert!(offset_of!(v4l2_query_ext_ctrl, minimum) == 40);
    assert!(offset_of!(v4l2_query_ext_ctrl, dims) == 88);
    assert!(size_of::<v4l2_querymenu>() == 44);
    assert!(size_of::<v4l2_ext_control>() == 20);
    assert!(size_of::<v4l2_selection>() == 64);
    assert!(size_of::<v4l2_event_subscription>() == 32);
};

// Layouts with pointers, `long` or `timeval`: checked on 64-bit (x86_64, aarch64), where the
// values were confirmed against the C headers.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<v4l2_decoder_cmd>() == 72);
    assert!(size_of::<v4l2_format>() == 208);
    assert!(offset_of!(v4l2_format, fmt) == 8);
    assert!(size_of::<v4l2_plane>() == 64);
    assert!(offset_of!(v4l2_plane, m) == 8);
    assert!(offset_of!(v4l2_plane, data_offset) == 16);
    assert!(size_of::<v4l2_buffer>() == 88);
    assert!(offset_of!(v4l2_buffer, timestamp) == 24);
    assert!(offset_of!(v4l2_buffer, sequence) == 56);
    assert!(offset_of!(v4l2_buffer, m) == 64);
    assert!(offset_of!(v4l2_buffer, length) == 72);
    assert!(offset_of!(v4l2_buffer, request_fd) == 80);
    assert!(size_of::<v4l2_ext_controls>() == 32);
    assert!(offset_of!(v4l2_ext_controls, controls) == 24);
    assert!(size_of::<v4l2_event>() == 136);
    assert!(offset_of!(v4l2_event, u) == 8);
    assert!(offset_of!(v4l2_event, pending) == 72);
    assert!(offset_of!(v4l2_event, timestamp) == 80);
    assert!(offset_of!(v4l2_event, id) == 96);
};
