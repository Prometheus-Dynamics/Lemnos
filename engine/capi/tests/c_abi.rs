//! The C surface called the way a C caller calls it: the `extern "C"` functions of
//! `lemnos_capi::surface`, with the structs, statuses and sinks `include/lemnos.h` declares. These
//! are the flows the C programs this replaced (`client.c`, `stream.c`) covered: the client's calls
//! and subscriptions, the frame streams and their jobs, a stream closed with a frame held, and the
//! package list. `c/smoke.c` is the one C program kept; it links the library and runs once against
//! the same service.
//!
//! Each function is called through a wrapper that holds its one `unsafe` call and says why it is
//! sound, so the tests below read as the C programs did.

mod common;

use core::ffi::{CStr, c_void};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Producing, frame_is_right, package_list, package_service, service, stream_service};
use lemnos_abi::{Handle, MessageSink};
use lemnos_capi::surface::{
    self as s, DeviceInfo, Frame, ItemInfo, PackageInfo, StreamInfo, StreamRegion, Subscription,
    Value,
};

const OK: i32 = 0;
const E_STALE_HANDLE: i32 = -4096;
const EACCES: i32 = -13;
const EBUSY: i32 = -16;
const ENOENT: i32 = -2;
const EINVAL: i32 = -22;
const EOPNOTSUPP: i32 = -95;
const FRAME_BYTES: usize = 5000;
const INPUT_BYTES: usize = 256;

/// The text a sink was given, the way the C programs' `collect` kept it.
struct Sink {
    text: Vec<u8>,
}

impl Sink {
    fn new() -> Self {
        Self { text: Vec::new() }
    }

    fn raw(&mut self) -> MessageSink {
        MessageSink {
            context: (&mut self.text as *mut Vec<u8>).cast::<c_void>(),
            write: Some(collect),
        }
    }

    fn said(&self) -> String {
        String::from_utf8_lossy(&self.text).into_owned()
    }
}

extern "C" fn collect(context: *mut c_void, text: *const u8, len: usize) {
    if len == 0 {
        return;
    }
    // SAFETY: `context` is the Vec of the sink that `raw` made, and it lives as long as the sink;
    // no other reference to it is in use during a call.
    let kept = unsafe { &mut *context.cast::<Vec<u8>>() };
    // SAFETY: the surface passes `len` bytes at `text`, valid for the duration of this callback.
    let bytes = unsafe { core::slice::from_raw_parts(text, len) };
    kept.extend_from_slice(bytes);
}

// ---- Wrappers: one C call each, with its one unsafe operation ----

fn check_version() -> i32 {
    let mine = s::VERSION;
    // SAFETY: `mine` is a valid AbiVersion for the call.
    unsafe { s::lemnos_check_version(&mine) }.0
}

fn client_open(socket: &CStr, name: &CStr, client: &mut Handle, sink: MessageSink) -> i32 {
    // SAFETY: both strings are NUL-terminated and live for the call; `client` is valid for a write.
    unsafe { s::lemnos_client_open(socket.as_ptr(), name.as_ptr(), 0, client, sink) }.0
}

fn client_close(client: Handle) -> i32 {
    s::lemnos_client_close(client).0
}

fn device_count(client: Handle) -> i32 {
    s::lemnos_device_count(client).0
}

fn device_info(client: Handle, device: u32, info: &mut DeviceInfo) -> i32 {
    // SAFETY: `info` is valid for a write for its whole size.
    unsafe { s::lemnos_device_info(client, device, info) }.0
}

fn device_name(client: Handle, device: u32) -> (i32, String) {
    let mut out = [0u8; 64];
    let mut len = 0usize;
    // SAFETY: `out` is writable for its capacity and `len` is valid for a write.
    let status = unsafe {
        s::lemnos_device_text(
            client,
            device,
            0,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut len,
        )
    }
    .0;
    (status, String::from_utf8_lossy(&out[..len]).into_owned())
}

fn open_contract(
    client: Handle,
    device: &CStr,
    tag: &CStr,
    opened: &mut u32,
    sink: MessageSink,
) -> i32 {
    // SAFETY: the strings are NUL-terminated for the call; `opened` is valid for a write.
    unsafe { s::lemnos_open(client, device.as_ptr(), tag.as_ptr(), opened, sink) }.0
}

fn call(
    client: Handle,
    opened: u32,
    method: u16,
    args: &[u8],
    reply: &mut [u8],
    sink: MessageSink,
) -> i32 {
    // SAFETY: `args` and `reply` are valid for their lengths for the call.
    unsafe {
        s::lemnos_call(
            client,
            opened,
            method,
            args.as_ptr(),
            args.len(),
            reply.as_mut_ptr(),
            reply.len(),
            sink,
        )
    }
    .0
}

fn subscribe(
    client: Handle,
    opened: u32,
    method: u16,
    sub: &mut Subscription,
    sink: MessageSink,
) -> i32 {
    // SAFETY: `sub` is valid for a write for its whole size.
    unsafe { s::lemnos_subscribe(client, opened, method, 1000, 0, sub, sink) }.0
}

fn next_value(client: Handle, value: &mut Value, into: &mut [u8], sink: MessageSink) -> i32 {
    // SAFETY: `value` and `into` are valid for writes for their sizes.
    unsafe { s::lemnos_next_value(client, 2000, value, into.as_mut_ptr(), into.len(), sink) }.0
}

fn unsubscribe(client: Handle, id: u64, sink: MessageSink) -> i32 {
    s::lemnos_unsubscribe(client, id, sink).0
}

fn set_power(client: Handle, device: u32, on: u32, sink: MessageSink) -> i32 {
    s::lemnos_set_power(client, device, on, sink).0
}

fn item_count(client: Handle, device: u32, sink: MessageSink) -> i32 {
    s::lemnos_item_count(client, device, sink).0
}

fn item_info(client: Handle, device: u32, item: u32, info: &mut ItemInfo) -> i32 {
    // SAFETY: `info` is valid for a write for its whole size.
    unsafe { s::lemnos_item_info(client, device, item, info) }.0
}

fn item_text(client: Handle, device: u32, item: u32, field: u32) -> (i32, String) {
    let mut out = [0u8; 64];
    let mut len = 0usize;
    // SAFETY: `out` is writable for its capacity and `len` is valid for a write.
    let status = unsafe {
        s::lemnos_item_text(
            client,
            device,
            item,
            field,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut len,
        )
    }
    .0;
    (status, String::from_utf8_lossy(&out[..len]).into_owned())
}

fn package_list_call(client: Handle, mode: &mut u32, sink: MessageSink) -> i32 {
    // SAFETY: `mode` is valid for a write.
    unsafe { s::lemnos_package_list(client, mode, sink) }.0
}

fn package_info(client: Handle, package: u32, info: &mut PackageInfo) -> i32 {
    // SAFETY: `info` is valid for a write for its whole size.
    unsafe { s::lemnos_package_info(client, package, info) }.0
}

fn package_text(client: Handle, package: u32, field: u32) -> (i32, String) {
    let mut out = [0u8; 256];
    let mut len = 0usize;
    // SAFETY: `out` is writable for its capacity and `len` is valid for a write.
    let status = unsafe {
        s::lemnos_package_text(
            client,
            package,
            field,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut len,
        )
    }
    .0;
    (status, String::from_utf8_lossy(&out[..len]).into_owned())
}

fn stream_open(
    client: Handle,
    device: u32,
    name: &CStr,
    flags: u32,
    stream: &mut Handle,
    sink: MessageSink,
) -> i32 {
    // SAFETY: `name` is NUL-terminated for the call; `stream` is valid for a write.
    unsafe { s::lemnos_stream_open(client, device, name.as_ptr(), flags, stream, sink) }.0
}

fn stream_open_held(
    client: Handle,
    device: u32,
    name: &CStr,
    hold: u32,
    flags: u32,
    stream: &mut Handle,
    sink: MessageSink,
) -> i32 {
    // SAFETY: `name` is NUL-terminated for the call; `stream` is valid for a write.
    unsafe { s::lemnos_stream_open_held(client, device, name.as_ptr(), hold, flags, stream, sink) }
        .0
}

fn stream_info(client: Handle, stream: Handle, info: &mut StreamInfo) -> i32 {
    // SAFETY: `info` is valid for a write for its whole size.
    unsafe { s::lemnos_stream_info(client, stream, info) }.0
}

/// Takes the next frame and copies its bytes out while the surface still holds them. `None` for
/// the bytes when the frame has none here (an unmapped stream).
fn next_frame(
    client: Handle,
    stream: Handle,
    frame: &mut Frame,
    sink: MessageSink,
) -> (i32, Option<Vec<u8>>) {
    let mut data: *const u8 = core::ptr::null();
    // SAFETY: `frame` is valid for a write; `data` is valid for a write, and whatever it is set to
    // stays valid until this stream's next call, which is after the copy below.
    let status =
        unsafe { s::lemnos_stream_next_frame(client, stream, 5000, frame, &mut data, sink) }.0;
    if status != 1 || data.is_null() {
        return (status, None);
    }
    // SAFETY: the surface says the frame's bytes are `frame.len` long at `data`, and they are read
    // before any other call on this stream.
    let bytes = unsafe { core::slice::from_raw_parts(data, frame.len as usize) }.to_vec();
    (status, Some(bytes))
}

fn stream_release(client: Handle, stream: Handle, buffer: u32, sink: MessageSink) -> i32 {
    s::lemnos_stream_release(client, stream, buffer, sink).0
}

fn stream_read(
    client: Handle,
    stream: Handle,
    buffer: u32,
    offset: usize,
    out: &mut [u8],
    sink: MessageSink,
) -> i32 {
    // SAFETY: `out` is valid for writes for its length.
    unsafe {
        s::lemnos_stream_read(
            client,
            stream,
            buffer,
            offset,
            out.as_mut_ptr(),
            out.len(),
            sink,
        )
    }
    .0
}

fn stream_write(
    client: Handle,
    stream: Handle,
    buffer: u32,
    offset: usize,
    data: &[u8],
    sink: MessageSink,
) -> i32 {
    // SAFETY: `data` is valid for reads for its length.
    unsafe {
        s::lemnos_stream_write(
            client,
            stream,
            buffer,
            offset,
            data.as_ptr(),
            data.len(),
            sink,
        )
    }
    .0
}

fn stream_post_job(client: Handle, stream: Handle, job: &[u8], sink: MessageSink) -> i32 {
    // SAFETY: `job` is valid for reads for its length.
    unsafe { s::lemnos_stream_post_job(client, stream, job.as_ptr(), job.len(), sink) }.0
}

fn stream_buffer_handle(
    client: Handle,
    stream: Handle,
    buffer: u32,
    sink: MessageSink,
) -> (i32, Option<OwnedFd>) {
    let mut fd = -1i32;
    // SAFETY: `fd` is valid for a write.
    let status = unsafe { s::lemnos_stream_buffer_handle(client, stream, buffer, &mut fd, sink) }.0;
    (status, owned(status, fd))
}

fn stream_plane_len(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    len: &mut usize,
) -> i32 {
    // SAFETY: `len` is valid for a write.
    unsafe { s::lemnos_stream_plane_len(client, stream, buffer, plane, len) }.0
}

/// A plane's bytes, copied out while the surface holds them, with the length it gave.
fn stream_plane(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    len: &mut usize,
) -> (i32, Vec<u8>) {
    let mut data: *const u8 = core::ptr::null();
    // SAFETY: `data` and `len` are valid for writes.
    let status = unsafe { s::lemnos_stream_plane(client, stream, buffer, plane, &mut data, len) }.0;
    if status != OK || data.is_null() {
        return (status, Vec::new());
    }
    // SAFETY: the surface says `*len` bytes are at `data`, held until this stream's next call.
    (
        status,
        unsafe { core::slice::from_raw_parts(data, *len) }.to_vec(),
    )
}

fn stream_region(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    sink: MessageSink,
) -> (i32, StreamRegion, Option<OwnedFd>) {
    let mut region = StreamRegion {
        struct_size: core::mem::size_of::<StreamRegion>() as u32,
        fd: -1,
        ..StreamRegion::default()
    };
    // SAFETY: `region` is valid for a write for its whole size.
    let status =
        unsafe { s::lemnos_stream_region(client, stream, buffer, plane, &mut region, sink) }.0;
    let fd = owned(status, region.fd);
    (status, region, fd)
}

fn stream_pool_page(client: Handle, stream: Handle, sink: MessageSink) -> (i32, Option<OwnedFd>) {
    let mut fd = -1i32;
    // SAFETY: `fd` is valid for a write.
    let status = unsafe { s::lemnos_stream_pool_page(client, stream, &mut fd, sink) }.0;
    (status, owned(status, fd))
}

fn stream_generation(client: Handle, stream: Handle) -> (i32, u32) {
    let mut generation = 0u32;
    // SAFETY: `generation` is valid for a write.
    let status = unsafe { s::lemnos_stream_generation(client, stream, &mut generation) }.0;
    (status, generation)
}

fn stream_end(client: Handle, stream: Handle, kind: *mut u32) -> i32 {
    // SAFETY: `kind` is valid for a write, or null for the surface to refuse.
    unsafe { s::lemnos_stream_end(client, stream, kind) }.0
}

fn stream_producer(client: Handle, stream: Handle, state: *mut u32, since_ns: *mut u64) -> i32 {
    // SAFETY: both are valid for writes, or null for the surface to refuse.
    unsafe { s::lemnos_stream_producer(client, stream, state, since_ns) }.0
}

fn stream_skipped(client: Handle, stream: Handle, skipped: &mut u64) -> i32 {
    // SAFETY: `skipped` is valid for a write.
    unsafe { s::lemnos_stream_skipped(client, stream, skipped) }.0
}

fn stream_close(client: Handle, stream: Handle, sink: MessageSink) -> i32 {
    s::lemnos_stream_close(client, stream, sink).0
}

/// A descriptor the surface lent or handed over, owned here, or none when the call failed.
fn owned(status: i32, fd: i32) -> Option<OwnedFd> {
    if status != OK || fd < 0 {
        return None;
    }
    // SAFETY: on success the surface gives the caller a new descriptor it owns; this is the one
    // owner, and the OwnedFd closes it.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

// ---- The tests ----

/// A client on the device service, opened the way `client.c` opened it.
fn open(socket: &Path) -> Handle {
    let mut client = Handle(0);
    let mut sink = Sink::new();
    let socket = std::ffi::CString::new(socket.to_str().unwrap()).unwrap();
    let name = c"c-abi";
    let status = client_open(&socket, name, &mut client, sink.raw());
    assert_eq!(status, OK, "{}", sink.said());
    client
}

#[test]
fn the_surface_reports_its_version_as_the_header_does() {
    assert_eq!(check_version(), OK);
}

#[test]
fn a_client_reads_calls_and_subscribes_through_the_surface() {
    let (service, socket, dir) = service();
    let client = open(&socket);
    assert_eq!(device_count(client), 1);
    let (status, name) = device_name(client, 0);
    assert_eq!((status, name.as_str()), (OK, "thing"));
    let mut info = DeviceInfo {
        struct_size: core::mem::size_of::<DeviceInfo>() as u32,
        ..DeviceInfo::default()
    };
    assert_eq!(device_info(client, 0, &mut info), OK);
    assert_eq!(info.state, 0);

    let mut opened = 0u32;
    let mut sink = Sink::new();
    let contract = c"example.counter/1";
    let device = c"thing";
    assert_eq!(
        open_contract(client, device, contract, &mut opened, sink.raw()),
        OK,
        "{}",
        sink.said()
    );
    let mut reply = [0u8; 16];
    let used = call(client, opened, 1, &[], &mut reply, sink.raw());
    assert_eq!(used, 4, "{}", sink.said());
    assert_eq!(u32::from_le_bytes(reply[..4].try_into().unwrap()), 1);

    let mut sub = Subscription {
        struct_size: core::mem::size_of::<Subscription>() as u32,
        ..Subscription::default()
    };
    assert_eq!(
        subscribe(client, opened, 1, &mut sub, sink.raw()),
        OK,
        "{}",
        sink.said()
    );
    let mut streamed = 0;
    while streamed < 3 {
        let mut value = Value {
            struct_size: core::mem::size_of::<Value>() as u32,
            ..Value::default()
        };
        let mut into = [0u8; 16];
        let status = next_value(client, &mut value, &mut into, sink.raw());
        assert!(status >= 0, "{}", sink.said());
        if status == 1 && value.subscription == sub.id {
            streamed += 1;
        }
    }
    assert_eq!(unsubscribe(client, sub.id, sink.raw()), OK);

    let items = item_count(client, 0, sink.raw());
    assert!(items >= 1, "{}", sink.said());
    let mut item = ItemInfo {
        struct_size: core::mem::size_of::<ItemInfo>() as u32,
        ..ItemInfo::default()
    };
    assert_eq!(item_info(client, 0, 0, &mut item), OK);
    assert_eq!(item.method, 1);
    assert_eq!(item_text(client, 0, 0, 0), (OK, "count".to_string()));
    assert_eq!(
        item_text(client, 0, 0, 2),
        (OK, "example.counter/1".to_string())
    );

    // Power off: the counter's call fails; power on: it answers again.
    assert_eq!(set_power(client, 0, 0, sink.raw()), OK);
    assert!(call(client, opened, 1, &[], &mut reply, sink.raw()) < 0);
    assert_eq!(set_power(client, 0, 1, sink.raw()), OK);

    assert_eq!(client_close(client), OK);
    assert_eq!(
        device_count(client),
        E_STALE_HANDLE,
        "a closed client is stale"
    );
    service.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn frames_are_read_in_place_by_copy_and_through_each_buffers_handle() {
    let streams = stream_service();
    let producing = Producing::start(&streams);
    let client = open(&streams.socket);
    let mut sink = Sink::new();
    let mut stream = Handle(0);
    let frames = c"frames";
    assert_eq!(
        stream_open(client, 0, frames, 0, &mut stream, sink.raw()),
        OK,
        "{}",
        sink.said()
    );

    let mut info = StreamInfo {
        struct_size: core::mem::size_of::<StreamInfo>() as u32,
        ..StreamInfo::default()
    };
    assert_eq!(stream_info(client, stream, &mut info), OK);
    assert_eq!((info.buffers, info.buffer_bytes), (4, FRAME_BYTES as u32));
    assert_ne!(info.flags & s::STREAM_BUFFER_HANDLES, 0);
    assert_eq!(info.flags & s::STREAM_WRITABLE, 0);
    assert_ne!(info.flags & s::STREAM_OPEN, 0);

    for _ in 0..6 {
        let mut frame = Frame {
            struct_size: core::mem::size_of::<Frame>() as u32,
            ..Frame::default()
        };
        let (status, bytes) = next_frame(client, stream, &mut frame, sink.raw());
        assert_eq!(status, 1, "{}", sink.said());
        let bytes = bytes.expect("frame bytes");
        assert_eq!(frame.len as usize, FRAME_BYTES);
        assert!(frame_is_right(&bytes, frame.sequence), "frame in place");

        let mut copy = [0u8; 16];
        assert_eq!(
            stream_read(client, stream, frame.buffer, 100, &mut copy, sink.raw()),
            OK
        );
        assert_eq!(copy[..], bytes[100..116]);

        let (status, fd) = stream_buffer_handle(client, stream, frame.buffer, sink.raw());
        assert_eq!(status, OK);
        let fd = fd.expect("a descriptor for the buffer");
        let mut through = vec![0u8; FRAME_BYTES];
        std::fs::File::from(fd)
            .read_exact_at(&mut through, 0)
            .unwrap();
        assert!(
            frame_is_right(&through, frame.sequence),
            "frame through its handle"
        );

        assert_eq!(stream_release(client, stream, frame.buffer, sink.raw()), OK);
    }

    let mut skipped = 0u64;
    assert_eq!(stream_skipped(client, stream, &mut skipped), OK);
    let mut kind = 99u32;
    assert_eq!(stream_end(client, stream, &mut kind), OK);
    assert_eq!(kind, s::STREAM_END_NONE, "a running stream has no end");
    let (mut state, mut since) = (99u32, 99u64);
    assert_eq!(stream_producer(client, stream, &mut state, &mut since), OK);
    assert_eq!((state, since), (s::STREAM_PRODUCER_RUNNING, 0));
    assert_eq!(
        stream_producer(client, stream, core::ptr::null_mut(), &mut since),
        EINVAL
    );
    assert_eq!(stream_end(client, stream, core::ptr::null_mut()), EINVAL);

    // A buffer's region: its own memory at its offset holds the frame's bytes, with the pool page
    // and generation there for a second reader. The frame is held while the region is read.
    let mut frame = Frame {
        struct_size: core::mem::size_of::<Frame>() as u32,
        ..Frame::default()
    };
    let (status, bytes) = next_frame(client, stream, &mut frame, sink.raw());
    assert_eq!(status, 1);
    let bytes = bytes.expect("frame bytes");
    let (status, region, fd) = stream_region(client, stream, frame.buffer, 0, sink.raw());
    assert_eq!(status, OK, "{}", sink.said());
    let fd = fd.expect("a region descriptor");
    assert_ne!(region.flags & s::REGION_DMA_BUF, 0);
    assert_eq!(region.index, frame.buffer);
    assert_eq!(region.payload_bytes, FRAME_BYTES as u64);
    assert_eq!(region.tail_bytes, 0);
    assert!(region.allocated_bytes >= region.payload_bytes);
    let mut copy = [0u8; 16];
    std::fs::File::from(fd)
        .read_exact_at(&mut copy, region.offset + 100)
        .unwrap();
    assert_eq!(
        copy[..],
        bytes[100..116],
        "the region's bytes are the frame's"
    );
    let (status, page) = stream_pool_page(client, stream, sink.raw());
    assert!(status == OK && page.is_some(), "pool page");
    let (status, generation) = stream_generation(client, stream);
    assert!(status == OK && generation >= 1, "generation");
    assert_eq!(stream_release(client, stream, frame.buffer, sink.raw()), OK);

    // Refusals: a stream taken read-only, a read past a buffer, a buffer that is not there, a frame
    // that is not held, and a handle of no buffer.
    assert_eq!(
        stream_write(client, stream, 0, 0, &[1], MessageSink::NONE),
        EACCES
    );
    let mut two = [0u8; 2];
    assert_eq!(
        stream_read(
            client,
            stream,
            0,
            FRAME_BYTES - 1,
            &mut two,
            MessageSink::NONE
        ),
        EINVAL
    );
    assert_eq!(
        stream_read(
            client,
            stream,
            info.buffers,
            0,
            &mut two[..1],
            MessageSink::NONE
        ),
        EINVAL
    );
    assert_eq!(stream_release(client, stream, 0, MessageSink::NONE), EINVAL);
    assert_eq!(
        stream_buffer_handle(client, stream, info.buffers, MessageSink::NONE).0,
        EINVAL
    );

    assert_eq!(stream_close(client, stream, sink.raw()), OK);
    assert_eq!(stream_info(client, stream, &mut info), E_STALE_HANDLE);

    // The same stream taken unmapped: its frames and their lengths, and no bytes.
    let mut unmapped = Handle(0);
    assert_eq!(
        stream_open(
            client,
            0,
            frames,
            s::STREAM_OPEN_UNMAPPED | s::STREAM_WRITABLE,
            &mut unmapped,
            MessageSink::NONE
        ),
        EINVAL
    );
    assert_eq!(
        stream_open(
            client,
            0,
            frames,
            s::STREAM_OPEN_UNMAPPED,
            &mut unmapped,
            sink.raw()
        ),
        OK,
        "{}",
        sink.said()
    );
    assert_eq!(stream_info(client, unmapped, &mut info), OK);
    assert_ne!(info.flags & s::STREAM_UNMAPPED, 0);
    assert_eq!(info.flags & s::STREAM_WRITABLE, 0);
    let mut held = Frame {
        struct_size: core::mem::size_of::<Frame>() as u32,
        ..Frame::default()
    };
    let (status, bytes) = next_frame(client, unmapped, &mut held, sink.raw());
    assert_eq!(status, 1);
    assert!(bytes.is_none(), "an unmapped frame has no bytes here");
    assert_eq!(held.len as usize, FRAME_BYTES);
    let mut plane_len = 0usize;
    assert_eq!(
        stream_plane_len(client, unmapped, held.buffer, 0, &mut plane_len),
        OK
    );
    assert_eq!(plane_len, FRAME_BYTES);
    assert_eq!(
        stream_plane_len(client, unmapped, held.buffer, 1, &mut plane_len),
        EINVAL
    );
    let mut plane_bytes = 1usize;
    let (status, plane) = stream_plane(client, unmapped, held.buffer, 0, &mut plane_bytes);
    assert_eq!((status, plane.len()), (OK, 0), "an unmapped plane is empty");
    assert_eq!(
        stream_read(client, unmapped, held.buffer, 0, &mut copy[..4], sink.raw()),
        EOPNOTSUPP
    );
    assert_eq!(
        stream_release(client, unmapped, held.buffer, sink.raw()),
        OK
    );
    assert_eq!(stream_close(client, unmapped, sink.raw()), OK);

    drop(page);
    client_close(client);
    producing.stop();
}

#[test]
fn a_consumer_fills_a_stream_and_posts_jobs_the_producer_answers() {
    let streams = stream_service();
    let producing = Producing::start(&streams);
    let client = open(&streams.socket);
    let mut sink = Sink::new();
    let mut input = Handle(0);
    assert_eq!(
        stream_open(
            client,
            0,
            c"input",
            s::STREAM_WRITABLE,
            &mut input,
            sink.raw()
        ),
        OK,
        "{}",
        sink.said()
    );
    let mut info = StreamInfo {
        struct_size: core::mem::size_of::<StreamInfo>() as u32,
        ..StreamInfo::default()
    };
    assert_eq!(stream_info(client, input, &mut info), OK);
    assert_ne!(info.flags & s::STREAM_WRITABLE, 0);
    for round in 0..6u32 {
        let buffer = round % info.buffers;
        let image: Vec<u8> = (0..INPUT_BYTES)
            .map(|i| (round as usize + i * 3) as u8)
            .collect();
        assert_eq!(
            stream_write(client, input, buffer, 0, &image, sink.raw()),
            OK,
            "{}",
            sink.said()
        );
        assert_eq!(
            stream_post_job(client, input, &buffer.to_le_bytes(), sink.raw()),
            1,
            "{}",
            sink.said()
        );
        let mut frame = Frame {
            struct_size: core::mem::size_of::<Frame>() as u32,
            ..Frame::default()
        };
        let (status, bytes) = next_frame(client, input, &mut frame, sink.raw());
        assert_eq!(status, 1, "{}", sink.said());
        assert_eq!((frame.buffer, frame.len as usize), (buffer, INPUT_BYTES));
        let bytes = bytes.expect("the job's frame");
        assert!(bytes.iter().zip(&image).all(|(made, was)| *made == !was));
        assert_eq!(stream_release(client, input, frame.buffer, sink.raw()), OK);
    }
    // Past a buffer's end is refused.
    assert_eq!(
        stream_write(
            client,
            input,
            0,
            0,
            &[0u8; INPUT_BYTES + 1],
            MessageSink::NONE
        ),
        EINVAL
    );
    assert_eq!(stream_close(client, input, sink.raw()), OK);

    // Shared memory: no descriptor for a buffer.
    let mut plain = Handle(0);
    assert_eq!(
        stream_open(client, 0, c"plain", 0, &mut plain, sink.raw()),
        OK
    );
    assert_eq!(stream_info(client, plain, &mut info), OK);
    assert_eq!(info.flags & s::STREAM_BUFFER_HANDLES, 0);
    assert_eq!(
        stream_buffer_handle(client, plain, 0, MessageSink::NONE).0,
        EOPNOTSUPP
    );
    assert_eq!(stream_close(client, plain, sink.raw()), OK);

    // Not there, and a flag not known.
    let mut nothing = Handle(0);
    assert_eq!(
        stream_open(client, 0, c"nothing", 0, &mut nothing, MessageSink::NONE),
        ENOENT
    );
    let mut frames_again = Handle(0);
    assert_eq!(
        stream_open(
            client,
            0,
            c"frames",
            1u32 << 7,
            &mut frames_again,
            MessageSink::NONE
        ),
        EINVAL
    );

    client_close(client);
    producing.stop();
}

#[test]
fn a_client_closed_with_a_frame_held_gives_its_stream_up() {
    let streams = stream_service();
    let producing = Producing::start(&streams);
    let client = open(&streams.socket);
    let mut sink = Sink::new();
    let mut stream = Handle(0);
    assert_eq!(
        stream_open(client, 0, c"frames", 0, &mut stream, sink.raw()),
        OK
    );
    let mut frame = Frame {
        struct_size: core::mem::size_of::<Frame>() as u32,
        ..Frame::default()
    };
    let (status, _) = next_frame(client, stream, &mut frame, MessageSink::NONE);
    assert_eq!(status, 1, "a frame to hold");
    assert_eq!(client_close(client), OK);

    // The service lets go of the stream when it sees the connection end.
    let client = open(&streams.socket);
    let mut tries = 0;
    let status = loop {
        let status = stream_open(client, 0, c"frames", 0, &mut stream, MessageSink::NONE);
        tries += 1;
        if status != EBUSY || tries == 200 {
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status, OK, "taken again");
    let (status, _) = next_frame(client, stream, &mut frame, MessageSink::NONE);
    assert_eq!(status, 1, "a frame after");
    client_close(client);
    producing.stop();
}

#[test]
fn the_package_list_comes_through_the_surface() {
    let packages = package_service();
    let client = open(&packages.socket);
    let mut sink = Sink::new();
    let mut mode = 99u32;
    let count = package_list_call(client, &mut mode, sink.raw());
    assert_eq!(count, 2, "{}", sink.said());
    assert_eq!(mode, 1, "the signed mode, as the header names it");
    let (status, note) = package_text(client, 0, 3);
    assert_eq!(
        (status, note.as_str()),
        (OK, "one key file line was not a key")
    );

    let expected = package_list();
    for (index, entry) in expected.packages.iter().enumerate() {
        let mut info = PackageInfo {
            struct_size: core::mem::size_of::<PackageInfo>() as u32,
            ..PackageInfo::default()
        };
        assert_eq!(package_info(client, index as u32, &mut info), OK);
        assert_eq!(info.loaded != 0, entry.driver.is_some());
        assert_eq!(
            package_text(client, index as u32, 0),
            (OK, entry.path.clone())
        );
        assert_eq!(
            package_text(client, index as u32, 1),
            (OK, entry.driver.clone().unwrap_or_default())
        );
        assert_eq!(
            package_text(client, index as u32, 2),
            (OK, entry.reason.clone())
        );
    }
    let mut past = PackageInfo {
        struct_size: core::mem::size_of::<PackageInfo>() as u32,
        ..PackageInfo::default()
    };
    assert_eq!(package_info(client, count as u32, &mut past), ENOENT);
    client_close(client);
}

// ---- The C smoke program, against the library built here ----

/// The board the C surface reports is the board file's `[board] id`, as `lemnos_board_id` writes it.
#[test]
fn the_board_id_comes_through_the_surface() {
    let (service, socket, dir) = service();
    let client = open(&socket);
    let mut out = [0u8; 64];
    let mut len = 0usize;
    // SAFETY: `out` is writable for its capacity and `len` is valid for a write.
    let status = unsafe {
        s::lemnos_board_id(
            client,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut len,
            MessageSink::NONE,
        )
    }
    .0;
    assert_eq!(status, OK);
    assert_eq!(&out[..len], b"c", "the harness's board id is \"c\"");
    assert_eq!(out[len], 0, "NUL-terminated");
    drop(service);
    let _ = std::fs::remove_dir_all(dir);
}

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn smoke_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/c/smoke.c")
}

fn build_smoke(out: &Path, link: &[String]) {
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../include");
    let output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(&include)
        .arg(smoke_source())
        .arg("-o")
        .arg(out)
        .args(link)
        .output()
        .expect("cc runs");
    assert!(
        output.status.success(),
        "cc: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_smoke(program: &Path, env: &[(&str, &Path)]) {
    let streams = stream_service();
    let packages = package_service();
    let producing = Producing::start(&streams);
    let mut command = Command::new(program);
    command.arg(&streams.socket).arg(&packages.socket);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    for expected in [
        "device=cam",
        "stream buffers=4 bytes=5000",
        "stale stream status=-4096",
        "packages=2",
        "done",
    ] {
        assert!(stdout.contains(expected), "{expected:?} in {stdout}");
    }
    producing.stop();
}

#[cfg(not(feature = "shared"))]
#[test]
fn the_c_smoke_program_runs_on_the_static_library() {
    assert!(
        cc_available(),
        "the C smoke program needs a C compiler (cc)"
    );
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let lib = deps.join("liblemnos_capi.a");
    assert!(lib.exists(), "cargo builds {} for the tests", lib.display());
    let link = [lib.to_str().unwrap(), "-lpthread", "-ldl", "-lm"].map(String::from);
    let program = deps.join(format!("lemnos-capi-smoke-static-{}", std::process::id()));
    build_smoke(&program, &link);
    run_smoke(&program, &[]);
    let _ = std::fs::remove_file(&program);
}

#[test]
fn the_c_smoke_program_runs_on_the_shared_library() {
    assert!(
        cc_available(),
        "the C smoke program needs a C compiler (cc)"
    );
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let lib = shared_library();
    assert!(lib.exists(), "cargo builds {} for the tests", lib.display());
    let link = [
        "-L".to_string(),
        lib.parent().unwrap().to_str().unwrap().to_string(),
        format!("-l:{}", lib.file_name().unwrap().to_str().unwrap()),
        "-lm".to_string(),
    ];
    let program = deps.join(format!("lemnos-capi-smoke-shared-{}", std::process::id()));
    build_smoke(&program, &link);
    // The loader finds the library by its SONAME, through a directory of links to it.
    let dir = deps.join(format!("lemnos-capi-soname-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let soname = dir.join("liblemnos_capi.so.1");
    let _ = std::fs::remove_file(&soname);
    std::os::unix::fs::symlink(&lib, &soname).unwrap();
    run_smoke(&program, &[("LD_LIBRARY_PATH", &dir)]);
    let _ = std::fs::remove_file(&program);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(not(feature = "shared"))]
fn shared_library() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("liblemnos_capi.so")
}

#[cfg(feature = "shared")]
fn shared_library() -> PathBuf {
    Path::new(env!("LEMNOS_CAPI_SHARED_DIR")).join("liblemnos_capi.so.1")
}

/// A consumer declares its hold through the C surface. On the four-buffer stream a declaration of 2
/// fits, and a declaration of 3 is refused as BUSY with its reason in the sink, and nothing is
/// attached: a plain open of the same stream then succeeds. Flags other than none are EINVAL.
#[test]
fn a_consumer_declares_its_hold_through_the_c_surface() {
    let streams = stream_service();
    let client = open(&streams.socket);
    let frames = c"frames";

    let mut sink = Sink::new();
    let mut stream = Handle(0);
    assert_eq!(
        stream_open_held(client, 0, frames, 2, 1 << 1, &mut stream, sink.raw()),
        EINVAL,
        "an unmapped declared open is not offered"
    );

    let mut refused = Handle(0);
    let mut sink = Sink::new();
    assert_eq!(
        stream_open_held(client, 0, frames, 3, 0, &mut refused, sink.raw()),
        EBUSY,
        "a declaration of 3 on 4 buffers does not fit"
    );
    assert!(sink.said().contains("declares 3"), "{}", sink.said());

    let mut sink = Sink::new();
    let mut held = Handle(0);
    assert_eq!(
        stream_open_held(client, 0, frames, 2, 0, &mut held, sink.raw()),
        OK,
        "{}",
        sink.said()
    );
    assert_eq!(stream_close(client, held, MessageSink::NONE), OK);
    // Once that stream is given up, a writer declares the same way.
    let mut sink = Sink::new();
    let mut writer = Handle(0);
    assert_eq!(
        stream_open_held(client, 0, frames, 2, 1 << 0, &mut writer, sink.raw()),
        OK,
        "{}",
        sink.said()
    );
    assert_eq!(stream_close(client, writer, MessageSink::NONE), OK);
}
