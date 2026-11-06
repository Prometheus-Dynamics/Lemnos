//! [`Client`]: the Rust wrapper over the C surface. It calls the exported
//! functions themselves, with the same pointers and structs a C caller passes,
//! so a Rust program linked against the surface (statically today) goes
//! through the C ABI rather than around it.

use core::ffi::c_void;
use std::ffi::CString;
use std::fmt;

use lemnos_abi::{Handle, MessageSink, Status};

use crate::ffi;
use crate::surface::{self, ChannelInfo, DeviceInfo, RawReading};

/// A call that failed: its status and the surface's words for why (empty
/// when it gave none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub status: Status,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "status {}", self.status.0)
        } else {
            write!(f, "{} (status {})", self.message, self.status.0)
        }
    }
}

impl std::error::Error for Error {}

impl From<Status> for Error {
    fn from(status: Status) -> Self {
        Self {
            status,
            message: String::new(),
        }
    }
}

/// The sink's callback: appends the message to the `String` behind `context`.
extern "C" fn append(context: *mut c_void, text: *const u8, len: usize) {
    if context.is_null() || text.is_null() {
        return;
    }
    // SAFETY: `context` is the `String` that `call` lends for the duration of
    // the call, and nothing else refers to it meanwhile.
    let out = unsafe { &mut *context.cast::<String>() };
    // SAFETY: the surface passes `len` bytes at `text`, valid for this call.
    let bytes = unsafe { core::slice::from_raw_parts(text, len) };
    out.push_str(&String::from_utf8_lossy(bytes));
}

/// Runs a surface call with a sink that collects its message.
fn call(f: impl FnOnce(MessageSink) -> Status) -> Result<u32, Error> {
    let mut message = String::new();
    let sink = MessageSink {
        context: (&raw mut message).cast(),
        write: Some(append),
    };
    let status = f(sink);
    status
        .into_result()
        .map_err(|status| Error { status, message })
}

/// Reads a string output of the surface, growing the buffer once if needed.
fn text(mut f: impl FnMut(&mut [u8], &mut usize) -> Status) -> Result<String, Error> {
    let mut buf = vec![0u8; 64];
    let mut len = 0usize;
    let mut status = f(&mut buf, &mut len);
    if status == Status::BUFFER_TOO_SMALL {
        buf.resize(len + 1, 0);
        status = f(&mut buf, &mut len);
    }
    status.into_result()?;
    buf.truncate(len);
    String::from_utf8(buf).map_err(|_| Error::from(Status::IO))
}

/// A device in the client's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Its index: what [`Client::read`] and [`Client::subscribe`] take.
    pub index: u32,
    pub id: String,
    pub info: DeviceInfo,
    pub channels: Vec<Channel>,
}

impl Device {
    /// The value of channel `name` in `reading`, in its unit.
    pub fn value(&self, reading: &Reading, name: &str) -> Option<f64> {
        let index = self.channels.iter().position(|c| c.name == name)?;
        reading.value(index, self.channels[index].info.exponent)
    }
}

/// One channel of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub info: ChannelInfo,
}

/// A reading, as the surface wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The device's index.
    pub device: u32,
    pub raw: RawReading,
}

impl Reading {
    /// Channel `index`'s value scaled by `10^exponent`; `None` when the
    /// channel was not read.
    pub fn value(&self, index: usize, exponent: i32) -> Option<f64> {
        let raw = *self.raw.values.get(index)?;
        (index < self.raw.channel_count as usize && raw != lemnos_ipc::NO_VALUE)
            .then(|| f64::from(raw) * 10f64.powi(exponent))
    }
}

/// A connection to `lemnosd` through the C surface. Closed on drop.
#[derive(Debug)]
pub struct Client {
    handle: Handle,
}

impl Client {
    /// Checks the surface's version, then connects to the socket at `socket`
    /// as `name`. `timeout_ms` bounds each request (0: the default).
    pub fn open(socket: &str, name: &str, timeout_ms: u32) -> Result<Self, Error> {
        // SAFETY: a pointer to a live `AbiVersion`, read during the call.
        let checked = unsafe { ffi::lemnos_check_version(&surface::VERSION) };
        checked.into_result()?;
        let socket = CString::new(socket).map_err(|_| Error::from(Status::INVALID_ARGUMENT))?;
        let name = CString::new(name).map_err(|_| Error::from(Status::INVALID_ARGUMENT))?;
        let mut handle = Handle::NULL;
        call(|sink| {
            // SAFETY: NUL-terminated strings and a handle to write, all alive
            // for the call.
            unsafe {
                ffi::lemnos_client_open(
                    socket.as_ptr(),
                    name.as_ptr(),
                    timeout_ms,
                    &mut handle,
                    sink,
                )
            }
        })?;
        Ok(Self { handle })
    }

    /// The handle the surface issued.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// The devices the service listed when the client connected.
    pub fn devices(&self) -> Result<Vec<Device>, Error> {
        let count = ffi::lemnos_device_count(self.handle).into_result()?;
        (0..count).map(|index| self.device(index)).collect()
    }

    fn device(&self, index: u32) -> Result<Device, Error> {
        let mut info = DeviceInfo {
            struct_size: size_of::<DeviceInfo>() as u32,
            ..DeviceInfo::default()
        };
        let id = text(|buf, len| {
            // SAFETY: `info`, `buf` (`buf.len()` bytes) and `len` are alive
            // and writable for the call.
            unsafe {
                ffi::lemnos_device_info(
                    self.handle,
                    index,
                    &mut info,
                    buf.as_mut_ptr(),
                    buf.len(),
                    len,
                )
            }
        })?;
        let channels = (0..info.channel_count)
            .map(|channel| {
                let mut info = ChannelInfo {
                    struct_size: size_of::<ChannelInfo>() as u32,
                    ..ChannelInfo::default()
                };
                let name = text(|buf, len| {
                    // SAFETY: as for the device's info above.
                    unsafe {
                        ffi::lemnos_channel_info(
                            self.handle,
                            index,
                            channel,
                            &mut info,
                            buf.as_mut_ptr(),
                            buf.len(),
                            len,
                        )
                    }
                })?;
                Ok(Channel { name, info })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Device {
            index,
            id,
            info,
            channels,
        })
    }

    /// Reads device `device` now.
    pub fn read(&self, device: u32) -> Result<Reading, Error> {
        let mut raw = RawReading::default();
        call(|sink| {
            // SAFETY: `raw` is alive and writable for the call, its
            // `struct_size` set.
            unsafe { ffi::lemnos_read(self.handle, device, &mut raw, sink) }
        })?;
        Ok(Reading { device, raw })
    }

    /// Streams device `device`'s readings at most every `period_ms` (0
    /// ends it). Returns the granted period.
    pub fn subscribe(&self, device: u32, period_ms: u32) -> Result<u32, Error> {
        let mut granted = 0u32;
        call(|sink| {
            // SAFETY: `granted` is alive and writable for the call.
            unsafe { ffi::lemnos_subscribe(self.handle, device, period_ms, &mut granted, sink) }
        })?;
        Ok(granted)
    }

    /// The next subscribed reading within `timeout_ms`; `None` when none came
    /// (or something other than a reading came first).
    pub fn next_reading(&self, timeout_ms: u32) -> Result<Option<Reading>, Error> {
        let mut device = 0u32;
        let mut raw = RawReading::default();
        let result = call(|sink| {
            // SAFETY: `device` and `raw` are alive and writable for the call.
            unsafe {
                ffi::lemnos_next_reading(self.handle, timeout_ms, &mut device, &mut raw, sink)
            }
        });
        match result {
            Ok(_) => Ok(Some(Reading { device, raw })),
            Err(e) if e.status == Status::TIMED_OUT || e.status == Status::AGAIN => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = ffi::lemnos_client_close(self.handle);
    }
}
