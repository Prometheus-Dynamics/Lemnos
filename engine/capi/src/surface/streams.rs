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
use lemnos_wire::ring::{RingError, SharedLayout};

use super::{checked, count, entry, fail, out_bytes, text, with_entry};

/// `lemnos_stream_open` flag, and `LemnosStreamInfo` flag: the buffers can be
/// written.
pub const STREAM_WRITABLE: u32 = 1 << 0;
/// `lemnos_stream_open` flag: map none of the buffers (see
/// `FrameStream::open_stream_unmapped`). Not with `STREAM_WRITABLE`.
pub const STREAM_OPEN_UNMAPPED: u32 = 1 << 1;
/// `LemnosStreamInfo` flag: each buffer is a dma-buf of its own.
pub const STREAM_BUFFER_HANDLES: u32 = 1 << 1;
/// `LemnosStreamInfo` flag: the host has not ended the stream.
pub const STREAM_OPEN: u32 = 1 << 2;
/// `LemnosStreamInfo` flag: the buffers' memory is not mapped by this stream, so
/// its frames have no bytes here.
pub const STREAM_UNMAPPED: u32 = 1 << 3;
/// `LemnosStreamRegion` flag: the descriptor allows a writable mapping.
pub const REGION_WRITABLE: u32 = 1 << 0;
/// `LemnosStreamRegion` flag: the descriptor is a dma-buf of its own (else a memfd of the pool).
pub const REGION_DMA_BUF: u32 = 1 << 1;

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

/// One plane of one buffer: where its bytes are. `fd` is a new descriptor the caller owns and
/// closes; the rest is as `lemnos_stream_region` documents.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamRegion {
    pub struct_size: u32,
    pub flags: u32,
    pub index: u32,
    pub fd: i32,
    pub offset: u64,
    pub payload_bytes: u64,
    pub allocated_bytes: u64,
    pub tail_bytes: u64,
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
        let known = STREAM_WRITABLE | STREAM_OPEN_UNMAPPED;
        if stream.is_null() || flags & !known != 0 || flags & known == known {
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
            entry.layout_refusal = None;
            let Some(index) = entry.devices.get(device as usize).map(|d| d.index) else {
                return Status::NOT_FOUND;
            };
            let taken = if flags & STREAM_WRITABLE != 0 {
                entry.session.open_stream_writable(index, name, platform)
            } else if flags & STREAM_OPEN_UNMAPPED != 0 {
                entry.session.open_stream_unmapped(index, name, platform)
            } else {
                entry.session.open_stream(index, name, platform)
            };
            let taken = match taken {
                Ok(taken) => taken,
                Err(error) => {
                    entry.layout_refusal = layout_refusal(&error);
                    return fail(error, sink);
                }
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

/// `LEMNOS_LAYOUT_RING`: the frame ring's header, which `lemnos_layout_refusal` reports.
pub const LAYOUT_RING: u32 = 0;
/// `LEMNOS_LAYOUT_MARKS`: a reader's marks page.
pub const LAYOUT_MARKS: u32 = 1;

/// The shared layout a stream open refused: the structure, and the value it found and the one this
/// build reads.
#[derive(Clone, Copy, Debug)]
pub(super) struct LayoutRefusal {
    which: u32,
    found: u64,
    expected: u64,
}

/// The refusal an open's error is, when the error is a shared layout's.
fn layout_refusal(error: &Error) -> Option<LayoutRefusal> {
    let Error::Stream(RingError::Layout {
        which,
        found,
        expected,
    }) = error
    else {
        return None;
    };
    let which = match which {
        SharedLayout::Ring => LAYOUT_RING,
        SharedLayout::Marks => LAYOUT_MARKS,
    };
    Some(LayoutRefusal {
        which,
        found: *found,
        expected: *expected,
    })
}

/// # Safety
/// `which`, `found` and `expected` are each null, or valid for a write during the call.
/// Reports the shared layout that the client's last `lemnos_stream_open` refused with
/// `LEMNOS_E_LAYOUT_MISMATCH`: which structure (`LEMNOS_LAYOUT_RING` or `LEMNOS_LAYOUT_MARKS`), the
/// value it found and the value this build reads. `-ENOENT` when that open refused no layout, or
/// none was made; the next open on the client replaces what this reports.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_layout_refusal(
    client: Handle,
    which: *mut u32,
    found: *mut u64,
    expected: *mut u64,
) -> Status {
    guard(|| {
        if which.is_null() || found.is_null() || expected.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_entry(client, |entry| {
            let Some(refusal) = entry.layout_refusal else {
                return Status::NOT_FOUND;
            };
            // SAFETY: not null, and valid for a write during this call.
            unsafe { which.write(refusal.which) };
            // SAFETY: not null, and valid for a write during this call.
            unsafe { found.write(refusal.found) };
            // SAFETY: not null, and valid for a write during this call.
            unsafe { expected.write(refusal.expected) };
            Status::OK
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
                (!s.stream.is_mapped(), STREAM_UNMAPPED),
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
                // An unmapped stream has no bytes here, so it is NULL too.
                let bytes = if s.stream.planes().count() > 1 || !s.stream.is_mapped() {
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

/// The bytes of plane `plane` of the held frame in `buffer`, as `lemnos_stream_plane`
/// gives them, but only the length: `len` is set to what `lemnos_stream_plane` would
/// return, with no mapping of the buffers. Works on an unmapped stream. The same
/// errors as `lemnos_stream_plane`.
///
/// # Safety
/// `len` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_plane_len(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    len: *mut usize,
) -> Status {
    guard(|| {
        if len.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| {
            if plane >= s.stream.planes().count() {
                return Status::INVALID_ARGUMENT;
            }
            let Some(Some(frame)) = s.held.get(buffer as usize) else {
                return Status::INVALID_ARGUMENT;
            };
            // SAFETY: not null, and valid for a write by the contract.
            unsafe { len.write(s.stream.plane_len(frame, plane)) };
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

/// Where a plane of a buffer is, and what memory it's in. `region` is filled in with `struct_size`
/// set: `flags` (`REGION_WRITABLE`, `REGION_DMA_BUF`), `index` (the pool index: buffer times planes
/// plus plane), `fd` (a new descriptor of the memory, which the caller closes), `offset` (where the
/// plane starts in it), `payload_bytes` (what the contract may use), `allocated_bytes` (what
/// exists from `offset`), and `tail_bytes` (0 today). A buffer or plane the stream doesn't have is
/// -EINVAL. See `docs/streams.md` for who may touch the memory and when.
///
/// # Safety
/// `region` is null or valid for its declared `struct_size`.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_region(
    client: Handle,
    stream: Handle,
    buffer: u32,
    plane: u32,
    region: *mut StreamRegion,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let region = match unsafe { checked(region) } {
            Ok(region) => region,
            Err(status) => return status,
        };
        with_stream(client, stream, |s| {
            let Some(got) = s.stream.region(buffer, plane) else {
                return Status::INVALID_ARGUMENT;
            };
            let fd = match got.fd.try_clone_to_owned() {
                Ok(fd) => fd,
                Err(error) => {
                    sink.write_fmt(format_args!("{error}"));
                    return Status::from_io_error(&error);
                }
            };
            region.flags = [
                (got.writable, REGION_WRITABLE),
                (
                    got.kind == lemnos_client::MemoryKind::DmaBuf,
                    REGION_DMA_BUF,
                ),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |flags, (_, flag)| flags | flag);
            region.index = got.index;
            region.fd = fd.into_raw_fd();
            region.offset = got.offset;
            region.payload_bytes = got.payload_bytes;
            region.allocated_bytes = got.allocated_bytes;
            region.tail_bytes = got.tail_bytes;
            Status::OK
        })
    })
}

/// A new descriptor of the pool page, read-only, for a second reader of the pool. Its rules are
/// `FrameStream::pool_page`'s.
///
/// # Safety
/// `fd` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_pool_page(
    client: Handle,
    stream: Handle,
    fd: *mut i32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if fd.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| match s.stream.pool_page() {
            Ok(page) => {
                // SAFETY: not null, and valid for a write by the contract.
                unsafe { fd.write(page.into_raw_fd()) };
                Status::OK
            }
            Err(error) => {
                sink.write_fmt(format_args!("{error}"));
                Status::from_io_error(&error)
            }
        })
    })
}

/// The pool's generation (see `FrameStream::generation`).
///
/// # Safety
/// `generation` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_generation(
    client: Handle,
    stream: Handle,
    generation: *mut u32,
) -> Status {
    guard(|| {
        if generation.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| {
            // SAFETY: not null, and valid for a write by the contract.
            unsafe { generation.write(s.stream.generation()) };
            Status::OK
        })
    })
}

/// How many frames the stream's reader was too late for, in total (see `FrameStream::skipped`).
///
/// # Safety
/// `skipped` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_stream_skipped(
    client: Handle,
    stream: Handle,
    skipped: *mut u64,
) -> Status {
    guard(|| {
        if skipped.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_stream(client, stream, |s| {
            // SAFETY: not null, and valid for a write by the contract.
            unsafe { skipped.write(s.stream.skipped()) };
            Status::OK
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

#[cfg(test)]
mod tests {
    use super::{LAYOUT_MARKS, LAYOUT_RING, layout_refusal};
    use lemnos_abi::Status;
    use lemnos_client::Error;
    use lemnos_wire::ring::{RingError, SharedLayout};

    #[test]
    fn a_layout_refusal_is_reported_with_its_structure_and_values() {
        let marks = Error::Stream(RingError::Layout {
            which: SharedLayout::Marks,
            found: 1,
            expected: 2,
        });
        let refusal = layout_refusal(&marks).expect("a layout refusal");
        assert_eq!(
            (refusal.which, refusal.found, refusal.expected),
            (LAYOUT_MARKS, 1, 2)
        );
        assert_eq!(Status::from(marks), Status::LAYOUT_MISMATCH);

        let ring = Error::Stream(RingError::Layout {
            which: SharedLayout::Ring,
            found: 0,
            expected: 2,
        });
        let refusal = layout_refusal(&ring).expect("a layout refusal");
        assert_eq!((refusal.which, refusal.found), (LAYOUT_RING, 0));
    }

    #[test]
    fn an_error_that_is_not_a_layout_refusal_reports_none() {
        assert!(layout_refusal(&Error::Stream(RingError::BadBuffer)).is_none());
        assert!(layout_refusal(&Error::Stream(RingError::Shm("x".into()))).is_none());
    }
}
