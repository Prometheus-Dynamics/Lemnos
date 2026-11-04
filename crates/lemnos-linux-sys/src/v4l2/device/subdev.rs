//! The V4L2 subdevice uAPI: `linux/v4l2-subdev.h` and `linux/v4l2-mediabus.h`.
//! Field names are the kernel's, as in [`super`].

#![allow(non_camel_case_types, missing_docs)]

use std::io;
use std::mem::offset_of;
use std::os::fd::BorrowedFd;

use super::geometry::{Fraction, Rect};
use crate::ioctl::{ioctl_ptr, ioctls, iowr, pod};

/// `struct v4l2_mbus_framefmt`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_mbus_framefmt {
    pub width: u32,
    pub height: u32,
    pub code: u32,
    pub field: u32,
    pub colorspace: u32,
    pub ycbcr_enc: u16,
    pub quantization: u16,
    pub xfer_func: u16,
    pub flags: u16,
    pub reserved: [u16; 10],
}

/// `struct v4l2_subdev_format`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_format {
    pub which: u32,
    pub pad: u32,
    pub format: v4l2_mbus_framefmt,
    pub stream: u32,
    pub reserved: [u32; 7],
}

/// `struct v4l2_subdev_mbus_code_enum`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_mbus_code_enum {
    pub pad: u32,
    pub index: u32,
    pub code: u32,
    pub which: u32,
    pub flags: u32,
    pub stream: u32,
    pub reserved: [u32; 6],
}

/// `struct v4l2_subdev_frame_size_enum`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_frame_size_enum {
    pub index: u32,
    pub pad: u32,
    pub code: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub which: u32,
    pub stream: u32,
    pub reserved: [u32; 7],
}

/// `struct v4l2_subdev_frame_interval`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_frame_interval {
    pub pad: u32,
    pub interval: Fraction,
    pub stream: u32,
    pub which: u32,
    pub reserved: [u32; 7],
}

/// `struct v4l2_subdev_frame_interval_enum`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_frame_interval_enum {
    pub index: u32,
    pub pad: u32,
    pub code: u32,
    pub width: u32,
    pub height: u32,
    pub interval: Fraction,
    pub which: u32,
    pub stream: u32,
    pub reserved: [u32; 7],
}

/// `struct v4l2_subdev_selection`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_selection {
    pub which: u32,
    pub pad: u32,
    pub target: u32,
    pub flags: u32,
    pub r: Rect,
    pub stream: u32,
    pub reserved: [u32; 7],
}

/// `struct v4l2_subdev_capability`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_capability {
    pub version: u32,
    pub capabilities: u32,
    pub reserved: [u32; 14],
}

/// `struct v4l2_subdev_route`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_route {
    pub sink_pad: u32,
    pub sink_stream: u32,
    pub source_pad: u32,
    pub source_stream: u32,
    pub flags: u32,
    pub reserved: [u32; 5],
}

/// `struct v4l2_subdev_routing`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct v4l2_subdev_routing {
    pub(crate) which: u32,
    pub(crate) len_routes: u32,
    pub(crate) routes: u64,
    pub(crate) num_routes: u32,
    pub(crate) reserved: [u32; 11],
}

/// `struct v4l2_subdev_client_capability`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_subdev_client_capability {
    pub capabilities: u64,
}

pod!(
    v4l2_mbus_framefmt,
    v4l2_subdev_format,
    v4l2_subdev_mbus_code_enum,
    v4l2_subdev_frame_size_enum,
    v4l2_subdev_frame_interval,
    v4l2_subdev_frame_interval_enum,
    v4l2_subdev_selection,
    v4l2_subdev_capability,
    v4l2_subdev_route,
    v4l2_subdev_client_capability,
);

ioctls! {
    VIDIOC_SUBDEV_QUERYCAP: v4l2_subdev_capability = ior(b'V', 0);
    VIDIOC_SUBDEV_ENUM_MBUS_CODE: v4l2_subdev_mbus_code_enum = iowr(b'V', 2);
    VIDIOC_SUBDEV_G_FMT: v4l2_subdev_format = iowr(b'V', 4);
    VIDIOC_SUBDEV_S_FMT: v4l2_subdev_format = iowr(b'V', 5);
    VIDIOC_SUBDEV_G_FRAME_INTERVAL: v4l2_subdev_frame_interval = iowr(b'V', 21);
    VIDIOC_SUBDEV_S_FRAME_INTERVAL: v4l2_subdev_frame_interval = iowr(b'V', 22);
    VIDIOC_SUBDEV_G_SELECTION: v4l2_subdev_selection = iowr(b'V', 61);
    VIDIOC_SUBDEV_S_SELECTION: v4l2_subdev_selection = iowr(b'V', 62);
    VIDIOC_SUBDEV_ENUM_FRAME_SIZE: v4l2_subdev_frame_size_enum = iowr(b'V', 74);
    VIDIOC_SUBDEV_ENUM_FRAME_INTERVAL: v4l2_subdev_frame_interval_enum = iowr(b'V', 75);
    VIDIOC_SUBDEV_G_CLIENT_CAP: v4l2_subdev_client_capability = ior(b'V', 101);
    VIDIOC_SUBDEV_S_CLIENT_CAP: v4l2_subdev_client_capability = iowr(b'V', 102);
}

const G_ROUTING: u32 = iowr::<v4l2_subdev_routing>(b'V', 38);
const S_ROUTING: u32 = iowr::<v4l2_subdev_routing>(b'V', 39);

/// One routing call over `routes`: their count as the kernel left it, and the
/// call's result.
fn routing(
    fd: BorrowedFd<'_>,
    request: u32,
    which: u32,
    num_routes: u32,
    routes: &mut [v4l2_subdev_route],
) -> (u32, io::Result<()>) {
    let mut raw = v4l2_subdev_routing {
        which,
        len_routes: routes.len() as u32,
        routes: routes.as_mut_ptr() as u64,
        num_routes,
        reserved: [0; 11],
    };
    // SAFETY: the request takes a `v4l2_subdev_routing`, which `raw` is; `routes`
    // points at `len_routes` entries, alive for the call.
    let result = unsafe { ioctl_ptr(fd, request, &raw mut raw) };
    (raw.num_routes, result.map(|_| ()))
}

/// Reads the routing table into `routes` (`VIDIOC_SUBDEV_G_ROUTING`). Returns the
/// number of routes the subdevice has, which on `ENOSPC` is more than `routes` holds.
pub fn g_routing(
    fd: BorrowedFd<'_>,
    which: u32,
    routes: &mut [v4l2_subdev_route],
) -> (u32, io::Result<()>) {
    routing(fd, G_ROUTING, which, 0, routes)
}

/// Replaces the routing table with `routes` (`VIDIOC_SUBDEV_S_ROUTING`). The kernel
/// writes back the table it kept.
pub fn s_routing(
    fd: BorrowedFd<'_>,
    which: u32,
    routes: &mut [v4l2_subdev_route],
) -> io::Result<()> {
    routing(fd, S_ROUTING, which, routes.len() as u32, routes).1
}

const _: () = {
    assert!(size_of::<v4l2_mbus_framefmt>() == 48);
    assert!(size_of::<v4l2_subdev_format>() == 88);
    assert!(size_of::<v4l2_subdev_mbus_code_enum>() == 48);
    assert!(size_of::<v4l2_subdev_frame_size_enum>() == 64);
    assert!(size_of::<v4l2_subdev_frame_interval>() == 48);
    assert!(size_of::<v4l2_subdev_frame_interval_enum>() == 64);
    assert!(size_of::<v4l2_subdev_selection>() == 64);
    assert!(size_of::<v4l2_subdev_capability>() == 64);
    assert!(size_of::<v4l2_subdev_route>() == 40);
    assert!(size_of::<v4l2_subdev_routing>() == 64);
    assert!(offset_of!(v4l2_subdev_routing, routes) == 8);
    assert!(offset_of!(v4l2_subdev_routing, num_routes) == 16);
    assert!(size_of::<v4l2_subdev_client_capability>() == 8);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn ioctl_numbers_match_the_headers() {
        assert_eq!(VIDIOC_SUBDEV_QUERYCAP.nr(), 0x8040_5600);
        assert_eq!(VIDIOC_SUBDEV_G_FMT.nr(), 0xc058_5604);
        assert_eq!(VIDIOC_SUBDEV_ENUM_MBUS_CODE.nr(), 0xc030_5602);
        assert_eq!(VIDIOC_SUBDEV_ENUM_FRAME_SIZE.nr(), 0xc040_564a);
        assert_eq!(VIDIOC_SUBDEV_ENUM_FRAME_INTERVAL.nr(), 0xc040_564b);
        assert_eq!(VIDIOC_SUBDEV_G_FRAME_INTERVAL.nr(), 0xc030_5615);
        assert_eq!(VIDIOC_SUBDEV_G_SELECTION.nr(), 0xc040_563d);
        assert_eq!(G_ROUTING, 0xc040_5626);
        assert_eq!(S_ROUTING, 0xc040_5627);
        assert_eq!(VIDIOC_SUBDEV_S_CLIENT_CAP.nr(), 0xc008_5666);
    }

    #[test]
    fn routing_on_a_plain_descriptor_fails_cleanly() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsFd::as_fd(&file);
        let mut routes = [v4l2_subdev_route::default(); 2];
        let (_, result) = g_routing(fd, 1, &mut routes);
        assert!(result.is_err());
        assert!(s_routing(fd, 1, &mut routes).is_err());
    }
}
