//! `include/lemnos.h`, implemented. Frame streams are in [`streams`].

mod streams;

pub use streams::*;

use core::ffi::{CStr, c_char};
use core::mem::{align_of, offset_of, size_of};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_abi::{AbiVersion, Handle, MessageSink, Slab, Status, guard, write_text};
use lemnos_client::JobEvent as ClientJobEvent;
use lemnos_client::{
    self as client, DeviceDescription, DeviceEntry, DeviceHealth, DeviceState, Error, PackageList,
    PackageMode, Session,
};
use lemnos_core::ItemKind;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceInfo {
    pub struct_size: u32,
    pub state: u32,
    pub health: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Subscription {
    pub struct_size: u32,
    pub reserved: u32,
    pub id: u64,
    pub period_us: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Value {
    pub struct_size: u32,
    pub len: u32,
    pub subscription: u64,
    pub stamp_ns: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ItemInfo {
    pub struct_size: u32,
    pub kind: u32,
    pub contract: u32,
    pub method: u32,
    pub exponent: i32,
    pub has_range: u32,
    pub min: i64,
    pub max: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PackageInfo {
    pub struct_size: u32,
    pub loaded: u32,
}

/// What happened to a long job: `kind` is [`JOB_PROGRESS`] or [`JOB_DONE`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JobEvent {
    pub struct_size: u32,
    pub kind: u32,
    pub job: u64,
    /// Progress: `done` of `total` (0 when not known).
    pub done: u64,
    pub total: u64,
    /// Done: how it ended.
    pub status: Status,
    /// Done: the result's length.
    pub len: u32,
}

pub const JOB_PROGRESS: u32 = 0;
pub const JOB_DONE: u32 = 1;

/// Size, alignment and the offset of every field named, in the order named: the layout a type of
/// this surface has. [`LAYOUT`] lists every field of every type it covers; a test checks that
/// against the declarations, so a field can't be added without moving the hash.
macro_rules! shape {
    ($t:ty $(, $field:ident)*) => {
        [size_of::<$t>() as u64, align_of::<$t>() as u64 $(, offset_of!($t, $field) as u64)*]
    };
}

/// FNV-1a over every word of every part, in order: [`lemnos_abi::layout_hash`] over the parts
/// read as one list.
const fn fold(parts: &[&[u64]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut p = 0;
    while p < parts.len() {
        let mut i = 0;
        while i < parts[p].len() {
            let mut word = parts[p][i];
            let mut byte = 0;
            while byte < 8 {
                hash ^= word & 0xff;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                word >>= 8;
                byte += 1;
            }
            i += 1;
        }
        p += 1;
    }
    hash
}

/// The layout hash of this surface's types, with the abi's own.
pub const LAYOUT: u64 = fold(&[
    &[lemnos_abi::LAYOUT_HASH],
    &shape!(DeviceInfo, struct_size, state, health, reserved),
    &shape!(Subscription, struct_size, reserved, id, period_us),
    &shape!(Value, struct_size, len, subscription, stamp_ns),
    &shape!(
        ItemInfo,
        struct_size,
        kind,
        contract,
        method,
        exponent,
        has_range,
        min,
        max
    ),
    &shape!(PackageInfo, struct_size, loaded),
    &shape!(
        StreamInfo,
        struct_size,
        flags,
        buffers,
        buffer_bytes,
        frame_slots,
        job_slots,
        job_bytes,
        planes
    ),
    &shape!(
        Frame,
        struct_size,
        buffer,
        len,
        reserved,
        sequence,
        identity,
        timestamp_ns
    ),
    &shape!(JobEvent, struct_size, kind, job, done, total, status, len),
]);

// `LEMNOS_LAYOUT_HASH` in `include/lemnos.h` must equal [`LAYOUT`]: a test reads the header.

/// This surface's version: 1.3.
pub const VERSION: AbiVersion = AbiVersion::new(1, 3, LAYOUT);

struct Entry {
    session: Session,
    devices: Vec<DeviceEntry>,
    opened: Vec<client::Handle>,
    descriptions: HashMap<u32, DeviceDescription>,
    /// The list `lemnos_package_list` last fetched.
    packages: Option<PackageList>,
    /// The frame streams this client took.
    streams: Slab<StreamSlot>,
}

/// The lock is held only to find an entry. Each entry has its own, so waiting
/// for a value on one client doesn't block another.
static CLIENTS: Mutex<Slab<Arc<Mutex<Entry>>>> = Mutex::new(Slab::new());

fn clients() -> MutexGuard<'static, Slab<Arc<Mutex<Entry>>>> {
    CLIENTS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn entry(client: Handle) -> Result<Arc<Mutex<Entry>>, Status> {
    clients().get(client).map(Arc::clone)
}

fn with_entry(client: Handle, f: impl FnOnce(&mut Entry) -> Status) -> Status {
    let entry = match entry(client) {
        Ok(entry) => entry,
        Err(status) => return status,
    };
    let mut entry = entry.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut entry)
}

fn fail(error: Error, sink: MessageSink) -> Status {
    sink.write_fmt(format_args!("{error}"));
    Status::from(error)
}

/// A count or length, as a status.
fn count(n: usize) -> Status {
    i32::try_from(n).map_or(Status::INVALID_ARGUMENT, Status)
}

/// # Safety
/// `text` is NUL-terminated or null, and only read during this call.
unsafe fn text<'a>(text: *const c_char) -> Result<&'a str, Status> {
    if text.is_null() {
        return Err(Status::INVALID_ARGUMENT);
    }
    // SAFETY: not null, and NUL-terminated by the caller's contract.
    let text = unsafe { CStr::from_ptr(text) };
    text.to_str().map_err(|_| Status::INVALID_ARGUMENT)
}

/// # Safety
/// `out` is null, or valid for writes of `capacity` bytes during this call.
unsafe fn out_bytes<'a>(out: *mut u8, capacity: usize) -> Option<&'a mut [u8]> {
    if out.is_null() {
        return None;
    }
    // SAFETY: by the caller's contract, and no one else uses it for this call.
    Some(unsafe { core::slice::from_raw_parts_mut(out, capacity) })
}

/// # Safety
/// As for [`out_bytes`], and `len` is null or valid for a write.
unsafe fn give_text(value: &str, out: *mut c_char, capacity: usize, len: *mut usize) -> Status {
    // SAFETY: passed on from this function's contract.
    let target = unsafe { out_bytes(out.cast(), capacity) };
    let written = write_text(value, target);
    if !len.is_null() {
        // SAFETY: not null, and valid for a write by the contract.
        unsafe { len.write(written.needed) };
    }
    written.status
}

/// Checks the size the caller set, in the `struct_size` that every output
/// struct starts with, and returns the struct to fill.
///
/// # Safety
/// `out` is null, or valid for reads and writes of its declared `struct_size`.
unsafe fn checked<'a, T>(out: *mut T) -> Result<&'a mut T, Status> {
    if out.is_null() {
        return Err(Status::INVALID_ARGUMENT);
    }
    // SAFETY: not null, and `struct_size` is the first field by the contract.
    let size = unsafe { out.cast::<u32>().read() };
    if (size as usize) < size_of::<T>() {
        return Err(Status::INVALID_ARGUMENT);
    }
    // SAFETY: not null, and at least as large as `T` as the caller says.
    Ok(unsafe { &mut *out })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_version() -> AbiVersion {
    VERSION
}

/// # Safety
/// `caller` is null or points to a version struct.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_check_version(caller: *const AbiVersion) -> Status {
    guard(|| {
        if caller.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        // SAFETY: not null, and a version struct by the contract.
        let caller = unsafe { *caller };
        VERSION.accepts(&caller)
    })
}

/// # Safety
/// `socket` and `name` are NUL-terminated; `client` is null or valid for a
/// write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
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
        #[cfg(target_os = "linux")]
        if let Err(error) = crate::single::this_copy() {
            sink.write_fmt(format_args!("{error}"));
            return Status::BUSY;
        }
        // SAFETY: NUL-terminated by the contract, read only in this call.
        let socket = match unsafe { text(socket) } {
            Ok(socket) => socket,
            Err(status) => return status,
        };
        // SAFETY: as for `socket`.
        let name = match unsafe { text(name) } {
            Ok(name) => name,
            Err(status) => return status,
        };
        let mut session = match Session::connect(socket, name) {
            Ok(session) => session,
            Err(error) => return fail(error, sink),
        };
        if timeout_ms > 0 {
            session.set_timeout(Duration::from_millis(u64::from(timeout_ms)));
        }
        let devices = match session.list() {
            Ok(devices) => devices,
            Err(error) => return fail(error, sink),
        };
        let entry = Arc::new(Mutex::new(Entry {
            session,
            devices,
            opened: Vec::new(),
            descriptions: HashMap::new(),
            packages: None,
            streams: Slab::new(),
        }));
        match clients().insert(entry) {
            Ok(handle) => {
                // SAFETY: not null, and valid for a write by the contract.
                unsafe { client.write(handle) };
                Status::OK
            }
            Err(status) => status,
        }
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_client_close(client: Handle) -> Status {
    guard(|| match clients().remove(client) {
        Ok(_) => Status::OK,
        Err(status) => status,
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_device_count(client: Handle) -> Status {
    guard(|| {
        with_entry(client, |entry| match entry.session.list() {
            Ok(devices) => {
                entry.devices = devices;
                count(entry.devices.len())
            }
            Err(error) => Status::from(error),
        })
    })
}

/// # Safety
/// `info` is null or valid for its declared `struct_size`.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_device_info(
    client: Handle,
    device: u32,
    info: *mut DeviceInfo,
) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            // SAFETY: passed on from this function's contract.
            let info = match unsafe { checked(info) } {
                Ok(info) => info,
                Err(status) => return status,
            };
            let Some(found) = entry.devices.get(device as usize) else {
                return Status::NOT_FOUND;
            };
            info.state = match found.state {
                DeviceState::Running => 0,
                DeviceState::BackingOff => 1,
                DeviceState::Off => 2,
            };
            info.health = match found.health {
                DeviceHealth::Ok => 0,
                DeviceHealth::Degraded => 1,
                DeviceHealth::Failed => 2,
            };
            Status::OK
        })
    })
}

/// # Safety
/// `out` is null or valid for `capacity` bytes; `len` is null or valid for a
/// write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_device_text(
    client: Handle,
    device: u32,
    field: u32,
    out: *mut c_char,
    capacity: usize,
    len: *mut usize,
) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            let Some(found) = entry.devices.get(device as usize) else {
                return Status::NOT_FOUND;
            };
            let joined;
            let value = match field {
                0 => found.name.as_str(),
                1 => found.driver.as_str(),
                2 => {
                    joined = found.tags.join(",");
                    joined.as_str()
                }
                3 => found.reason.as_str(),
                _ => return Status::INVALID_ARGUMENT,
            };
            // SAFETY: passed on from this function's contract.
            unsafe { give_text(value, out, capacity, len) }
        })
    })
}

/// # Safety
/// `device` and `tag` are NUL-terminated; `opened` is null or valid for a
/// write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_open(
    client: Handle,
    device: *const c_char,
    tag: *const c_char,
    opened: *mut u32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if opened.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        // SAFETY: NUL-terminated by the contract, read only in this call.
        let device = match unsafe { text(device) } {
            Ok(device) => device,
            Err(status) => return status,
        };
        // SAFETY: as for `device`.
        let tag = match unsafe { text(tag) } {
            Ok(tag) => tag,
            Err(status) => return status,
        };
        with_entry(client, |entry| match entry.session.open(device, tag) {
            Ok(handle) => {
                let index = entry.opened.len() as u32;
                entry.opened.push(handle);
                // SAFETY: not null, and valid for a write by the contract.
                unsafe { opened.write(index) };
                Status::OK
            }
            Err(error) => fail(error, sink),
        })
    })
}

/// # Safety
/// `args` is valid for reads of `args_len` bytes (or null with 0); `reply` is
/// valid for writes of `reply_capacity` bytes (or null with 0).
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_call(
    client: Handle,
    opened: u32,
    method: u16,
    args: *const u8,
    args_len: usize,
    reply: *mut u8,
    reply_capacity: usize,
    sink: MessageSink,
) -> Status {
    guard(|| {
        let args: &[u8] = if args_len == 0 {
            &[]
        } else if args.is_null() {
            return Status::INVALID_ARGUMENT;
        } else {
            // SAFETY: not null, and valid for `args_len` bytes by the contract.
            unsafe { core::slice::from_raw_parts(args, args_len) }
        };
        // SAFETY: passed on from this function's contract.
        let reply: &mut [u8] = match unsafe { out_bytes(reply, reply_capacity) } {
            Some(reply) => reply,
            None if reply_capacity == 0 => &mut [],
            None => return Status::INVALID_ARGUMENT,
        };
        with_entry(client, |entry| {
            let Entry {
                session,
                opened: handles,
                ..
            } = entry;
            let Some(handle) = handles.get_mut(opened as usize) else {
                return Status::NOT_FOUND;
            };
            match session.call(handle, method, args, reply) {
                Ok(n) => count(n),
                Err(error) => fail(error, sink),
            }
        })
    })
}

/// # Safety
/// `args` is valid for `args_len` bytes (or `args_len` is 0); `job` is valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_job_start(
    client: Handle,
    opened: u32,
    method: u16,
    args: *const u8,
    args_len: usize,
    job: *mut u64,
    sink: MessageSink,
) -> Status {
    guard(|| {
        if job.is_null() {
            return Status::INVALID_ARGUMENT;
        }
        let args: &[u8] = if args_len == 0 {
            &[]
        } else if args.is_null() {
            return Status::INVALID_ARGUMENT;
        } else {
            // SAFETY: not null, and valid for `args_len` bytes by the contract.
            unsafe { core::slice::from_raw_parts(args, args_len) }
        };
        with_entry(client, |entry| {
            let Entry {
                session,
                opened: handles,
                ..
            } = entry;
            let Some(handle) = handles.get_mut(opened as usize) else {
                return Status::NOT_FOUND;
            };
            match session.start_job(handle, method, args) {
                Ok(number) => {
                    // SAFETY: not null, and valid for a write by the contract.
                    unsafe { job.write(number) };
                    Status::OK
                }
                Err(error) => fail(error, sink),
            }
        })
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_job_cancel(client: Handle, job: u64, sink: MessageSink) -> Status {
    guard(|| {
        with_entry(client, |entry| match entry.session.cancel_job(job) {
            Ok(()) => Status::OK,
            Err(error) => fail(error, sink),
        })
    })
}

/// # Safety
/// `event` is null or valid for its declared `struct_size`; `result` is null or valid for
/// `capacity` bytes.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_job_next(
    client: Handle,
    timeout_ms: u32,
    event: *mut JobEvent,
    result: *mut u8,
    capacity: usize,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let event = match unsafe { checked(event) } {
            Ok(event) => event,
            Err(status) => return status,
        };
        // SAFETY: passed on from this function's contract.
        let result: &mut [u8] = match unsafe { out_bytes(result, capacity) } {
            Some(result) => result,
            None if capacity == 0 => &mut [],
            None => return Status::INVALID_ARGUMENT,
        };
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        with_entry(client, |entry| {
            let Some(next) = entry.session.wait_job_event(timeout) else {
                return Status::OK;
            };
            match next {
                ClientJobEvent::Progress { job, done, total } => {
                    event.kind = JOB_PROGRESS;
                    event.job = job;
                    event.done = done;
                    event.total = total;
                    event.status = Status::OK;
                    event.len = 0;
                    Status(1)
                }
                ClientJobEvent::Done {
                    job,
                    status,
                    result: bytes,
                } => {
                    event.kind = JOB_DONE;
                    event.job = job;
                    event.done = 0;
                    event.total = 0;
                    event.status = status;
                    event.len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                    match result.get_mut(..bytes.len()) {
                        Some(into) => {
                            into.copy_from_slice(&bytes);
                            Status(1)
                        }
                        None => Status::BUFFER_TOO_SMALL,
                    }
                }
            }
        })
    })
}

/// # Safety
/// `subscription` is null or valid for its declared `struct_size`.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_subscribe(
    client: Handle,
    opened: u32,
    method: u16,
    period_us: u64,
    only_changes: u32,
    subscription: *mut Subscription,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let subscription = match unsafe { checked(subscription) } {
            Ok(subscription) => subscription,
            Err(status) => return status,
        };
        with_entry(client, |entry| {
            let Entry {
                session,
                opened: handles,
                ..
            } = entry;
            let Some(handle) = handles.get_mut(opened as usize) else {
                return Status::NOT_FOUND;
            };
            match session.subscribe(handle, method, period_us, only_changes != 0) {
                Ok(granted) => {
                    subscription.id = granted.id;
                    subscription.period_us = granted.period_us;
                    Status::OK
                }
                Err(error) => fail(error, sink),
            }
        })
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_unsubscribe(
    client: Handle,
    subscription: u64,
    sink: MessageSink,
) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            match entry.session.unsubscribe(subscription) {
                Ok(()) => Status::OK,
                Err(error) => fail(error, sink),
            }
        })
    })
}

/// # Safety
/// `value` is null or valid for its declared `struct_size`; `into` is valid
/// for writes of `capacity` bytes (or null with 0).
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_next_value(
    client: Handle,
    timeout_ms: u32,
    value: *mut Value,
    into: *mut u8,
    capacity: usize,
    sink: MessageSink,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let value = match unsafe { checked(value) } {
            Ok(value) => value,
            Err(status) => return status,
        };
        // SAFETY: passed on from this function's contract.
        let into: &mut [u8] = match unsafe { out_bytes(into, capacity) } {
            Some(into) => into,
            None if capacity == 0 => &mut [],
            None => return Status::INVALID_ARGUMENT,
        };
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        with_entry(client, |entry| {
            match entry.session.next_value(into, timeout) {
                Ok(Some(info)) => {
                    value.len = info.len as u32;
                    value.subscription = info.subscription;
                    value.stamp_ns = info.stamp_ns;
                    Status(1)
                }
                Ok(None) => Status::OK,
                Err(error) => fail(error, sink),
            }
        })
    })
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_set_power(
    client: Handle,
    device: u32,
    on: u32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            let Some(index) = entry.devices.get(device as usize).map(|d| d.index) else {
                return Status::NOT_FOUND;
            };
            match entry.session.set_power(index, on != 0) {
                Ok(()) => Status::OK,
                Err(error) => fail(error, sink),
            }
        })
    })
}

/// Loads a device's description once and hands it to `f`.
fn with_description(
    entry: &mut Entry,
    device: u32,
    sink: MessageSink,
    f: impl FnOnce(&DeviceDescription) -> Status,
) -> Status {
    let Some(index) = entry.devices.get(device as usize).map(|d| d.index) else {
        return Status::NOT_FOUND;
    };
    if !entry.descriptions.contains_key(&index) {
        match entry.session.describe(index) {
            Ok(description) => {
                entry.descriptions.insert(index, description);
            }
            Err(error) => return fail(error, sink),
        }
    }
    entry.descriptions.get(&index).map_or(Status::NOT_FOUND, f)
}

#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub extern "C" fn lemnos_item_count(client: Handle, device: u32, sink: MessageSink) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            with_description(entry, device, sink, |d| count(d.items.len()))
        })
    })
}

/// # Safety
/// `info` is null or valid for its declared `struct_size`.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_item_info(
    client: Handle,
    device: u32,
    item: u32,
    info: *mut ItemInfo,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let info = match unsafe { checked(info) } {
            Ok(info) => info,
            Err(status) => return status,
        };
        with_entry(client, |entry| {
            with_description(entry, device, MessageSink::NONE, |d| {
                let Some(found) = d.items.get(item as usize) else {
                    return Status::NOT_FOUND;
                };
                info.kind = match found.kind {
                    ItemKind::Value => 0,
                    ItemKind::Control => 1,
                    ItemKind::Action => 2,
                };
                info.contract = u32::from(found.contract);
                info.method = u32::from(found.method);
                info.exponent = i32::from(found.exponent);
                info.has_range = u32::from(found.min.is_some() && found.max.is_some());
                info.min = found.min.unwrap_or(0);
                info.max = found.max.unwrap_or(0);
                Status::OK
            })
        })
    })
}

/// # Safety
/// `out` is null or valid for `capacity` bytes; `len` is null or valid for a
/// write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_item_text(
    client: Handle,
    device: u32,
    item: u32,
    field: u32,
    out: *mut c_char,
    capacity: usize,
    len: *mut usize,
) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            with_description(entry, device, MessageSink::NONE, |d| {
                let Some(found) = d.items.get(item as usize) else {
                    return Status::NOT_FOUND;
                };
                let value = match field {
                    0 => found.name.as_str(),
                    1 => found.unit.as_str(),
                    2 => match d.contracts.get(usize::from(found.contract)) {
                        Some(contract) => contract.tag.as_str(),
                        None => return Status::NOT_FOUND,
                    },
                    _ => return Status::INVALID_ARGUMENT,
                };
                // SAFETY: passed on from this function's contract.
                unsafe { give_text(value, out, capacity, len) }
            })
        })
    })
}

/// # Safety
/// `mode` is null or valid for a write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_package_list(
    client: Handle,
    mode: *mut u32,
    sink: MessageSink,
) -> Status {
    guard(|| {
        with_entry(client, |entry| match entry.session.packages() {
            Ok(list) => {
                if !mode.is_null() {
                    let value = match list.mode {
                        PackageMode::Off => 0,
                        PackageMode::Signed => 1,
                        PackageMode::Open => 2,
                    };
                    // SAFETY: not null, and valid for a write by the contract.
                    unsafe { mode.write(value) };
                }
                let n = list.packages.len();
                entry.packages = Some(list);
                count(n)
            }
            Err(error) => fail(error, sink),
        })
    })
}

/// # Safety
/// `info` is null or valid for its declared `struct_size`.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_package_info(
    client: Handle,
    package: u32,
    info: *mut PackageInfo,
) -> Status {
    guard(|| {
        // SAFETY: passed on from this function's contract.
        let info = match unsafe { checked(info) } {
            Ok(info) => info,
            Err(status) => return status,
        };
        with_entry(client, |entry| {
            let Some(found) = entry
                .packages
                .as_ref()
                .and_then(|list| list.packages.get(package as usize))
            else {
                return Status::NOT_FOUND;
            };
            info.loaded = u32::from(found.driver.is_some());
            Status::OK
        })
    })
}

/// # Safety
/// `out` is null or valid for `capacity` bytes; `len` is null or valid for a
/// write.
#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]
pub unsafe extern "C" fn lemnos_package_text(
    client: Handle,
    package: u32,
    field: u32,
    out: *mut c_char,
    capacity: usize,
    len: *mut usize,
) -> Status {
    guard(|| {
        with_entry(client, |entry| {
            let Some(list) = entry.packages.as_ref() else {
                return Status::NOT_FOUND;
            };
            let value = if field == 3 {
                list.note.as_str()
            } else {
                let Some(found) = list.packages.get(package as usize) else {
                    return Status::NOT_FOUND;
                };
                match field {
                    0 => found.path.as_str(),
                    1 => found.driver.as_deref().unwrap_or(""),
                    2 => found.reason.as_str(),
                    _ => return Status::INVALID_ARGUMENT,
                }
            };
            // SAFETY: passed on from this function's contract.
            unsafe { give_text(value, out, capacity, len) }
        })
    })
}
