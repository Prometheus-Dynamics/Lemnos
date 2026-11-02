//! `linux/videodev2.h` structures and request numbers, as `#[repr(C)]` types.
//!
//! The kernel's unions are byte arrays here (`Format::fmt`, `Event::u`), read
//! and written by offset (`get_u32`/`put_u32`), so the field code needs no
//! `unsafe`. The one union with a pointer or descriptor member, the buffer's
//! `m`, is a real union: its members are written whole, and read only where
//! the kernel has just filled them in.

use crate::ioctl::{ior, iow, iowr};
/// The V4L2 request type byte, `'V'`.
const V: u8 = b'V';

/// Marks a type whose all-zero bytes are a valid value.
///
/// # Safety
///
/// Implement only for `#[repr(C)]` types whose fields are integers, byte or
/// integer arrays, null-able pointers, `timeval`, or unions of those.
pub(crate) unsafe trait Pod: Copy {
    /// The all-zero value.
    fn zeroed() -> Self {
        // SAFETY: `Pod` promises every all-zero bit pattern is a valid value.
        unsafe { std::mem::zeroed() }
    }
}

// SAFETY: plain integers and arrays of them.
unsafe impl Pod for u32 {}
// SAFETY: a `timeval` is two integers.
unsafe impl Pod for libc::timeval {}

/// The kernel's `struct v4l2_capability` (104 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    reserved: [u32; 3],
}
// SAFETY: integers and byte arrays only.
unsafe impl Pod for Capability {}

/// The kernel's `struct v4l2_format` (208 bytes): a type, then the union of
/// the pixel formats, at offset 8 (the union holds pointers, so it is 8-aligned).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Format {
    pub buf_type: u32,
    pub fmt: FormatUnion,
}
// SAFETY: integers and byte arrays only.
unsafe impl Pod for Format {}

/// The 200-byte format union, 8-aligned as in the kernel.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(crate) struct FormatUnion(pub [u8; 200]);

/// The kernel's `struct v4l2_fmtdesc` (64 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FmtDesc {
    pub index: u32,
    pub buf_type: u32,
    pub flags: u32,
    pub description: [u8; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    reserved: [u32; 3],
}
// SAFETY: integers and byte arrays only.
unsafe impl Pod for FmtDesc {}

/// The kernel's `struct v4l2_requestbuffers` (20 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RequestBuffers {
    pub count: u32,
    pub buf_type: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    reserved: [u8; 3],
}
// SAFETY: integers and byte arrays only.
unsafe impl Pod for RequestBuffers {}

/// The kernel's `struct v4l2_plane` (64 bytes on 64-bit targets).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Plane {
    pub bytesused: u32,
    pub length: u32,
    pub m: Memory,
    pub data_offset: u32,
    reserved: [u32; 11],
}
// SAFETY: `Memory` is zero-valid (offset 0) and the rest are integers.
unsafe impl Pod for Plane {}

/// The `union` of the buffer and plane `m` fields: the offset of an MMAP
/// buffer, a DMA-buf descriptor, a user pointer, or a plane array.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union Memory {
    pub offset: u32,
    pub userptr: usize,
    pub planes: *mut Plane,
    pub fd: i32,
}
// SAFETY: zero is offset 0, a valid union member; the pointer is null.
unsafe impl Pod for Memory {}

/// The kernel's `struct v4l2_buffer` (88 bytes on 64-bit targets).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Buffer {
    pub index: u32,
    pub buf_type: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub timestamp: libc::timeval,
    pub timecode: [u8; 16],
    pub sequence: u32,
    pub memory: u32,
    pub m: Memory,
    pub length: u32,
    reserved2: u32,
    pub request_fd: i32,
    reserved: u32,
}
// SAFETY: as for `Plane`.
unsafe impl Pod for Buffer {}

/// The kernel's `struct v4l2_exportbuffer` (64 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ExportBuffer {
    pub buf_type: u32,
    pub index: u32,
    pub plane: u32,
    pub flags: u32,
    pub fd: i32,
    reserved: [u32; 11],
}
// SAFETY: integers only.
unsafe impl Pod for ExportBuffer {}

/// The kernel's `struct v4l2_encoder_cmd` (40 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct EncoderCmd {
    pub cmd: u32,
    pub flags: u32,
    raw: [u32; 8],
}
// SAFETY: integers only.
unsafe impl Pod for EncoderCmd {}

/// The kernel's `struct v4l2_decoder_cmd` (72 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DecoderCmd {
    pub cmd: u32,
    pub flags: u32,
    raw: [u64; 8],
}
// SAFETY: integers only.
unsafe impl Pod for DecoderCmd {}

/// The kernel's `struct v4l2_event_subscription` (32 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Subscription {
    pub event_type: u32,
    pub id: u32,
    pub flags: u32,
    reserved: [u32; 5],
}
// SAFETY: integers only.
unsafe impl Pod for Subscription {}

/// The 64-byte payload union of `struct v4l2_event`, 8-aligned.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(crate) struct EventUnion(pub [u8; 64]);

/// The kernel's `struct v4l2_event` (136 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub event_type: u32,
    pub u: EventUnion,
    pub pending: u32,
    pub sequence: u32,
    pub timestamp: [i64; 2],
    pub id: u32,
    reserved: [u32; 8],
}
// SAFETY: integers and byte arrays only.
unsafe impl Pod for Event {}

/// The kernel's `struct v4l2_selection` (64 bytes): a queue's crop, compose or
/// bounds rectangle (`r`), read or set through `G_SELECTION` / `S_SELECTION`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Selection {
    pub buf_type: u32,
    pub target: u32,
    pub flags: u32,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    reserved: [u32; 9],
}
// SAFETY: integers only.
unsafe impl Pod for Selection {}

/// `VIDIOC_QUERYCAP`.
pub(crate) const QUERYCAP: u32 = ior::<Capability>(V, 0);
/// `VIDIOC_ENUM_FMT`.
pub(crate) const ENUM_FMT: u32 = iowr::<FmtDesc>(V, 2);
/// `VIDIOC_G_FMT`.
pub(crate) const G_FMT: u32 = iowr::<Format>(V, 4);
/// `VIDIOC_S_FMT`.
pub(crate) const S_FMT: u32 = iowr::<Format>(V, 5);
/// `VIDIOC_REQBUFS`.
pub(crate) const REQBUFS: u32 = iowr::<RequestBuffers>(V, 8);
/// `VIDIOC_QUERYBUF`.
pub(crate) const QUERYBUF: u32 = iowr::<Buffer>(V, 9);
/// `VIDIOC_QBUF`.
pub(crate) const QBUF: u32 = iowr::<Buffer>(V, 15);
/// `VIDIOC_DQBUF`.
pub(crate) const DQBUF: u32 = iowr::<Buffer>(V, 17);
/// `VIDIOC_STREAMON` (takes a pointer to an `int`).
pub(crate) const STREAMON: u32 = iow::<i32>(V, 18);
/// `VIDIOC_STREAMOFF` (takes a pointer to an `int`).
pub(crate) const STREAMOFF: u32 = iow::<i32>(V, 19);
/// `VIDIOC_EXPBUF`.
pub(crate) const EXPBUF: u32 = iowr::<ExportBuffer>(V, 16);
/// `VIDIOC_ENCODER_CMD`.
pub(crate) const ENCODER_CMD: u32 = iowr::<EncoderCmd>(V, 77);
/// `VIDIOC_DECODER_CMD`.
pub(crate) const DECODER_CMD: u32 = iowr::<DecoderCmd>(V, 96);
/// `VIDIOC_SUBSCRIBE_EVENT`.
pub(crate) const SUBSCRIBE_EVENT: u32 = iow::<Subscription>(V, 90);
/// `VIDIOC_DQEVENT`.
pub(crate) const DQEVENT: u32 = ior::<Event>(V, 89);
/// `VIDIOC_G_SELECTION`.
pub(crate) const G_SELECTION: u32 = iowr::<Selection>(V, 94);
/// `VIDIOC_S_SELECTION`.
pub(crate) const S_SELECTION: u32 = iowr::<Selection>(V, 95);

/// Reads a native-endian `u32` at `offset` of a byte array.
pub(crate) fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// Writes a native-endian `u32` at `offset` of a byte array.
pub(crate) fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn structure_sizes_match_the_kernel_header() {
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<Format>(), 208);
        assert_eq!(size_of::<FmtDesc>(), 64);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        assert_eq!(size_of::<Plane>(), 64);
        assert_eq!(size_of::<Buffer>(), 88);
        assert_eq!(size_of::<ExportBuffer>(), 64);
        assert_eq!(size_of::<EncoderCmd>(), 40);
        assert_eq!(size_of::<DecoderCmd>(), 72);
        assert_eq!(size_of::<Subscription>(), 32);
        assert_eq!(size_of::<Event>(), 136);
        assert_eq!(size_of::<Selection>(), 64);
    }

    #[test]
    fn field_offsets_match_the_kernel_header() {
        assert_eq!(offset_of!(Capability, card), 16);
        assert_eq!(offset_of!(Capability, bus_info), 48);
        assert_eq!(offset_of!(Capability, device_caps), 88);
        assert_eq!(offset_of!(Format, fmt), 8);
        assert_eq!(offset_of!(Buffer, timestamp), 24);
        assert_eq!(offset_of!(Buffer, timecode), 40);
        assert_eq!(offset_of!(Buffer, sequence), 56);
        assert_eq!(offset_of!(Buffer, memory), 60);
        assert_eq!(offset_of!(Buffer, m), 64);
        assert_eq!(offset_of!(Buffer, length), 72);
        assert_eq!(offset_of!(Buffer, request_fd), 80);
        assert_eq!(offset_of!(Plane, m), 8);
        assert_eq!(offset_of!(Plane, data_offset), 16);
        assert_eq!(offset_of!(ExportBuffer, fd), 16);
        assert_eq!(offset_of!(DecoderCmd, raw), 8);
        assert_eq!(offset_of!(Event, u), 8);
        assert_eq!(offset_of!(Event, pending), 72);
        assert_eq!(offset_of!(Event, sequence), 76);
        assert_eq!(offset_of!(Event, timestamp), 80);
        assert_eq!(offset_of!(Event, id), 96);
    }

    #[test]
    fn request_numbers_match_the_kernel_header() {
        assert_eq!(QUERYCAP, 0x8068_5600);
        assert_eq!(ENUM_FMT, 0xc040_5602);
        assert_eq!(G_FMT, 0xc0d0_5604);
        assert_eq!(S_FMT, 0xc0d0_5605);
        assert_eq!(REQBUFS, 0xc014_5608);
        assert_eq!(QUERYBUF, 0xc058_5609);
        assert_eq!(QBUF, 0xc058_560f);
        assert_eq!(DQBUF, 0xc058_5611);
        assert_eq!(EXPBUF, 0xc040_5610);
        assert_eq!(STREAMON, 0x4004_5612);
        assert_eq!(STREAMOFF, 0x4004_5613);
        assert_eq!(ENCODER_CMD, 0xc028_564d);
        assert_eq!(DECODER_CMD, 0xc048_5660);
        assert_eq!(SUBSCRIBE_EVENT, 0x4020_565a);
        assert_eq!(DQEVENT, 0x8088_5659);
        assert_eq!(G_SELECTION, 0xc040_565e);
        assert_eq!(S_SELECTION, 0xc040_565f);
    }

    #[test]
    fn byte_fields_round_trip() {
        let mut bytes = [0u8; 8];
        put_u32(&mut bytes, 4, 0x0102_0304);
        assert_eq!(get_u32(&bytes, 4), 0x0102_0304);
        assert_eq!(get_u32(&bytes, 0), 0);
    }
}
