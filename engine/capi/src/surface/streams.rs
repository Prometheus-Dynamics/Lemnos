//! A device's frame streams: taking one, frames read in place, buffers read and
//! written by copy, jobs, and a buffer's own descriptor on a stream of
//! dma-bufs, each plane's when the buffers are in planes.
//!
//! Each stream has its own lock, so waiting for a frame holds up neither the
//! client's other calls nor its other streams. Nothing on the way to a frame or
//! back allocates.

use core::ffi::c_char;
use std::os::fd::IntoRawFd;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_abi::{Handle, MessageSink, Status, guard};
use lemnos_client::{Error, FrameStream, HeldFrame, StreamPlatform};
use lemnos_wire::ring::RingError;

use super::{checked, count, entry, fail, out_bytes, text, with_entry};

/// `lemnos_stream_open` flag, and `LemnosStreamInfo` flag: the buffers can be
/// written.
pub const STREAM_WRITABLE: u32 = 1 << 0;
/// `LemnosStreamInfo` flag: each buffer is a dma-buf of its own.
pub const STREAM_BUFFER_HANDLES: u32 = 1 << 1;
/// `LemnosStreamInfo` flag: the host has not ended the stream.
pub const STREAM_OPEN: u32 = 1 << 2;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamInfo {
    pub struct_size: u32,
    pub flags: u32,
    pub buffers: u32,
    pub buffer_bytes: u32,
    pub frame_slots: u32,
    pub job_slots: u32,
    pub job_bytes: u32,
    /// The memory planes of each buffer: 1 unless the board file gives more.
    pub planes: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame {
    pub struct_size: u32,
    pub buffer: u32,
    pub len: u32,
    pub reserved: u32,
    pub sequence: u64,
    pub identity: u64,
    pub timestamp_ns: u64,
}

/// A stream and the frames the caller holds, one place per buffer, made when
/// the stream is taken. The ring never hands out a buffer that is held, so a
/// place is never taken twice.
pub(super) struct Stream {
    stream: FrameStream,
    held: Box<[Option<HeldFrame>]>,
}

/// `None` once closed, for a caller that still had the slot.
pub(super) type StreamSlot = Arc<Mutex<Option<Stream>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(target_os = "linux")]
fn platform() -> Option<Arc<dyn StreamPlatform>> {
    Some(Arc::new(lemnos_linux_stream::LinuxStreams))
}

#[cfg(not(target_os = "linux"))]
fn platform() -> Option<Arc<dyn StreamPlatform>> {
    None
}

/// Finds the stream under the client's lock, then works on it under its own.
fn with_stream(client: Handle, stream: Handle, f: impl FnOnce(&mut Stream) -> Status) -> Status {
    let slot = match entry(client).and_then(|entry| {
        let entry = lock(&entry);
        entry.streams.get(stream).map(Arc::clone)
    }) {
        Ok(slot) => slot,
        Err(status) => return status,
    };
    let mut slot = lock(&slot);
    match slot.as_mut() {
        Some(stream) => f(stream),
        None => Status::STALE_HANDLE,
    }
}

/// A buffer or range the caller named that the stream doesn't have is the
/// caller's mistake, not the producer's.
fn fail_access(error: Error, sink: MessageSink) -> Status {
    match error {
        Error::Stream(RingError::BadBuffer | RingError::BadLength) => {
            sink.write_fmt(format_args!("{error}"));
            Status::INVALID_ARGUMENT
        }
        error => fail(error, sink),
    }
}

/// # Safety
/// `name` is NUL-terminated; `stream` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_open(
    client: Handle,
    device: u32,
    name: *const c_char,
    flags: u32,
    stream: *mut Handle,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if stream.is_null() || flags & !STREAM_WRITABLE != 0 {
            return Status::INVALID_ARGUMENT;
        }
        // SAFETY: NUL-terminated by the contract, read only in this call.
        let name = match unsafe { text(name) } {
            Ok(name) => name,
            Err(status) => return status,
        };
        let Some(platform) = platform() else {
            return Status::NOT_SUPPORTED;
        };
        with_entry(client, |entry| {
            let Some(index) = entry.devices.get(device as usize).map(|d| d.index) else {
                return Status::NOT_FOUND;
            };
            let taken = if flags & STREAM_WRITABLE != 0 {
                entry.session.open_stream_writable(index, name, platform)
            } else {
                entry.session.open_stream(index, name, platform)
            };
            let taken = match taken {
                Ok(taken) => taken,
                Err(error) => return fail(error, sink),
            };
            let held = (0..taken.layout().buffers).map(|_| None).collect();
            let slot = Arc::new(Mutex::new(Some(Stream {
                stream: taken,
                held,
            })));
            match entry.streams.insert(Arc::clone(&slot)) {
                Ok(handle) => {
                    // SAFETY: not null, and valid for a write by the contract.
                    unsafe { stream.write(handle) };
                    Status::OK
                }
                Err(status) => {
                    if let Some(Stream { stream, .. }) = lock(&slot).take() {
                        let _ = entry.session.close_stream(stream);
                    }
                    status
                }
            }
        })
    })
}

/// # Safety
/// `info` is null or valid for its declared `struct_size`.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_info(
    client: Handle,
    stream: Handle,
    info: *mut StreamInfo,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let info = match unsafe { checked(info) } {
            Ok(info) => info,
            Err(status) => return status,
        };
        with_stream(client, stream, |s| {
            let layout = s.stream.layout();
            info.buffers = layout.buffers;
            info.buffer_bytes = layout.buffer_bytes;
            info.frame_slots = layout.frame_slots;
            info.job_slots = layout.job_slots;
            info.job_bytes = layout.job_bytes;
            info.planes = s.stream.planes().count();
            info.flags = [
                (s.stream.is_writable(), STREAM_WRITABLE),
                (s.stream.has_buffer_handles(), STREAM_BUFFER_HANDLES),
                (s.stream.is_open(), STREAM_OPEN),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |flags, (_, flag)| flags | flag);
            Status::OK
        })
    })
}

/// # Safety
/// `frame` is null or valid for its declared `struct_size`; `data` is null or
/// valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_next_frame(
    client: Handle,
    stream: Handle,
    timeout_ms: u32,
    frame: *mut Frame,
    data: *mut *const u8,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let frame = match unsafe { checked(frame) } {
            Ok(frame) => frame,
            Err(status) => return status,
        };
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        with_stream(client, stream, |s| {
            let got = match s.stream.next_frame(timeout) {
                Ok(Some(got)) => got,
                Ok(None) => return Status::OK,
                Err(error) => return fail(error, sink),
            };
            frame.buffer = got.buffer();
            frame.len = got.len() as u32;
            frame.sequence = got.sequence();
            frame.identity = got.identity();
            frame.timestamp_ns = got.timestamp_ns();
            if !data.is_null() {
                // A frame in planes is not one piece of memory: `lemnos_stream_plane` reads
                // each, and `data` says so with NULL rather than point at less than `len`.
                let bytes = if s.stream.planes().count() > 1 {
                    core::ptr::null()
                } else {
                    s.stream.bytes(&got).as_ptr()
                };
                // SAFETY: not null, and valid for a write by the contract.
                unsafe { data.write(bytes) };
            }
            match s.held.get_mut(got.buffer() as usize) {
                Some(place @ None) => {
                    *place = Some(got);
                    Status(1)
                }
                // The ring hands out only a buffer it has and the caller doesn't hold.
                _ => {
                    let _ = s.stream.release(got);
                    Status::IO
                }
            }
        })
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_stream_release(
    client: Handle,
    stream: Handle,
    buffer: u32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        with_stream(client, stream, |s| {
            let Some(frame) = s.held.get_mut(buffer as usize).and_then(Option::take) else {
                return Status::INVALID_ARGUMENT;
            };
            match s.stream.release(frame) {
                Ok(()) => Status::OK,
                Err(error) => fail(error, sink),
            }
        })
    })
}

/// # Safety
/// `out` is valid for writes of `len` bytes (or null with 0).
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_read(
    client: Handle,
    stream: Handle,
    buffer: u32,
    offset: usize,
    out: *mut u8,
    len: usize,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let out: &mut [u8] = match unsafe { out_bytes(out, len) } {
            Some(out) => out,
            None if len == 0 => &mut [],
            None => return Status::INVALID_ARGUMENT,
        };
        with_stream(client, stream, |s| {
            match s.stream.read(buffer, offset, out) {
                Ok(()) => Status::OK,
                Err(error) => fail_access(error, sink),
            }
        })
    })
}

/// # Safety
/// `data` is valid for reads of `len` bytes (or null with 0).
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_write(
    client: Handle,
    stream: Handle,
    buffer: u32,
    offset: usize,
    data: *const u8,
    len: usize,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let data = match unsafe { in_bytes(data, len) } {
            Ok(data) => data,
            Err(status) => return status,
        };
        with_stream(client, stream, |s| {
            match s.stream.write(buffer, offset, data) {
                Ok(()) => Status::OK,
                Err(error) => fail_access(error, sink),
            }
        })
    })
}

/// # Safety
/// `job` is valid for reads of `len` bytes (or null with 0).
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_post_job(
    client: Handle,
    stream: Handle,
    job: *const u8,
    len: usize,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let job = match unsafe { in_bytes(job, len) } {
            Ok(job) => job,
            Err(status) => return status,
        };
        with_stream(client, stream, |s| match s.stream.post_job(job) {
            Ok(posted) => count(usize::from(posted)),
            Err(error @ Error::Stream(RingError::JobTooLarge | RingError::BadLength)) => {
                sink.write_fmt(format_args!("{error}"));
                Status::INVALID_ARGUMENT
            }
            Err(error) => fail(error, sink),
        })
    })
}

/// # Safety
/// `fd` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_buffer_handle(
    client: Handle,
    stream: Handle,
    buffer: u32,
    fd: *mut i32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if fd.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| {
            let Some(handle) = s.stream.buffer_handle(buffer) else {
                if s.stream.has_buffer_handles() {
                    return Status::INVALID_ARGUMENT;
                }
                sink.write_fmt(format_args!(
                    "the stream's buffers are one piece of shared memory, with no handle each"
                ));
                return Status::NOT_SUPPORTED;
            };
            match handle.try_clone_to_owned() {
                Ok(owned) => {
                    // SAFETY: not null, and valid for a write by the contract.
                    unsafe { fd.write(owned.into_raw_fd()) };
                    Status::OK
                }
                Err(error) => {
                    sink.write_fmt(format_args!("{error}"));
                    Status::from_io_error(&error)
                }
            }
        })
    })
}

/// # Safety
/// `data` and `len` are each null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_plane(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    data: *mut *const u8,
    len: *mut usize,
) -> Status {
    guard(|| {
        if data.is_null() || len.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| {
            if plane >= s.stream.planes().count() {
                return Status::INVALID_ARGUMENT;
            }
            let Some(Some(frame)) = s.held.get(buffer as usize) else {
                return Status::INVALID_ARGUMENT;
            };
            let bytes = s.stream.plane(frame, plane);
            // SAFETY: not null, and valid for a write by the contract.
            unsafe { data.write(bytes.as_ptr()) };
            // SAFETY: not null, and valid for a write by the contract.
            unsafe { len.write(bytes.len()) };
            Status::OK
        })
    })
}

/// # Safety
/// `fd` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_plane_handle(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    fd: *mut i32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if fd.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| {
            let Some(handle) = s.stream.plane_handle(buffer, plane) else {
                if s.stream.has_buffer_handles() {
                    return Status::INVALID_ARGUMENT;
                }
                sink.write_fmt(format_args!(
                    "the stream's buffers are one piece of shared memory, with no handle each"
                ));
                return Status::NOT_SUPPORTED;
            };
            match handle.try_clone_to_owned() {
                Ok(owned) => {
                    // SAFETY: not null, and valid for a write by the contract.
                    unsafe { fd.write(owned.into_raw_fd()) };
                    Status::OK
                }
                Err(error) => {
                    sink.write_fmt(format_args!("{error}"));
                    Status::from_io_error(&error)
                }
            }
        })
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_stream_close(client: Handle, stream: Handle, sink: MessageSink) -> Status {
    guard(|| {
        let slot = match entry(client).and_then(|entry| lock(&entry).streams.remove(stream)) {
            Ok(slot) => slot,
            Err(status) => return status,
        };
        // Waits for a call on the stream that is under way.
        let Some(Stream { mut stream, held }) = lock(&slot).take() else {
            return Status::STALE_HANDLE;
        };
        for frame in held.into_iter().flatten() {
            let _ = stream.release(frame);
        }
        with_entry(client, |entry| match entry.session.close_stream(stream) {
            Ok(()) => Status::OK,
            Err(error) => fail(error, sink),
        })
    })
}

/// # Safety
/// `data` is valid for reads of `len` bytes, or null with 0.
unsafe fn in_bytes<'a>(data: *const u8, len: usize) -> Result<&'a [u8], Status> {
    if len == 0 {
        Ok(&[])
    } else if data.is_null() {
        Err(Status::INVALID_ARGUMENT)
    } else {
        // SAFETY: not null, and valid for `len` bytes by the contract.
        Ok(unsafe { core::slice::from_raw_parts(data, len) })
    }
}
