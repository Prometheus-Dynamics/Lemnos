//! The exported C functions (`include/lemnos.h`). Each one runs inside
//! [`lemnos_abi::guard`], returns a [`Status`], and writes its results through
//! the caller's pointers; on an error it writes the reason to the
//! [`MessageSink`] it was given, if any.
//!
//! Devices are named by their index in the list the client received when it
//! connected (`lemnos_device_count`), so a read passes no strings.
//!
//! Output structs start with `struct_size`: the caller sets it to the size it
//! was built with, and a call refuses a struct smaller than its own
//! (`INVALID_ARGUMENT`), so a newer surface never writes past an older
//! caller's struct.

use core::ffi::{CStr, c_char};
use core::mem::{align_of, offset_of, size_of};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_abi::{AbiVersion, Handle, MessageSink, Slab, Status, guard, layout_hash};
use lemnos_ipc::wire::Code;
use lemnos_ipc::{
    ClientError, ClientEvent, ClientOptions, DeviceClient, DeviceDesc, ErrorKind, Reading, Refusal,
    Update,
};

/// Channels a reading carries at most (`LEMNOS_MAX_CHANNELS`).
pub const MAX_CHANNELS: usize = 16;

/// A device in the client's list (`LemnosDeviceInfo`). The numbers are the
/// IPC wire's (`lemnos_ipc::wire::Code`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    pub struct_size: u32,
    /// `LEMNOS_CLASS_*`.
    pub class: u32,
    /// `LEMNOS_STATUS_*`.
    pub status: u32,
    pub channel_count: u32,
    pub control_count: u32,
    /// LEDs, for lights; 0 otherwise.
    pub pixels: u32,
}

/// One channel of a device (`LemnosChannelInfo`): a raw value `v` is
/// `v * 10^exponent` in the quantity's unit.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelInfo {
    pub struct_size: u32,
    /// `LEMNOS_QUANTITY_*`.
    pub quantity: u32,
    /// `LEMNOS_AXIS_*`.
    pub axis: u32,
    pub exponent: i32,
}

/// A reading (`LemnosReading`): one fixed-point value per channel, in the
/// order of the device's channels, `LEMNOS_NO_VALUE` for a channel not read.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawReading {
    pub struct_size: u32,
    /// `LEMNOS_STATUS_*`.
    pub status: u32,
    /// Microseconds on `CLOCK_BOOTTIME` when the read started.
    pub timestamp_us: u64,
    pub channel_count: u32,
    pub reserved: u32,
    pub values: [i32; MAX_CHANNELS],
}

impl Default for RawReading {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            status: 0,
            timestamp_us: 0,
            channel_count: 0,
            reserved: 0,
            values: [lemnos_ipc::NO_VALUE; MAX_CHANNELS],
        }
    }
}

const LAYOUT: u64 = layout_hash(&[
    lemnos_abi::LAYOUT_HASH,
    MAX_CHANNELS as u64,
    size_of::<DeviceInfo>() as u64,
    align_of::<DeviceInfo>() as u64,
    offset_of!(DeviceInfo, pixels) as u64,
    size_of::<ChannelInfo>() as u64,
    align_of::<ChannelInfo>() as u64,
    offset_of!(ChannelInfo, exponent) as u64,
    size_of::<RawReading>() as u64,
    align_of::<RawReading>() as u64,
    offset_of!(RawReading, timestamp_us) as u64,
    offset_of!(RawReading, values) as u64,
]);

/// `LEMNOS_LAYOUT_HASH` in `include/lemnos.h`. The layouts it hashes hold no
/// pointers, so it is the same on every target; this fails to compile on one
/// where it is not.
const HEADER_LAYOUT_HASH: u64 = 0xfd63_0e78_4f30_59fe;
const _: () = assert!(
    LAYOUT == HEADER_LAYOUT_HASH,
    "update LEMNOS_LAYOUT_HASH in lemnos.h"
);

/// This surface's version (`lemnos_version`): 1.0.
pub const VERSION: AbiVersion = AbiVersion::new(1, 0, LAYOUT);

struct Entry {
    client: DeviceClient,
    /// The list the client received when it connected: devices are named by
    /// their index in it.
    devices: Vec<DeviceDesc>,
}

/// The open clients. The lock is held only to find an entry; each entry has
/// its own, so a call waiting for a reading on one client does not block
/// another client.
static CLIENTS: Mutex<Slab<Arc<Mutex<Entry>>>> = Mutex::new(Slab::new());

fn clients() -> MutexGuard<'static, Slab<Arc<Mutex<Entry>>>> {
    CLIENTS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with_entry(client: Handle, f: impl FnOnce(&mut Entry) -> Status) -> Status {
    let entry = match clients().get(client) {
        Ok(entry) => Arc::clone(entry),
        Err(status) => return status,
    };
    let mut entry = entry.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut entry)
}

fn kind_status(kind: ErrorKind) -> Status {
    match kind {
        ErrorKind::NotFound => Status::NOT_FOUND,
        ErrorKind::Unavailable => Status::NO_DEVICE,
        ErrorKind::Busy => Status::BUSY,
        ErrorKind::PermissionDenied => Status::ACCESS,
        ErrorKind::Timeout => Status::TIMED_OUT,
        ErrorKind::InvalidInput | ErrorKind::Configuration => Status::INVALID_ARGUMENT,
        ErrorKind::Unsupported => Status::NOT_SUPPORTED,
        // ENXIO: what i2c-dev returns for an address nobody acknowledges.
        ErrorKind::Nack => Status::from_errno(6),
        // EOVERFLOW.
        ErrorKind::Overrun => Status::from_errno(75),
        _ => Status::IO,
    }
}

fn error_status(error: &ClientError) -> Status {
    match error {
        ClientError::Io(e) => Status::from_io_error(e),
        ClientError::Protocol(_) => Status::IO,
        ClientError::Refused(refusal) => match refusal {
            Refusal::NotAllowed => Status::NOT_PERMITTED,
            // ERANGE.
            Refusal::OutOfRange => Status::from_errno(34),
            Refusal::UnknownDevice
            | Refusal::UnknownControl
            | Refusal::UnknownChannel
            | Refusal::UnknownHandle => Status::NOT_FOUND,
            Refusal::Device(kind) => kind_status(*kind),
            Refusal::Owned | Refusal::Claimed => Status::BUSY,
            Refusal::Unsupported => Status::NOT_SUPPORTED,
        },
        ClientError::Timeout => Status::TIMED_OUT,
        // ENOTCONN.
        ClientError::Closed => Status::from_errno(107),
        ClientError::Rejected(_) => Status::INVALID_ARGUMENT,
    }
}

fn fail(error: &ClientError, sink: MessageSink) -> Status {
    sink.write_fmt(format_args!("{error}"));
    error_status(error)
}

/// # Safety
/// `text` is null or a NUL-terminated string that stays valid for `'a`.
unsafe fn text<'a>(text: *const c_char) -> Result<&'a str, Status> {
    if text.is_null() {
        return Err(Status::INVALID_ARGUMENT);
    }
    // SAFETY: not null, and NUL-terminated and valid by the caller's contract.
    let text = unsafe { CStr::from_ptr(text) };
    text.to_str().map_err(|_| Status::INVALID_ARGUMENT)
}

/// Writes `value` to `out`.
///
/// # Safety
/// `out` is null or valid for a write of a `T`.
unsafe fn put<T>(out: *mut T, value: T) -> Status {
    if out.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: not null, and valid for the write by the caller's contract.
    unsafe { out.write(value) };
    Status::OK
}

/// Checks the `struct_size` the caller set in the struct at `out` (its first
/// field) against this surface's `T`.
///
/// # Safety
/// `out` is null or valid for a read of a `u32` at its start.
unsafe fn sized<T>(out: *mut T) -> Status {
    if out.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: not null, and its first field is the caller's `struct_size`.
    let caller = unsafe { out.cast::<u32>().read() };
    if (caller as usize) < size_of::<T>() {
        Status::INVALID_ARGUMENT
    } else {
        Status::OK
    }
}

/// Copies `value` into `buf` with a terminating NUL and writes its length
/// (without the NUL) to `len`. `BUFFER_TOO_SMALL` (nothing copied, the length
/// still written) when it does not fit in `capacity` bytes.
///
/// # Safety
/// `buf` is null or valid for writes of `capacity` bytes; `len` is null or
/// valid for a write of a `usize`.
unsafe fn put_text(value: &str, buf: *mut u8, capacity: usize, len: *mut usize) -> Status {
    let out = if buf.is_null() {
        None
    } else {
        // SAFETY: not null, and valid for `capacity` bytes by the caller's contract.
        Some(unsafe { core::slice::from_raw_parts_mut(buf, capacity) })
    };
    let written = lemnos_abi::write_text(value, out);
    if !len.is_null() {
        // SAFETY: not null, and valid for the write by the caller's contract.
        unsafe { len.write(written.needed) };
    }
    written.status
}

fn raw_reading(reading: &Reading) -> RawReading {
    let mut raw = RawReading {
        status: u32::from(reading.status.code()),
        timestamp_us: reading.timestamp_us,
        channel_count: reading.raw.len().min(MAX_CHANNELS) as u32,
        ..RawReading::default()
    };
    for (out, value) in raw.values.iter_mut().zip(&reading.raw) {
        *out = *value;
    }
    raw
}

fn device_id(entry: &Entry, device: u32) -> Result<String, Status> {
    entry
        .devices
        .get(device as usize)
        .map(|d| d.id.clone())
        .ok_or(Status::NOT_FOUND)
}

/// The surface's version, for the handshake (`lemnos_check_version`).
#[unsafe(no_mangle)]
pub extern "C" fn lemnos_version() -> AbiVersion {
    VERSION
}

/// `OK` when this surface serves a caller built against `caller`
/// (`AbiVersion::accepts`), else `VERSION_MISMATCH`.
///
/// # Safety
/// `caller` is null or points to a readable `LemnosAbiVersion`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_check_version(caller: *const AbiVersion) -> Status {
    guard(|| {
        if caller.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        // SAFETY: not null, and readable by the caller's contract.
        let caller = unsafe { caller.read() };
        VERSION.accepts(&caller)
    })
}

/// Connects to the `lemnosd` socket at `socket` as `name` and writes the
/// client's handle to `client`. `timeout_ms` bounds each request (0: the
/// client's default).
///
/// # Safety
/// `socket` and `name` are NUL-terminated strings; `client` is valid for a
/// write of a handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_client_open(
    socket: *const c_char,
    name: *const c_char,
    timeout_ms: u32,
    client: *mut Handle,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if client.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        // SAFETY: NUL-terminated by the caller's contract, and only read in
        // this call.
        let socket = match unsafe { text(socket) } {
            Ok(socket) => socket,
            Err(status) => return status,
        };
        // SAFETY: as for `socket`.
        let name = match unsafe { text(name) } {
            Ok(name) => name,
            Err(status) => return status,
        };
        let mut options = ClientOptions::new(socket, name);
        if timeout_ms > 0 {
            options = options.timeout(Duration::from_millis(u64::from(timeout_ms)));
        }
        let device_client = match options.devices() {
            Ok(device_client) => device_client,
            Err(error) => return fail(&error, sink),
        };
        let devices = device_client.devices().cloned().collect();
        let entry = Arc::new(Mutex::new(Entry {
            client: device_client,
            devices,
        }));
        let handle = match clients().insert(entry) {
            Ok(handle) => handle,
            Err(status) => return status,
        };
        // SAFETY: not null (checked above), and valid by the caller's contract.
        unsafe { put(client, handle) }
    })
}

/// Disconnects `client`; its handle is stale from now on.
#[unsafe(no_mangle)]
pub extern "C" fn lemnos_client_close(client: Handle) -> Status {
    guard(|| match clients().remove(client) {
        Ok(_) => Status::OK,
        Err(status) => status,
    })
}

/// The number of devices in `client`'s list (as a positive status).
#[unsafe(no_mangle)]
pub extern "C" fn lemnos_device_count(client: Handle) -> Status {
    guard(|| {
        with_entry(client, |e| {
            Status(e.devices.len().min(i32::MAX as usize) as i32)
        })
    })
}

/// Device `device` of `client`'s list: its info in `info`, and its id in
/// `id` (`id_capacity` bytes, NUL-terminated; its length without the NUL in
/// `id_len`). `BUFFER_TOO_SMALL` when the id does not fit; `info` is written
/// either way.
///
/// # Safety
/// `info` is valid for a `LemnosDeviceInfo` whose `struct_size` the caller
/// set; `id` is null or valid for `id_capacity` bytes; `id_len` is null or
/// valid for a `size_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_device_info(
    client: Handle,
    device: u32,
    info: *mut DeviceInfo,
    id: *mut u8,
    id_capacity: usize,
    id_len: *mut usize,
) -> Status {
    guard(|| {
        // SAFETY: valid for its first field by the caller's contract.
        let sized = unsafe { sized(info) };
        if !sized.is_ok() {
            return sized;
        }
        with_entry(client, |e| {
            let Some(desc) = e.devices.get(device as usize) else {
                return Status::NOT_FOUND;
            };
            let value = DeviceInfo {
                struct_size: size_of::<DeviceInfo>() as u32,
                class: u32::from(desc.class.code()),
                status: u32::from(desc.status.code()),
                channel_count: desc.channels.len() as u32,
                control_count: desc.controls.len() as u32,
                pixels: u32::from(desc.pixels),
            };
            // SAFETY: checked above, and valid by the caller's contract.
            let written = unsafe { put(info, value) };
            if !written.is_ok() {
                return written;
            }
            // SAFETY: the buffer and length pointers' contract above.
            unsafe { put_text(&desc.id, id, id_capacity, id_len) }
        })
    })
}

/// Channel `channel` of device `device`: its info in `info`, its name in
/// `name` as for `lemnos_device_info`'s id.
///
/// # Safety
/// As for `lemnos_device_info`, with a `LemnosChannelInfo`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_channel_info(
    client: Handle,
    device: u32,
    channel: u32,
    info: *mut ChannelInfo,
    name: *mut u8,
    name_capacity: usize,
    name_len: *mut usize,
) -> Status {
    guard(|| {
        // SAFETY: valid for its first field by the caller's contract.
        let sized = unsafe { sized(info) };
        if !sized.is_ok() {
            return sized;
        }
        with_entry(client, |e| {
            let Some(desc) = e
                .devices
                .get(device as usize)
                .and_then(|d| d.channels.get(channel as usize))
            else {
                return Status::NOT_FOUND;
            };
            let value = ChannelInfo {
                struct_size: size_of::<ChannelInfo>() as u32,
                quantity: u32::from(desc.quantity.code()),
                axis: u32::from(desc.axis.code()),
                exponent: i32::from(desc.exponent),
            };
            // SAFETY: checked above, and valid by the caller's contract.
            let written = unsafe { put(info, value) };
            if !written.is_ok() {
                return written;
            }
            // SAFETY: the buffer and length pointers' contract above.
            unsafe { put_text(&desc.name, name, name_capacity, name_len) }
        })
    })
}

/// Reads device `device` now (a one-shot read) into `reading`.
///
/// # Safety
/// `reading` is valid for a `LemnosReading` whose `struct_size` the caller
/// set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_read(
    client: Handle,
    device: u32,
    reading: *mut RawReading,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: valid for its first field by the caller's contract.
        let sized = unsafe { sized(reading) };
        if !sized.is_ok() {
            return sized;
        }
        with_entry(client, |e| {
            let id = match device_id(e, device) {
                Ok(id) => id,
                Err(status) => return status,
            };
            match e.client.read(&id) {
                // SAFETY: checked above, and valid by the caller's contract.
                Ok(r) => unsafe { put(reading, raw_reading(&r)) },
                Err(error) => fail(&error, sink),
            }
        })
    })
}

/// Streams device `device`'s readings at most every `period_ms` (0 ends the
/// subscription); the period the service granted goes to `granted_ms`.
///
/// # Safety
/// `granted_ms` is null or valid for a write of a `uint32_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_subscribe(
    client: Handle,
    device: u32,
    period_ms: u32,
    granted_ms: *mut u32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        with_entry(client, |e| {
            let id = match device_id(e, device) {
                Ok(id) => id,
                Err(status) => return status,
            };
            match e.client.subscribe(&id, period_ms) {
                Ok(granted) if granted_ms.is_null() => {
                    let _ = granted;
                    Status::OK
                }
                // SAFETY: not null, and valid by the caller's contract.
                Ok(granted) => unsafe { put(granted_ms, granted) },
                Err(error) => fail(&error, sink),
            }
        })
    })
}

/// Waits up to `timeout_ms` for the next subscribed reading: `OK` with the
/// device's index in `device` and the reading in `reading`; `TIMED_OUT` when
/// none came; `AGAIN` when something else came first (an event, a
/// reconnection), so the caller simply calls again.
///
/// # Safety
/// `device` is valid for a write of a `uint32_t`; `reading` as for
/// `lemnos_read`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_next_reading(
    client: Handle,
    timeout_ms: u32,
    device: *mut u32,
    reading: *mut RawReading,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: valid for its first field by the caller's contract.
        let sized = unsafe { sized(reading) };
        if !sized.is_ok() {
            return sized;
        }
        if device.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        with_entry(client, |e| {
            let wait = Duration::from_millis(u64::from(timeout_ms));
            match e.client.next_event_timeout(wait) {
                Ok(Some(ClientEvent::Data(Update::Reading(r)))) => {
                    let Some(index) = e.devices.iter().position(|d| d.id == r.device) else {
                        return Status::AGAIN;
                    };
                    // SAFETY: not null (checked above), and valid by the
                    // caller's contract.
                    let written = unsafe { put(device, index as u32) };
                    if !written.is_ok() {
                        return written;
                    }
                    // SAFETY: checked above, and valid by the caller's contract.
                    unsafe { put(reading, raw_reading(&r)) }
                }
                Ok(Some(_)) => Status::AGAIN,
                Ok(None) => Status::TIMED_OUT,
                Err(error) => fail(&error, sink),
            }
        })
    })
}
