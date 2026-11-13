//! A Rust client that goes through the C surface, so a program can share one
//! installed library instead of compiling the client in.

use core::ffi::c_char;
use std::ffi::CString;
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::Duration;

use lemnos_abi::{Handle, MessageSink, Status};
use lemnos_client::{PackageEntry, PackageList, PackageMode};

use crate::ffi;
use crate::surface;

/// One value from a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value {
    pub subscription: u64,
    pub stamp_ns: u64,
    pub len: usize,
}

/// A connection to the service, through the C surface.
#[derive(Debug)]
pub struct Client {
    handle: Handle,
}

fn c_string(text: &str) -> Result<CString, Status> {
    CString::new(text).map_err(|_| Status::INVALID_ARGUMENT)
}

const fn checked(status: Status) -> Result<u32, Status> {
    if status.0 < 0 {
        Err(status)
    } else {
        Ok(status.0 as u32)
    }
}

/// Text from one of the surface's text calls, `get(out, capacity, len)`, at
/// whatever length it is.
fn text_of(
    mut get: impl FnMut(*mut c_char, usize, &mut usize) -> Status,
) -> Result<String, Status> {
    let mut bytes = vec![0u8; 256];
    loop {
        let mut len = 0;
        let status = get(bytes.as_mut_ptr().cast(), bytes.len(), &mut len);
        if status == Status::BUFFER_TOO_SMALL && len >= bytes.len() {
            bytes.resize(len + 1, 0);
            continue;
        }
        checked(status)?;
        bytes.truncate(len);
        return String::from_utf8(bytes).map_err(|_| Status::IO);
    }
}

/// One frame from a stream, read in place until it is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFrame<'a> {
    pub buffer: u32,
    pub sequence: u64,
    pub identity: u64,
    pub timestamp_ns: u64,
    pub bytes: &'a [u8],
}

/// A device's frame stream, through the C surface. Closed when dropped.
#[derive(Debug)]
pub struct Stream<'c> {
    client: &'c Client,
    handle: Handle,
}

impl Client {
    pub fn connect(socket: &str, name: &str, timeout: Duration) -> Result<Self, Status> {
        let version = surface::VERSION;
        // SAFETY: a valid version struct, read only during the call.
        checked(unsafe { ffi::lemnos_check_version(&version) })?;
        let (socket, name) = (c_string(socket)?, c_string(name)?);
        let mut handle = Handle::NULL;
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: both strings are NUL-terminated and outlive the call, and
        // `handle` is valid for a write.
        checked(unsafe {
            ffi::lemnos_client_open(
                socket.as_ptr(),
                name.as_ptr(),
                timeout_ms,
                &mut handle,
                MessageSink::NONE,
            )
        })?;
        Ok(Self { handle })
    }

    /// Refreshes the list and says how many devices it has.
    pub fn device_count(&self) -> Result<u32, Status> {
        checked(ffi::lemnos_device_count(self.handle))
    }

    pub fn device_name(&self, device: u32) -> Result<String, Status> {
        text_of(|out, capacity, len| {
            // SAFETY: `out` is valid for `capacity` bytes and `len` for a write.
            unsafe { ffi::lemnos_device_text(self.handle, device, 0, out, capacity, len) }
        })
    }

    /// What became of the service's driver packages.
    pub fn packages(&self) -> Result<PackageList, Status> {
        let mut mode = 0;
        // SAFETY: `mode` is valid for a write.
        let count = checked(unsafe {
            ffi::lemnos_package_list(self.handle, &mut mode, MessageSink::NONE)
        })?;
        let mode = match mode {
            0 => PackageMode::Off,
            1 => PackageMode::Signed,
            2 => PackageMode::Open,
            _ => return Err(Status::IO),
        };
        let field = |package, field| {
            text_of(|out, capacity, len| {
                // SAFETY: `out` is valid for `capacity` bytes and `len` for a write.
                unsafe { ffi::lemnos_package_text(self.handle, package, field, out, capacity, len) }
            })
        };
        let mut packages = Vec::with_capacity(count as usize);
        for package in 0..count {
            let mut info = surface::PackageInfo {
                struct_size: size_of::<surface::PackageInfo>() as u32,
                ..Default::default()
            };
            // SAFETY: `info` is valid for its declared size during the call.
            checked(unsafe { ffi::lemnos_package_info(self.handle, package, &mut info) })?;
            packages.push(PackageEntry {
                path: field(package, 0)?,
                driver: if info.loaded != 0 {
                    Some(field(package, 1)?)
                } else {
                    None
                },
                reason: field(package, 2)?,
            });
        }
        Ok(PackageList {
            mode,
            note: field(0, 3)?,
            packages,
        })
    }

    /// Takes a device's stream, by its place in the list; `writable` for one
    /// the consumer fills.
    pub fn open_stream(
        &self,
        device: u32,
        name: &str,
        writable: bool,
    ) -> Result<Stream<'_>, Status> {
        let flags = if writable {
            surface::STREAM_WRITABLE
        } else {
            0
        };
        self.open_stream_with(device, name, flags)
    }

    /// Takes a device's stream with none of its buffers mapped (see
    /// `lemnos_stream_open` and `LEMNOS_STREAM_OPEN_UNMAPPED`).
    pub fn open_stream_unmapped(&self, device: u32, name: &str) -> Result<Stream<'_>, Status> {
        self.open_stream_with(device, name, surface::STREAM_OPEN_UNMAPPED)
    }

    fn open_stream_with(&self, device: u32, name: &str, flags: u32) -> Result<Stream<'_>, Status> {
        let name = c_string(name)?;
        let mut handle = Handle::NULL;
        // SAFETY: the string is NUL-terminated and outlives the call, and
        // `handle` is valid for a write.
        checked(unsafe {
            ffi::lemnos_stream_open(
                self.handle,
                device,
                name.as_ptr(),
                flags,
                &mut handle,
                MessageSink::NONE,
            )
        })?;
        Ok(Stream {
            client: self,
            handle,
        })
    }

    /// Opens a contract, returning the number the other calls take.
    pub fn open(&self, device: &str, tag: &str) -> Result<u32, Status> {
        let (device, tag) = (c_string(device)?, c_string(tag)?);
        let mut opened = 0;
        // SAFETY: both strings are NUL-terminated and outlive the call, and
        // `opened` is valid for a write.
        checked(unsafe {
            ffi::lemnos_open(
                self.handle,
                device.as_ptr(),
                tag.as_ptr(),
                &mut opened,
                MessageSink::NONE,
            )
        })?;
        Ok(opened)
    }

    pub fn call(
        &self,
        opened: u32,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, Status> {
        // SAFETY: both slices are valid for their lengths during the call.
        let n = checked(unsafe {
            ffi::lemnos_call(
                self.handle,
                opened,
                method,
                args.as_ptr(),
                args.len(),
                reply.as_mut_ptr(),
                reply.len(),
                MessageSink::NONE,
            )
        })?;
        Ok(n as usize)
    }

    /// Returns the subscription's id.
    pub fn subscribe(&self, opened: u32, method: u16, period_us: u64) -> Result<u64, Status> {
        let mut out = surface::Subscription {
            struct_size: size_of::<surface::Subscription>() as u32,
            ..Default::default()
        };
        // SAFETY: `out` is valid for its declared size during the call.
        checked(unsafe {
            ffi::lemnos_subscribe(
                self.handle,
                opened,
                method,
                period_us,
                0,
                &mut out,
                MessageSink::NONE,
            )
        })?;
        Ok(out.id)
    }

    pub fn next_value(&self, into: &mut [u8], timeout: Duration) -> Result<Option<Value>, Status> {
        let mut out = surface::Value {
            struct_size: size_of::<surface::Value>() as u32,
            ..Default::default()
        };
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `out` is valid for its declared size and `into` for its
        // length during the call.
        let got = checked(unsafe {
            ffi::lemnos_next_value(
                self.handle,
                timeout_ms,
                &mut out,
                into.as_mut_ptr(),
                into.len(),
                MessageSink::NONE,
            )
        })?;
        Ok((got == 1).then_some(Value {
            subscription: out.subscription,
            stamp_ns: out.stamp_ns,
            len: out.len as usize,
        }))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = ffi::lemnos_client_close(self.handle);
    }
}

impl Stream<'_> {
    pub fn info(&self) -> Result<surface::StreamInfo, Status> {
        let mut info = surface::StreamInfo {
            struct_size: size_of::<surface::StreamInfo>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is valid for its declared size during the call.
        checked(unsafe { ffi::lemnos_stream_info(self.client.handle, self.handle, &mut info) })?;
        Ok(info)
    }

    /// The next frame, or `None` after `timeout` or once the host ended the
    /// stream. Allocates nothing. Its bytes borrow the stream, so every frame
    /// taken is dropped before [`release`](Self::release) or a write.
    pub fn next_frame(&self, timeout: Duration) -> Result<Option<StreamFrame<'_>>, Status> {
        let mut frame = surface::Frame {
            struct_size: size_of::<surface::Frame>() as u32,
            ..Default::default()
        };
        let mut data: *const u8 = core::ptr::null();
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `frame` is valid for its declared size and `data` for a
        // write during the call.
        let got = checked(unsafe {
            ffi::lemnos_stream_next_frame(
                self.client.handle,
                self.handle,
                timeout_ms,
                &mut frame,
                &mut data,
                MessageSink::NONE,
            )
        })?;
        if got != 1 {
            return Ok(None);
        }
        let bytes = if frame.len == 0 || data.is_null() {
            &[][..]
        } else {
            // SAFETY: the surface points `data` at the frame's `len` bytes in
            // the stream's mapping, which stays until the frame is released
            // or the stream closed. Both need `self` mutably or by value, so
            // neither happens while this borrow lives.
            unsafe { core::slice::from_raw_parts(data, frame.len as usize) }
        };
        Ok(Some(StreamFrame {
            buffer: frame.buffer,
            sequence: frame.sequence,
            identity: frame.identity,
            timestamp_ns: frame.timestamp_ns,
            bytes,
        }))
    }

    /// Gives a frame's buffer back, by its number.
    pub fn release(&mut self, buffer: u32) -> Result<(), Status> {
        checked(ffi::lemnos_stream_release(
            self.client.handle,
            self.handle,
            buffer,
            MessageSink::NONE,
        ))
        .map(drop)
    }

    pub fn read(&self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<(), Status> {
        // SAFETY: `out` is valid for its length during the call.
        checked(unsafe {
            ffi::lemnos_stream_read(
                self.client.handle,
                self.handle,
                buffer,
                offset,
                out.as_mut_ptr(),
                out.len(),
                MessageSink::NONE,
            )
        })
        .map(drop)
    }

    pub fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<(), Status> {
        // SAFETY: `data` is valid for its length during the call.
        checked(unsafe {
            ffi::lemnos_stream_write(
                self.client.handle,
                self.handle,
                buffer,
                offset,
                data.as_ptr(),
                data.len(),
                MessageSink::NONE,
            )
        })
        .map(drop)
    }

    /// `false` when too many jobs are waiting.
    pub fn post_job(&mut self, job: &[u8]) -> Result<bool, Status> {
        // SAFETY: `job` is valid for its length during the call.
        checked(unsafe {
            ffi::lemnos_stream_post_job(
                self.client.handle,
                self.handle,
                job.as_ptr(),
                job.len(),
                MessageSink::NONE,
            )
        })
        .map(|posted| posted == 1)
    }

    /// A descriptor of the buffer's own dma-buf.
    pub fn buffer_handle(&self, buffer: u32) -> Result<OwnedFd, Status> {
        let mut fd = -1;
        // SAFETY: `fd` is valid for a write.
        checked(unsafe {
            ffi::lemnos_stream_buffer_handle(
                self.client.handle,
                self.handle,
                buffer,
                &mut fd,
                MessageSink::NONE,
            )
        })?;
        // SAFETY: on success the surface gives a new descriptor that only the
        // caller owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Where plane `plane` of `buffer` is: its descriptor (a new one, owned by
    /// the caller), offset, sizes, and flags. See `lemnos_stream_region`.
    pub fn region(
        &self,
        buffer: u32,
        plane: u32,
    ) -> Result<(surface::StreamRegion, OwnedFd), Status> {
        let mut region = surface::StreamRegion {
            struct_size: size_of::<surface::StreamRegion>() as u32,
            fd: -1,
            ..Default::default()
        };
        // SAFETY: `region` is valid for its declared size, for the call.
        checked(unsafe {
            ffi::lemnos_stream_region(
                self.client.handle,
                self.handle,
                buffer,
                plane,
                &mut region,
                MessageSink::NONE,
            )
        })?;
        // SAFETY: on success the surface gives a new descriptor that only the caller owns.
        let fd = unsafe { OwnedFd::from_raw_fd(region.fd) };
        region.fd = -1;
        Ok((region, fd))
    }

    /// A new read-only descriptor of the pool page (see `lemnos_stream_pool_page`).
    pub fn pool_page(&self) -> Result<OwnedFd, Status> {
        let mut fd = -1;
        // SAFETY: `fd` is valid for a write.
        checked(unsafe {
            ffi::lemnos_stream_pool_page(
                self.client.handle,
                self.handle,
                &mut fd,
                MessageSink::NONE,
            )
        })?;
        // SAFETY: on success the surface gives a new descriptor that only the caller owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// The pool's generation (see `lemnos_stream_generation`).
    pub fn generation(&self) -> Result<u32, Status> {
        let mut generation = 0;
        // SAFETY: `generation` is valid for a write.
        checked(unsafe {
            ffi::lemnos_stream_generation(self.client.handle, self.handle, &mut generation)
        })?;
        Ok(generation)
    }

    /// The frames the reader was too late for (see `lemnos_stream_skipped`).
    pub fn skipped(&self) -> Result<u64, Status> {
        let mut skipped = 0;
        // SAFETY: `skipped` is valid for a write.
        checked(unsafe {
            ffi::lemnos_stream_skipped(self.client.handle, self.handle, &mut skipped)
        })?;
        Ok(skipped)
    }

    /// The part of the held frame in `buffer` that is in plane `plane`, on a
    /// stream whose buffers are in planes ([`info`](Self::info)'s `planes`).
    pub fn plane(&self, buffer: u32, plane: u32) -> Result<&[u8], Status> {
        let (mut data, mut len): (*const u8, usize) = (core::ptr::null(), 0);
        // SAFETY: `data` and `len` are valid for a write during the call.
        checked(unsafe {
            ffi::lemnos_stream_plane(
                self.client.handle,
                self.handle,
                buffer,
                plane,
                &mut data,
                &mut len,
            )
        })?;
        if len == 0 || data.is_null() {
            return Ok(&[]);
        }
        // SAFETY: the surface points `data` at `len` bytes of the held frame's
        // plane, mapped for as long as the stream is open; the frame stays
        // held while `self` is borrowed, as for `next_frame`.
        Ok(unsafe { core::slice::from_raw_parts(data, len) })
    }

    /// The length of plane `plane` of the held frame in `buffer`, without the
    /// bytes: works on a stream opened unmapped (see `lemnos_stream_plane_len`).
    pub fn plane_len(&self, buffer: u32, plane: u32) -> Result<usize, Status> {
        let mut len = 0;
        // SAFETY: `len` is valid for a write during the call.
        checked(unsafe {
            ffi::lemnos_stream_plane_len(self.client.handle, self.handle, buffer, plane, &mut len)
        })?;
        Ok(len)
    }

    /// A descriptor of the dma-buf behind plane `plane` of the buffer.
    pub fn plane_handle(&self, buffer: u32, plane: u32) -> Result<OwnedFd, Status> {
        let mut fd = -1;
        // SAFETY: `fd` is valid for a write.
        checked(unsafe {
            ffi::lemnos_stream_plane_handle(
                self.client.handle,
                self.handle,
                buffer,
                plane,
                &mut fd,
                MessageSink::NONE,
            )
        })?;
        // SAFETY: on success the surface gives a new descriptor that only the
        // caller owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Gives the stream back, saying whether the service took it.
    pub fn close(self) -> Result<(), Status> {
        let status = ffi::lemnos_stream_close(self.client.handle, self.handle, MessageSink::NONE);
        core::mem::forget(self);
        checked(status).map(drop)
    }
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        let _ = ffi::lemnos_stream_close(self.client.handle, self.handle, MessageSink::NONE);
    }
}
