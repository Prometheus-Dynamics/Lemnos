//! A driver as a package: a compiled file the service loads, built from the same `#[driver]`
//! type a built-in driver is.
//!
//! The package's side is one line, [`export_package!`](crate::export_package). It serves the
//! driver through the C boundary in [`lemnos_abi::package`]: the manifest in C form, the
//! driver's calls behind a function table, and what the driver needs fetched from the host as
//! handles. The host's side is the other half of each conversion here, used by the loader.
//!
//! What a package's driver can be given: other devices, and I2C buses. Any other kind of bus is
//! `NOT_SUPPORTED` when it binds, as it is on a host that lacks one.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use std::sync::{Mutex, OnceLock, PoisonError};

use lemnos_abi::package::{
    DRIVER_TICKS, DRIVER_TRACKS_CALLERS, HEALTH_DEGRADED, HEALTH_FAILED, HEALTH_GONE, HEALTH_OK,
    HostTable, ITEM_HAS_FIELD, ITEM_HAS_MAX, ITEM_HAS_MIN, MAX_TRANSFER_STEPS, NEED_DEVICE, Out,
    PackageContract, PackageDriver, PackageField, PackageHealth, PackageInstance, PackageItem,
    PackageManifest, PackageMethod, PackageNeed, PackageSetting, PackageShape, PackageTransfer,
    SETTING_BOOL, SETTING_FLOAT, SETTING_INT, SETTING_TEXT, Slice, SliceMut, Str, driver_table,
};
use lemnos_abi::{Handle, guard};
use lemnos_hal::i2c::{self, I2c as HalI2c, Operation};

use crate::{
    BusKind, CallerId, Config, ConfigValue, ContractInfo, Description, Driver, ErrorKind, Field,
    Health, I2c, Item, ItemKind, LocalCaller, Manifest, MethodId, MethodInfo, Need, NeedKind,
    Param, Provider, Reply, Resources, Result, Router, Shape, Status, Tag, Unit,
};

/// The boundary's own types, for [`export_package!`](crate::export_package) and the loader.
pub use lemnos_abi::package as abi;

const BUSES: [BusKind; 13] = [
    BusKind::I2c,
    BusKind::Spi,
    BusKind::Gpio,
    BusKind::Pwm,
    BusKind::Uart,
    BusKind::Usb,
    BusKind::UsbFunction,
    BusKind::Registers,
    BusKind::Interrupt,
    BusKind::Dma,
    BusKind::Clocks,
    BusKind::Reset,
    BusKind::Stream,
];

const UNITS: [Unit; 13] = [
    Unit::One,
    Unit::MetrePerSecondSquared,
    Unit::RadianPerSecond,
    Unit::Tesla,
    Unit::Volt,
    Unit::Ampere,
    Unit::Watt,
    Unit::DegreeCelsius,
    Unit::RevolutionPerMinute,
    Unit::Radian,
    Unit::Pascal,
    Unit::Hertz,
    Unit::Second,
];

const ITEM_KINDS: [ItemKind; 3] = [ItemKind::Value, ItemKind::Control, ItemKind::Action];

/// The deepest a shape from a package may nest.
const MAX_SHAPE_DEPTH: u32 = 16;

/// A kind of bus as [`PackageNeed::kind`] carries it: 1 and up, in the order of [`BusKind`].
pub fn bus_code(bus: BusKind) -> u32 {
    BUSES.iter().position(|kind| *kind == bus).unwrap_or(0) as u32 + 1
}

/// The kind of bus `code` names, if this build knows it.
pub fn bus_of(code: u32) -> Option<BusKind> {
    BUSES.get((code as usize).checked_sub(1)?).copied()
}

fn code_in<T: PartialEq>(table: &[T], value: &T) -> u8 {
    table.iter().position(|entry| entry == value).unwrap_or(0) as u8
}

fn kept<T>(values: Vec<T>) -> Slice<'static, T> {
    Slice::new(Vec::leak(values))
}

/// The status a bus error crosses the boundary as. Unlike [`status_of`](crate::status_of), a
/// missing acknowledge and an overrun keep their own codes, so the driver sees them as they were.
pub fn status_of_bus(kind: ErrorKind) -> Status {
    match kind {
        ErrorKind::Nack => Status::from_errno(121),
        ErrorKind::Overrun => Status::from_errno(75),
        kind => crate::status_of(kind),
    }
}

/// The bus error a status from the host stands for.
pub fn bus_error_of(status: Status) -> ErrorKind {
    match status {
        Status::NOT_FOUND => ErrorKind::NotFound,
        Status::NO_DEVICE => ErrorKind::Unavailable,
        Status::BUSY => ErrorKind::Busy,
        Status::ACCESS => ErrorKind::PermissionDenied,
        Status::TIMED_OUT => ErrorKind::Timeout,
        Status::INVALID_ARGUMENT => ErrorKind::InvalidInput,
        Status::NOT_SUPPORTED => ErrorKind::Unsupported,
        status => status
            .errno()
            .map_or(ErrorKind::Failed, ErrorKind::from_errno),
    }
}

// ---- The driver model in C form, as the package sends it ----

fn shape_out(shape: &Shape) -> PackageShape {
    let plain = |kind| PackageShape {
        kind,
        count: 0,
        of: Slice::EMPTY,
        fields: Slice::EMPTY,
    };
    match shape {
        Shape::Unit => plain(0),
        Shape::Bool => plain(1),
        Shape::U8 => plain(2),
        Shape::U16 => plain(3),
        Shape::U32 => plain(4),
        Shape::U64 => plain(5),
        Shape::I8 => plain(6),
        Shape::I16 => plain(7),
        Shape::I32 => plain(8),
        Shape::I64 => plain(9),
        Shape::F32 => plain(10),
        Shape::F64 => plain(11),
        Shape::Str => plain(12),
        Shape::Bytes => plain(13),
        Shape::Option(of) => PackageShape {
            of: kept(Vec::from([shape_out(of)])),
            ..plain(14)
        },
        Shape::Array(of, count) => PackageShape {
            count: *count,
            of: kept(Vec::from([shape_out(of)])),
            ..plain(15)
        },
        Shape::Record(fields) => PackageShape {
            fields: kept(fields.iter().map(field_out).collect()),
            ..plain(16)
        },
    }
}

fn field_out(field: &Field) -> PackageField {
    PackageField {
        name: Str::new(field.name),
        shape: shape_out(&field.shape),
    }
}

fn method_out(method: &MethodInfo) -> PackageMethod {
    let params = method.params.iter().map(|param| PackageField {
        name: Str::new(param.name),
        shape: shape_out(&param.shape),
    });
    PackageMethod {
        id: method.id,
        has_out_buffer: u16::from(method.has_out_buffer),
        reserved: 0,
        name: Str::new(method.name),
        params: kept(params.collect()),
        returns: shape_out(&method.returns),
    }
}

/// A manifest in C form. The tables it points to are kept for good: call it once per driver.
pub fn manifest_out(manifest: &Manifest) -> PackageManifest {
    let needs = manifest.needs.iter().map(|need| {
        let (kind, tag) = match need.kind {
            NeedKind::Bus(bus) => (bus_code(bus), ""),
            NeedKind::Device(tag) => (NEED_DEVICE, tag.as_str()),
        };
        PackageNeed {
            name: Str::new(need.name),
            kind,
            optional: u32::from(need.optional),
            tag: Str::new(tag),
        }
    });
    let provides = manifest.provides.iter().map(|contract| PackageContract {
        tag: Str::new(contract.tag.as_str()),
        methods: kept(contract.methods.iter().map(method_out).collect()),
    });
    PackageManifest {
        name: Str::new(manifest.name),
        needs: kept(needs.collect()),
        provides: kept(provides.collect()),
    }
}

/// A device's health in C form.
pub fn health_out(health: Health) -> PackageHealth {
    match health {
        Health::Ok => PackageHealth::OK,
        Health::Degraded(why) => PackageHealth::new(HEALTH_DEGRADED, why),
        Health::Failed(why) => PackageHealth::new(HEALTH_FAILED, why),
        Health::Gone(why) => PackageHealth::new(HEALTH_GONE, why),
    }
}

fn item_out(item: &Item) -> PackageItem {
    let mut flags = 0;
    if item.min.is_some() {
        flags |= ITEM_HAS_MIN;
    }
    if item.max.is_some() {
        flags |= ITEM_HAS_MAX;
    }
    if item.field.is_some() {
        flags |= ITEM_HAS_FIELD;
    }
    PackageItem {
        kind: code_in(&ITEM_KINDS, &item.kind),
        unit: code_in(&UNITS, &item.unit),
        exponent: item.exponent,
        flags,
        contract: item.method.contract,
        reserved: 0,
        method: item.method.method,
        min: item.min.unwrap_or(0),
        max: item.max.unwrap_or(0),
        name: Str::new(item.name),
        field: Str::new(item.field.unwrap_or("")),
    }
}

// ---- The same, as the host reads it ----

fn shape_in(shape: &PackageShape, depth: u32) -> core::result::Result<Shape, &'static str> {
    if depth == 0 {
        return Err("a shape nests too deeply");
    }
    let inner = || match shape.of.as_slice() {
        [of] => shape_in(of, depth - 1).map(|of| &*Box::leak(Box::new(of))),
        _ => Err("an option or array has no inner shape"),
    };
    Ok(match shape.kind {
        0 => Shape::Unit,
        1 => Shape::Bool,
        2 => Shape::U8,
        3 => Shape::U16,
        4 => Shape::U32,
        5 => Shape::U64,
        6 => Shape::I8,
        7 => Shape::I16,
        8 => Shape::I32,
        9 => Shape::I64,
        10 => Shape::F32,
        11 => Shape::F64,
        12 => Shape::Str,
        13 => Shape::Bytes,
        14 => Shape::Option(inner()?),
        15 => Shape::Array(inner()?, shape.count),
        16 => {
            let fields: core::result::Result<Vec<Field>, _> = shape
                .fields
                .as_slice()
                .iter()
                .map(|field| {
                    Ok(Field {
                        name: field.name.as_str(),
                        shape: shape_in(&field.shape, depth - 1)?,
                    })
                })
                .collect();
            Shape::Record(Vec::leak(fields?))
        }
        _ => return Err("a shape of a kind this host does not know"),
    })
}

fn tag_in(tag: &'static str) -> core::result::Result<Tag, &'static str> {
    let well_formed = tag.split_once('/').is_some_and(|(name, major)| {
        !name.is_empty() && !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit())
    });
    if well_formed {
        Ok(Tag::new(tag))
    } else {
        Err("a contract tag is not name/major")
    }
}

/// The manifest a package registered, checked, as the host's own type. What it points to is
/// kept for good: call it once per package.
pub fn manifest_in(manifest: &PackageManifest) -> core::result::Result<Manifest, &'static str> {
    let name = manifest.name.as_str();
    if name.is_empty() {
        return Err("the driver has no name");
    }
    let mut needs = Vec::new();
    for need in manifest.needs.as_slice() {
        let kind = if need.kind == NEED_DEVICE {
            NeedKind::Device(tag_in(need.tag.as_str())?)
        } else {
            NeedKind::Bus(bus_of(need.kind).ok_or("a need of a kind this host does not know")?)
        };
        if need.name.as_str().is_empty() {
            return Err("a need has no name");
        }
        needs.push(Need {
            name: need.name.as_str(),
            kind,
            optional: need.optional != 0,
        });
    }
    if manifest.provides.as_slice().len() > usize::from(u8::MAX) {
        return Err("too many contracts");
    }
    let mut provides = Vec::new();
    for contract in manifest.provides.as_slice() {
        let mut methods = Vec::new();
        for method in contract.methods.as_slice() {
            let mut params = Vec::new();
            for param in method.params.as_slice() {
                params.push(Param {
                    name: param.name.as_str(),
                    shape: shape_in(&param.shape, MAX_SHAPE_DEPTH)?,
                });
            }
            methods.push(MethodInfo {
                id: method.id,
                name: method.name.as_str(),
                params: Vec::leak(params),
                returns: shape_in(&method.returns, MAX_SHAPE_DEPTH)?,
                has_out_buffer: method.has_out_buffer != 0,
            });
        }
        provides.push(ContractInfo {
            tag: tag_in(contract.tag.as_str())?,
            methods: Vec::leak(methods),
        });
    }
    Ok(Manifest {
        name,
        needs: Vec::leak(needs),
        provides: Vec::leak(provides),
    })
}

/// A package's answer about health, as the host's own type. A kind this host does not know is
/// a failure.
pub fn health_in(health: PackageHealth) -> Health {
    let reason = health.reason.as_str();
    match health.kind {
        HEALTH_OK => Health::Ok,
        HEALTH_DEGRADED => Health::Degraded(reason),
        HEALTH_GONE => Health::Gone(reason),
        HEALTH_FAILED => Health::Failed(reason),
        _ => Health::Failed("the package reported a health this host does not know"),
    }
}

/// One item a package described, as the host's own type. `None` for a kind of item this host
/// does not know.
pub fn item_in(item: &PackageItem) -> Option<Item> {
    Some(Item {
        kind: *ITEM_KINDS.get(usize::from(item.kind))?,
        name: item.name.as_str(),
        unit: UNITS
            .get(usize::from(item.unit))
            .copied()
            .unwrap_or(Unit::One),
        exponent: item.exponent,
        min: (item.flags & ITEM_HAS_MIN != 0).then_some(item.min),
        max: (item.flags & ITEM_HAS_MAX != 0).then_some(item.max),
        method: MethodId {
            contract: item.contract,
            method: item.method,
        },
        field: (item.flags & ITEM_HAS_FIELD != 0).then(|| item.field.as_str()),
    })
}

/// A device's settings in C form, lent for one bind.
pub fn settings_out<'a>(config: &Config<'a>) -> Vec<PackageSetting<'a>> {
    config
        .entries()
        .iter()
        .map(|(key, value)| {
            let mut setting = PackageSetting {
                key: Str::new(key),
                kind: SETTING_INT,
                reserved: 0,
                int: 0,
                float: 0.0,
                text: Str::EMPTY,
            };
            match *value {
                ConfigValue::Int(value) => setting.int = value,
                ConfigValue::Float(value) => {
                    setting.kind = SETTING_FLOAT;
                    setting.float = value;
                }
                ConfigValue::Bool(value) => {
                    setting.kind = SETTING_BOOL;
                    setting.int = i64::from(value);
                }
                ConfigValue::Str(value) => {
                    setting.kind = SETTING_TEXT;
                    setting.text = Str::new(value);
                }
            }
            setting
        })
        .collect()
}

fn setting_in<'a>(setting: &PackageSetting<'a>) -> (&'a str, ConfigValue<'a>) {
    let value = match setting.kind {
        SETTING_FLOAT => ConfigValue::Float(setting.float),
        SETTING_BOOL => ConfigValue::Bool(setting.int != 0),
        SETTING_TEXT => ConfigValue::Str(setting.text.as_str()),
        _ => ConfigValue::Int(setting.int),
    };
    (setting.key.as_str(), value)
}

// ---- The package's side ----

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How this package's drivers reach the devices they use: each `Dev` holds a number, and the
/// host's handle for it is here.
struct Remote {
    host: OnceLock<HostTable>,
    devices: Mutex<Vec<Handle>>,
}

static REMOTE: Remote = Remote {
    host: OnceLock::new(),
    devices: Mutex::new(Vec::new()),
};

impl Remote {
    fn handle(&self, device: u32) -> Result<(&HostTable, Handle)> {
        let host = self.host.get().ok_or(Status::NO_DEVICE)?;
        let handle = lock(&self.devices).get(device as usize).copied();
        match handle {
            Some(handle) if !handle.is_null() => Ok((host, handle)),
            _ => Err(Status::STALE_HANDLE),
        }
    }

    fn add(&self, handle: Handle) -> u32 {
        let mut devices = lock(&self.devices);
        let free = devices.iter().position(|held| held.is_null());
        match free {
            Some(slot) => {
                devices[slot] = handle;
                slot as u32
            }
            None => {
                devices.push(handle);
                (devices.len() - 1) as u32
            }
        }
    }

    fn remove(&self, slot: u32) -> Handle {
        lock(&self.devices)
            .get_mut(slot as usize)
            .map_or(Handle::NULL, |held| core::mem::replace(held, Handle::NULL))
    }
}

impl Router for Remote {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let (host, handle) = self.handle(device)?;
        let room = reply.len();
        let mut len = 0;
        let status = (host.device_call)(
            handle,
            method.method,
            Slice::new(args),
            SliceMut::new(reply),
            Out::new(&mut len),
        );
        status.into_result().map(|_| len.min(room))
    }

    fn health(&self, device: u32) -> Health {
        let Ok((host, handle)) = self.handle(device) else {
            return Health::Failed("missing");
        };
        let mut health = PackageHealth::OK;
        if !(host.device_health)(handle, Out::new(&mut health)).is_ok() {
            return Health::Failed("the host did not answer");
        }
        health_in(health)
    }
}

/// The devices one driver took, given back to the host when the driver goes.
struct Devices {
    host: HostTable,
    slots: Vec<u32>,
}

impl Drop for Devices {
    fn drop(&mut self) {
        for slot in self.slots.drain(..) {
            let _ = (self.host.release)(REMOTE.remove(slot));
        }
    }
}

/// An I2C bus the host holds, used through its handle.
struct RemoteI2c {
    host: HostTable,
    bus: Handle,
}

impl i2c::ErrorType for RemoteI2c {
    type Error = ErrorKind;
}

impl HalI2c for RemoteI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> core::result::Result<(), ErrorKind> {
        if operations.len() > MAX_TRANSFER_STEPS {
            return Err(ErrorKind::InvalidInput);
        }
        let mut steps: [PackageTransfer<'_>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| PackageTransfer::write(&[]));
        let count = operations.len();
        for (step, operation) in steps.iter_mut().zip(operations.iter_mut()) {
            *step = match operation {
                Operation::Read(room) => PackageTransfer::read(room),
                Operation::Write(bytes) => PackageTransfer::write(bytes),
            };
        }
        let status = (self.host.i2c_transaction)(
            self.bus,
            u16::from(address),
            SliceMut::new(&mut steps[..count]),
        );
        if status.is_ok() {
            Ok(())
        } else {
            Err(bus_error_of(status))
        }
    }
}

impl Drop for RemoteI2c {
    fn drop(&mut self) {
        let _ = (self.host.release)(self.bus);
    }
}

/// What a binding driver asks the host for, through the host's table.
struct Asking<'a> {
    host: &'a HostTable,
    binding: *mut c_void,
    devices: &'a mut Devices,
}

impl Asking<'_> {
    fn take(&mut self, kind: u32, need: &str, tag: &str) -> Result<Handle> {
        let mut handle = Handle::NULL;
        let status = (self.host.take)(
            self.binding,
            kind,
            Str::new(need),
            Str::new(tag),
            Out::new(&mut handle),
        );
        status.into_result()?;
        if handle.is_null() {
            return Err(Status::STALE_HANDLE);
        }
        Ok(handle)
    }
}

impl Provider for Asking<'_> {
    fn provided(&self, need: &str) -> bool {
        (self.host.provided)(self.binding, Str::new(need)) != 0
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        let handle = self.take(NEED_DEVICE, need, tag.as_str())?;
        let slot = REMOTE.add(handle);
        self.devices.slots.push(slot);
        // The host took the device for this contract, so its position there is the host's.
        Ok(LocalCaller::new(&REMOTE, slot, 0))
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        let bus = self.take(bus_code(BusKind::I2c), need, "")?;
        Ok(I2c(Box::new(RemoteI2c {
            host: *self.host,
            bus,
        })))
    }
}

/// A `#[driver]` type as its package serves it. [`export_package!`](crate::export_package)
/// names it; nothing else needs to.
pub struct Exported<D> {
    // Dropped first: it may still use the devices below.
    driver: D,
    _devices: Devices,
}

impl<D: Driver + Send + 'static> PackageInstance for Exported<D> {
    fn bind(
        host: &HostTable,
        binding: *mut c_void,
        settings: &[PackageSetting<'_>],
        link: Option<&str>,
    ) -> Result<Self> {
        let host = REMOTE.host.get_or_init(|| *host);
        let mut devices = Devices {
            host: *host,
            slots: Vec::new(),
        };
        let entries: Vec<(&str, ConfigValue<'_>)> = settings.iter().map(setting_in).collect();
        let mut asking = Asking {
            host,
            binding,
            devices: &mut devices,
        };
        let mut resources = Resources::new(&mut asking, link);
        let driver = D::bind(&mut resources, &Config::new(&entries))?;
        Ok(Self {
            driver,
            _devices: devices,
        })
    }

    fn describe(&self, items: &mut [PackageItem]) -> u32 {
        let mut description = Description::new();
        self.driver.describe(&mut description);
        let mut count = 0;
        for (item, out) in description.items().zip(items.iter_mut()) {
            *out = item_out(item);
            count += 1;
        }
        let left_out = description.items().count() - count + description.dropped();
        (count + left_out) as u32
    }

    fn health(&self) -> PackageHealth {
        health_out(self.driver.health())
    }

    fn dispatch(
        &mut self,
        contract: u8,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> (Status, usize) {
        let mut writer = Reply::new(reply);
        let status = self
            .driver
            .dispatch(MethodId { contract, method }, args, &mut writer);
        (status, writer.len())
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.driver.tick(now_ns)
    }

    fn caller(&mut self, caller: u64) {
        self.driver.caller(CallerId(caller));
    }

    fn caller_gone(&mut self, caller: u64) {
        self.driver.caller_gone(CallerId(caller));
    }
}

/// A package's `register` for the driver `D`: its manifest in C form and the table that serves
/// it. [`export_package!`](crate::export_package) puts it in the package's descriptor.
pub extern "C" fn register<D: Driver + Send + 'static>(
    driver: Out<'_, Slice<'static, PackageDriver>>,
) -> Status {
    guard(|| {
        let mut flags = 0;
        if D::TICKS {
            flags |= DRIVER_TICKS;
        }
        if D::TRACKS_CALLERS {
            flags |= DRIVER_TRACKS_CALLERS;
        }
        let registered = Box::leak(Box::new(PackageDriver {
            manifest: manifest_out(&D::MANIFEST),
            table: driver_table::<Exported<D>>(flags),
        }));
        driver.set(Slice::new(core::slice::from_ref(registered)));
        Status::OK
    })
}

/// Makes a crate a driver package: exports the one symbol the service looks for, serving the
/// `#[driver]` type named. Use it once, in a crate built as a `cdylib` with `panic = "unwind"`.
///
/// ```ignore
/// lemnos_core::export_package!(MyDriver);
/// ```
#[macro_export]
macro_rules! export_package {
    ($driver:ty) => {
        #[unsafe(no_mangle)]
        #[allow(non_upper_case_globals)]
        pub static lemnos_package: $crate::package::abi::PackageDescriptor =
            $crate::package::abi::PackageDescriptor::new($crate::package::register::<$driver>);
    };
}
