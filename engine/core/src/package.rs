//! A driver as a package: a compiled file the service loads, built from the same `#[driver]`
//! type a built-in driver is.
//!
//! The package's side is one line, [`export_package!`](crate::export_package). It serves the
//! driver through the C boundary in [`lemnos_abi::package`]: the manifest in C form, the
//! driver's calls behind a function table, and what the driver needs fetched from the host as
//! handles. The host's side is the other half of each conversion here, used by the loader.
//!
//! A package's driver can be given everything a built-in driver can: other devices and every
//! kind of bus and block in [`Provider`]. Each is a handle the host holds, used through the
//! host's table for its kind, so the driver's own code is the same either way.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use std::sync::{Mutex, OnceLock, PoisonError};

use core::time::Duration;

use lemnos_abi::package::{
    BUFFERS_HANDLES, BUFFERS_PLAIN, DRIVER_JOBS, DRIVER_TICKS, DRIVER_TRACKS_CALLERS, Fd,
    HEALTH_DEGRADED, HEALTH_FAILED, HEALTH_GONE, HEALTH_OK, HostTable, ITEM_HAS_FIELD,
    ITEM_HAS_MAX, ITEM_HAS_MIN, MAX_ISO_PACKETS, MAX_TRANSFER_STEPS, METHOD_JOB, NEED_DEVICE,
    NO_BUFFER, NO_JOB, NO_TIMEOUT, Out, PackageContract, PackageControlSetup, PackageDmaBuffer,
    PackageDriver, PackageEdge, PackageField, PackageFrame, PackageHealth, PackageInstance,
    PackageInterruptEvent, PackageIsoPacket, PackageItem, PackageJobState, PackageLineConfig,
    PackageManifest, PackageMethod, PackageNeed, PackagePwmConfig, PackageSetting, PackageShape,
    PackageSpiSegment, PackageTransfer, PackageUsbEvent, SETTING_BOOL, SETTING_FLOAT, SETTING_INT,
    SETTING_TEXT, Slice, SliceMut, Str, TAKE_CLOCKS, TAKE_DMA, TAKE_I2C, TAKE_INTERRUPT, TAKE_LINE,
    TAKE_PWM, TAKE_RAW_SPI, TAKE_REGISTERS, TAKE_RESET, TAKE_SPI, TAKE_STREAM, TAKE_UART,
    TAKE_USB_DEVICE, TAKE_USB_FUNCTION, USB_EVENT_CONFIGURED, USB_EVENT_DECONFIGURED,
    USB_EVENT_NONE, USB_EVENT_RESUMED, USB_EVENT_SETUP, USB_EVENT_SUSPENDED, driver_table,
};
use lemnos_abi::{Handle, guard};
use lemnos_hal::blocks::{
    BufferHandle, BufferKind, ControlSetup, DmaBuffer, InterruptEvent, IsoPacket, StreamFrame,
    UsbFunctionEvent, iso_shape_ok,
};
use lemnos_hal::i2c::{self, I2c as HalI2c, Operation};
use lemnos_hal::raw::{
    Bias, Direction, Drive, Edge, EdgeDetect, LineConfig, Polarity, PwmConfig, RawLine, RawPwm,
    RawSpi, SpiConfig, SpiMode, SpiSegment,
};
use lemnos_hal::spi::{self, SpiDevice};
use lemnos_hal::{
    Clocks, DmaBuffers, Interrupt, Parity as UartParity, Registers, Reset, StopBits,
    StreamEndpoint, Uart as HalUart, UartFormat, UsbDevice, UsbFunction,
};

use crate::{
    BusKind, CallerId, ClocksHandle, Config, ConfigValue, ContractInfo, Description, DmaHandle,
    Driver, ErrorKind, Field, Health, I2c, InterruptHandle, Item, ItemKind, JobId, JobState, Line,
    LocalCaller, Manifest, MethodId, MethodInfo, Need, NeedKind, Param, Provider, Pwm, RawSpiBus,
    RegistersHandle, Reply, ResetHandle, Resources, Result, Router, Shape, Spi, Status,
    StreamHandle, Tag, Uart, Unit, UsbDeviceHandle, UsbFunctionHandle,
};

/// The boundary's own types, for [`export_package!`](crate::export_package) and the loader.
pub use lemnos_abi::package as abi;

const BUSES: [BusKind; 14] = [
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
    BusKind::Pio,
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

const NACK: Status = Status::from_errno(121);
const OVERRUN: Status = Status::from_errno(75);
const CONFIGURATION: Status = Status::from_errno(71);

/// The status a resource's error crosses the boundary as. Every kind has its own, so the
/// driver sees the kind the host's resource reported.
pub fn status_of_bus(kind: ErrorKind) -> Status {
    match kind {
        ErrorKind::NotFound => Status::NOT_FOUND,
        ErrorKind::Unavailable => Status::NO_DEVICE,
        ErrorKind::Busy => Status::BUSY,
        ErrorKind::PermissionDenied => Status::ACCESS,
        ErrorKind::Timeout => Status::TIMED_OUT,
        ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        ErrorKind::Unsupported => Status::NOT_SUPPORTED,
        ErrorKind::Configuration => CONFIGURATION,
        ErrorKind::Nack => NACK,
        ErrorKind::Overrun => OVERRUN,
        _ => Status::IO,
    }
}

/// The resource error a status from the host stands for. A handle the host no longer knows
/// reads as the resource being gone.
pub fn bus_error_of(status: Status) -> ErrorKind {
    match status {
        Status::NOT_FOUND => ErrorKind::NotFound,
        Status::NO_DEVICE | Status::STALE_HANDLE => ErrorKind::Unavailable,
        Status::BUSY => ErrorKind::Busy,
        Status::ACCESS => ErrorKind::PermissionDenied,
        Status::TIMED_OUT => ErrorKind::Timeout,
        Status::INVALID_ARGUMENT => ErrorKind::InvalidInput,
        Status::NOT_SUPPORTED => ErrorKind::Unsupported,
        CONFIGURATION => ErrorKind::Configuration,
        NACK => ErrorKind::Nack,
        OVERRUN => ErrorKind::Overrun,
        Status::IO => ErrorKind::Failed,
        status => status
            .errno()
            .map_or(ErrorKind::Failed, ErrorKind::from_errno),
    }
}

// ---- Resource values in C form, both ways ----

/// A timeout as it crosses: nanoseconds, never [`NO_TIMEOUT`].
pub fn timeout_out(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_nanos())
        .unwrap_or(NO_TIMEOUT)
        .min(NO_TIMEOUT - 1)
}

pub fn timeout_in(timeout_ns: u64) -> Duration {
    Duration::from_nanos(timeout_ns)
}

pub fn line_config_out(config: &LineConfig) -> PackageLineConfig {
    PackageLineConfig {
        direction: config.direction as u8,
        initial: u8::from(config.initial),
        active_low: u8::from(config.active_low),
        bias: config.bias as u8,
        drive: config.drive as u8,
        edge: config.edge as u8,
        reserved: 0,
        debounce_us: config.debounce_us,
    }
}

/// `None` for a value this build does not know.
pub fn line_config_in(config: PackageLineConfig) -> Option<LineConfig> {
    Some(LineConfig {
        direction: *[Direction::Input, Direction::Output].get(usize::from(config.direction))?,
        initial: config.initial != 0,
        active_low: config.active_low != 0,
        bias: *[Bias::AsIs, Bias::PullUp, Bias::PullDown, Bias::Disabled]
            .get(usize::from(config.bias))?,
        drive: *[Drive::PushPull, Drive::OpenDrain, Drive::OpenSource]
            .get(usize::from(config.drive))?,
        edge: *[
            EdgeDetect::None,
            EdgeDetect::Rising,
            EdgeDetect::Falling,
            EdgeDetect::Both,
        ]
        .get(usize::from(config.edge))?,
        debounce_us: config.debounce_us,
    })
}

pub fn edge_out(edge: Option<Edge>) -> PackageEdge {
    edge.map_or(PackageEdge::default(), |edge| PackageEdge {
        present: 1,
        rising: u32::from(edge.rising),
        seq: edge.seq,
        reserved: 0,
        timestamp_ns: edge.timestamp_ns,
    })
}

pub fn edge_in(edge: PackageEdge) -> Option<Edge> {
    (edge.present != 0).then_some(Edge {
        rising: edge.rising != 0,
        timestamp_ns: edge.timestamp_ns,
        seq: edge.seq,
    })
}

pub fn pwm_config_out(config: &PwmConfig) -> PackagePwmConfig {
    PackagePwmConfig {
        period_ns: config.period_ns,
        duty_ns: config.duty_ns,
        polarity: config.polarity as u32,
        enabled: u32::from(config.enabled),
    }
}

/// `None` for a polarity this build does not know.
pub fn pwm_config_in(config: PackagePwmConfig) -> Option<PwmConfig> {
    Some(PwmConfig {
        period_ns: config.period_ns,
        duty_ns: config.duty_ns,
        polarity: *[Polarity::Normal, Polarity::Inversed].get(config.polarity as usize)?,
        enabled: config.enabled != 0,
    })
}

/// A serial frame as it crosses the package boundary: `(parity, stop_bits)`, parity 0 none, 1 even,
/// 2 odd, and stop bits 1 or 2. The rate crosses as it is.
pub fn uart_format_out(format: UartFormat) -> (u8, u8) {
    let parity = match format.parity {
        UartParity::None => 0,
        UartParity::Even => 1,
        UartParity::Odd => 2,
    };
    let stop_bits = match format.stop_bits {
        StopBits::One => 1,
        StopBits::Two => 2,
    };
    (parity, stop_bits)
}

/// `None` for a parity or a stop-bit count this build does not know.
pub fn uart_format_in(baud: u32, parity: u8, stop_bits: u8) -> Option<UartFormat> {
    Some(UartFormat {
        baud,
        parity: *[UartParity::None, UartParity::Even, UartParity::Odd].get(usize::from(parity))?,
        stop_bits: match stop_bits {
            1 => StopBits::One,
            2 => StopBits::Two,
            _ => return None,
        },
    })
}

pub fn interrupt_event_out(event: Option<InterruptEvent>) -> PackageInterruptEvent {
    event.map_or(PackageInterruptEvent::default(), |event| {
        PackageInterruptEvent {
            present: 1,
            count: event.count,
            timestamp_ns: event.timestamp_ns,
        }
    })
}

pub fn interrupt_event_in(event: PackageInterruptEvent) -> Option<InterruptEvent> {
    (event.present != 0).then_some(InterruptEvent {
        count: event.count,
        timestamp_ns: event.timestamp_ns,
    })
}

pub fn dma_buffer_out(buffer: DmaBuffer) -> PackageDmaBuffer {
    PackageDmaBuffer {
        id: buffer.id,
        reserved: 0,
        len: buffer.len,
        device_address: buffer.device_address,
    }
}

pub fn dma_buffer_in(buffer: PackageDmaBuffer) -> DmaBuffer {
    DmaBuffer {
        id: buffer.id,
        len: buffer.len,
        device_address: buffer.device_address,
    }
}

pub fn frame_out(frame: StreamFrame) -> PackageFrame {
    PackageFrame {
        buffer: frame.buffer,
        bytes: frame.bytes,
        sequence: frame.sequence,
        identity: frame.identity,
        timestamp_ns: frame.timestamp_ns,
    }
}

pub fn frame_in(frame: PackageFrame) -> StreamFrame {
    StreamFrame {
        buffer: frame.buffer,
        bytes: frame.bytes,
        sequence: frame.sequence,
        identity: frame.identity,
        timestamp_ns: frame.timestamp_ns,
    }
}

pub fn setup_out(setup: ControlSetup) -> PackageControlSetup {
    PackageControlSetup {
        request_type: setup.request_type,
        request: setup.request,
        value: setup.value,
        index: setup.index,
        reserved: 0,
    }
}

pub fn setup_in(setup: PackageControlSetup) -> ControlSetup {
    ControlSetup {
        request_type: setup.request_type,
        request: setup.request,
        value: setup.value,
        index: setup.index,
    }
}

pub fn usb_event_out(event: Option<UsbFunctionEvent>) -> PackageUsbEvent {
    let plain = |kind| PackageUsbEvent {
        kind,
        ..PackageUsbEvent::default()
    };
    match event {
        None => plain(USB_EVENT_NONE),
        Some(UsbFunctionEvent::Configured) => plain(USB_EVENT_CONFIGURED),
        Some(UsbFunctionEvent::Deconfigured) => plain(USB_EVENT_DECONFIGURED),
        Some(UsbFunctionEvent::Suspended) => plain(USB_EVENT_SUSPENDED),
        Some(UsbFunctionEvent::Resumed) => plain(USB_EVENT_RESUMED),
        Some(UsbFunctionEvent::Setup { setup, length }) => PackageUsbEvent {
            kind: USB_EVENT_SETUP,
            length: u32::from(length),
            setup: setup_out(setup),
        },
    }
}

/// `Err` for an event this build does not know.
pub fn usb_event_in(
    event: PackageUsbEvent,
) -> core::result::Result<Option<UsbFunctionEvent>, ErrorKind> {
    Ok(Some(match event.kind {
        USB_EVENT_NONE => return Ok(None),
        USB_EVENT_CONFIGURED => UsbFunctionEvent::Configured,
        USB_EVENT_DECONFIGURED => UsbFunctionEvent::Deconfigured,
        USB_EVENT_SUSPENDED => UsbFunctionEvent::Suspended,
        USB_EVENT_RESUMED => UsbFunctionEvent::Resumed,
        USB_EVENT_SETUP => UsbFunctionEvent::Setup {
            setup: setup_in(event.setup),
            length: u16::try_from(event.length).map_err(|_| ErrorKind::Failed)?,
        },
        _ => return Err(ErrorKind::Failed),
    }))
}

/// One raw SPI segment in C form.
pub fn segment_out<'a>(segment: &'a mut SpiSegment<'_>) -> PackageSpiSegment<'a> {
    PackageSpiSegment {
        mode: segment.config.mode.bits(),
        bits_per_word: segment.config.bits_per_word,
        cs_change: u8::from(segment.cs_change),
        reserved: 0,
        speed_hz: segment.config.speed_hz,
        delay_us: segment.delay_us,
        reserved2: [0; 3],
        tx: Slice::new(segment.tx),
        rx: SliceMut::new(segment.rx),
    }
}

/// One raw SPI segment as the host's own type, borrowed from the package's.
pub fn segment_in<'a>(segment: &'a mut PackageSpiSegment<'_>) -> SpiSegment<'a> {
    SpiSegment {
        config: SpiConfig {
            mode: SpiMode::from_bits(segment.mode),
            speed_hz: segment.speed_hz,
            bits_per_word: segment.bits_per_word,
        },
        tx: segment.tx.as_slice(),
        rx: segment.rx.as_mut_slice(),
        cs_change: segment.cs_change != 0,
        delay_us: segment.delay_us,
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
        flags: if method.job { METHOD_JOB } else { 0 },
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
                job: method.flags & METHOD_JOB != 0,
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
        let status = (host.device.call)(
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
        if !(host.device.health)(handle, Out::new(&mut health)).is_ok() {
            return Health::Failed("the host did not answer");
        }
        health_in(health)
    }
}

/// The devices one driver took, given back to the host when the driver goes.
struct Devices {
    host: &'static HostTable,
    slots: Vec<u32>,
}

impl Drop for Devices {
    fn drop(&mut self) {
        for slot in self.slots.drain(..) {
            let _ = (self.host.release)(REMOTE.remove(slot));
        }
    }
}

/// Something the host holds for this driver, used through its handle and given back when the
/// driver lets go of it. It is every kind of resource: which one is the host's to know, and a
/// call for another kind is refused there.
struct Taken {
    host: &'static HostTable,
    handle: Handle,
}

impl Drop for Taken {
    fn drop(&mut self) {
        let _ = (self.host.release)(self.handle);
    }
}

type Hal<T> = core::result::Result<T, ErrorKind>;

fn done(status: Status) -> Hal<()> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(bus_error_of(status))
    }
}

/// A length the host wrote, never more than the room it was given.
fn length(status: Status, len: usize, room: usize) -> Hal<usize> {
    done(status).map(|()| len.min(room))
}

impl i2c::ErrorType for Taken {
    type Error = ErrorKind;
}

impl HalI2c for Taken {
    fn transaction(&mut self, address: u8, operations: &mut [Operation<'_>]) -> Hal<()> {
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
        done((self.host.i2c.transaction)(
            self.handle,
            u16::from(address),
            SliceMut::new(&mut steps[..count]),
        ))
    }
}

impl spi::ErrorType for Taken {
    type Error = ErrorKind;
}

impl SpiDevice for Taken {
    fn transaction(&mut self, operations: &mut [spi::Operation<'_, u8>]) -> Hal<()> {
        if operations.len() > MAX_TRANSFER_STEPS {
            return Err(ErrorKind::InvalidInput);
        }
        let mut steps: [PackageTransfer<'_>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| PackageTransfer::write(&[]));
        let count = operations.len();
        for (step, operation) in steps.iter_mut().zip(operations.iter_mut()) {
            *step = match operation {
                spi::Operation::Read(room) => PackageTransfer::read(room),
                spi::Operation::Write(bytes) => PackageTransfer::write(bytes),
                spi::Operation::Transfer(room, bytes) => PackageTransfer::both(room, bytes),
                spi::Operation::TransferInPlace(bytes) => PackageTransfer::in_place(bytes),
                spi::Operation::DelayNs(ns) => PackageTransfer::delay(*ns),
            };
        }
        done((self.host.spi.transaction)(
            self.handle,
            SliceMut::new(&mut steps[..count]),
        ))
    }
}

impl RawSpi for Taken {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Hal<()> {
        if segments.len() > MAX_TRANSFER_STEPS {
            return Err(ErrorKind::InvalidInput);
        }
        let count = segments.len();
        let mut segments = segments.iter_mut();
        let mut carried: [Option<PackageSpiSegment<'_>>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| segments.next().map(segment_out));
        // The segments in one row, as the host takes them.
        let mut row: [PackageSpiSegment<'_>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|at| match carried[at].take() {
                Some(segment) => segment,
                None => PackageSpiSegment {
                    mode: 0,
                    bits_per_word: 0,
                    cs_change: 0,
                    reserved: 0,
                    speed_hz: 0,
                    delay_us: 0,
                    reserved2: [0; 3],
                    tx: Slice::EMPTY,
                    rx: SliceMut::empty(),
                },
            });
        done((self.host.spi.transfer)(
            self.handle,
            SliceMut::new(&mut row[..count]),
        ))
    }
}

impl RawLine for Taken {
    fn configure(&mut self, config: &LineConfig) -> Hal<()> {
        done((self.host.line.configure)(
            self.handle,
            line_config_out(config),
        ))
    }

    fn get(&mut self) -> Hal<bool> {
        let mut value = 0;
        done((self.host.line.get)(self.handle, Out::new(&mut value)))?;
        Ok(value != 0)
    }

    fn set(&mut self, value: bool) -> Hal<()> {
        done((self.host.line.set)(self.handle, u32::from(value)))
    }

    fn read_edge(&mut self) -> Hal<Option<Edge>> {
        let mut edge = PackageEdge::default();
        done((self.host.line.read_edge)(self.handle, Out::new(&mut edge)))?;
        Ok(edge_in(edge))
    }
}

impl RawPwm for Taken {
    fn configure(&mut self, config: &PwmConfig) -> Hal<()> {
        done((self.host.pwm.configure)(
            self.handle,
            pwm_config_out(config),
        ))
    }

    fn config(&mut self) -> Hal<PwmConfig> {
        let mut config = PackagePwmConfig::default();
        done((self.host.pwm.config)(self.handle, Out::new(&mut config)))?;
        pwm_config_in(config).ok_or(ErrorKind::Failed)
    }
}

impl HalUart for Taken {
    type Error = ErrorKind;

    fn configure(&mut self, format: UartFormat) -> Hal<()> {
        let (parity, stop_bits) = uart_format_out(format);
        done((self.host.uart.configure)(
            self.handle,
            format.baud,
            parity,
            stop_bits,
        ))
    }

    fn write(&mut self, data: &[u8]) -> Hal<usize> {
        let mut len = 0;
        let status = (self.host.uart.write)(self.handle, Slice::new(data), Out::new(&mut len));
        length(status, len, data.len())
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Hal<usize> {
        let (room, mut len) = (out.len(), 0);
        let status = (self.host.uart.read)(
            self.handle,
            SliceMut::new(out),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, room)
    }
}

impl Registers for Taken {
    type Error = ErrorKind;

    fn size(&self) -> usize {
        let mut size = 0;
        let status = (self.host.registers.size)(self.handle, Out::new(&mut size));
        if status.is_ok() { size } else { 0 }
    }

    fn read32(&mut self, offset: usize) -> Hal<u32> {
        let mut value = 0;
        done((self.host.registers.read32)(
            self.handle,
            offset,
            Out::new(&mut value),
        ))?;
        Ok(value)
    }

    fn write32(&mut self, offset: usize, value: u32) -> Hal<()> {
        done((self.host.registers.write32)(self.handle, offset, value))
    }

    fn modify32(&mut self, offset: usize, clear: u32, set: u32) -> Hal<()> {
        done((self.host.registers.modify32)(
            self.handle,
            offset,
            clear,
            set,
        ))
    }

    fn write_block(&mut self, offset: usize, values: &[u32]) -> Hal<()> {
        done((self.host.registers.write_block)(
            self.handle,
            offset,
            Slice::new(values),
        ))
    }
}

impl Interrupt for Taken {
    type Error = ErrorKind;

    fn enable(&mut self) -> Hal<()> {
        done((self.host.interrupt.enable)(self.handle))
    }

    fn disable(&mut self) -> Hal<()> {
        done((self.host.interrupt.disable)(self.handle))
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Hal<Option<InterruptEvent>> {
        let mut event = PackageInterruptEvent::default();
        done((self.host.interrupt.wait)(
            self.handle,
            timeout.map_or(NO_TIMEOUT, timeout_out),
            Out::new(&mut event),
        ))?;
        Ok(interrupt_event_in(event))
    }
}

impl DmaBuffers for Taken {
    type Error = ErrorKind;

    fn allocate(&mut self, len: u64) -> Hal<DmaBuffer> {
        let mut buffer = PackageDmaBuffer::default();
        done((self.host.dma.allocate)(
            self.handle,
            len,
            Out::new(&mut buffer),
        ))?;
        Ok(dma_buffer_in(buffer))
    }

    fn release(&mut self, id: u32) -> Hal<()> {
        done((self.host.dma.release)(self.handle, id))
    }

    fn write(&mut self, id: u32, offset: u64, data: &[u8]) -> Hal<()> {
        done((self.host.dma.write)(
            self.handle,
            id,
            offset,
            Slice::new(data),
        ))
    }

    fn read(&mut self, id: u32, offset: u64, out: &mut [u8]) -> Hal<()> {
        done((self.host.dma.read)(
            self.handle,
            id,
            offset,
            SliceMut::new(out),
        ))
    }

    fn sync_for_device(&mut self, id: u32) -> Hal<()> {
        done((self.host.dma.sync_for_device)(self.handle, id))
    }

    fn sync_for_cpu(&mut self, id: u32) -> Hal<()> {
        done((self.host.dma.sync_for_cpu)(self.handle, id))
    }
}

impl Clocks for Taken {
    type Error = ErrorKind;

    fn enable(&mut self, clock: u32) -> Hal<()> {
        done((self.host.clocks.enable)(self.handle, clock))
    }

    fn disable(&mut self, clock: u32) -> Hal<()> {
        done((self.host.clocks.disable)(self.handle, clock))
    }

    fn rate_hz(&mut self, clock: u32) -> Hal<u64> {
        let mut hz = 0;
        done((self.host.clocks.rate_hz)(
            self.handle,
            clock,
            Out::new(&mut hz),
        ))?;
        Ok(hz)
    }

    fn set_rate_hz(&mut self, clock: u32, hz: u64) -> Hal<u64> {
        let mut settled = 0;
        done((self.host.clocks.set_rate_hz)(
            self.handle,
            clock,
            hz,
            Out::new(&mut settled),
        ))?;
        Ok(settled)
    }
}

impl Reset for Taken {
    type Error = ErrorKind;

    fn assert(&mut self, line: u32) -> Hal<()> {
        done((self.host.reset.assert_reset)(self.handle, line))
    }

    fn deassert(&mut self, line: u32) -> Hal<()> {
        done((self.host.reset.deassert_reset)(self.handle, line))
    }
}

impl StreamEndpoint for Taken {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        let mut count = 0;
        let status = (self.host.stream.buffer_count)(self.handle, Out::new(&mut count));
        if status.is_ok() { count } else { 0 }
    }

    fn publish(&mut self, frame: StreamFrame) -> Hal<()> {
        done((self.host.stream.publish)(self.handle, frame_out(frame)))
    }

    fn next_buffer(&mut self) -> Hal<Option<u32>> {
        let mut buffer = NO_BUFFER;
        done((self.host.stream.next_buffer)(
            self.handle,
            Out::new(&mut buffer),
        ))?;
        Ok((buffer != NO_BUFFER).then_some(buffer))
    }

    fn next_job(&mut self, out: &mut [u8]) -> Hal<Option<usize>> {
        let (room, mut len) = (out.len(), NO_JOB);
        done((self.host.stream.next_job)(
            self.handle,
            SliceMut::new(out),
            Out::new(&mut len),
        ))?;
        Ok((len != NO_JOB).then_some(len.min(room)))
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Hal<bool> {
        let mut attached = 0;
        done((self.host.stream.write)(
            self.handle,
            buffer,
            offset,
            Slice::new(data),
            Out::new(&mut attached),
        ))?;
        Ok(attached != 0)
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Hal<bool> {
        let mut attached = 0;
        done((self.host.stream.read)(
            self.handle,
            buffer,
            offset,
            SliceMut::new(out),
            Out::new(&mut attached),
        ))?;
        Ok(attached != 0)
    }

    fn buffer_handle(&self, buffer: u32) -> Hal<Option<BufferHandle<'_>>> {
        let mut fd = Fd::NONE;
        done((self.host.stream.buffer_handle)(
            self.handle,
            buffer,
            Out::new(&mut fd),
        ))?;
        lent(self, fd)
    }

    fn buffer_current(&self, buffer: u32) -> Hal<bool> {
        let mut current = 0;
        done((self.host.stream.buffer_current)(
            self.handle,
            buffer,
            Out::new(&mut current),
        ))?;
        Ok(current != 0)
    }

    fn buffer_kind(&self) -> BufferKind {
        let mut kind = BUFFERS_PLAIN;
        let status = (self.host.stream.buffer_kind)(self.handle, Out::new(&mut kind));
        // A host that can't say has no handles to lend.
        if status.is_ok() && kind == BUFFERS_HANDLES {
            BufferKind::Handles
        } else {
            BufferKind::Plain
        }
    }

    fn fault_consumer(&mut self) -> Hal<()> {
        done((self.host.stream.fault_consumer)(self.handle))
    }

    fn planes(&self) -> u32 {
        let mut count = 1;
        let status = (self.host.stream.planes)(self.handle, Out::new(&mut count));
        // A host that can't say lends whole buffers.
        if status.is_ok() { count.max(1) } else { 1 }
    }

    fn plane_offset(&self, plane: u32) -> Option<usize> {
        let mut offset = 0;
        let status = (self.host.stream.plane_offset)(self.handle, plane, Out::new(&mut offset));
        status.is_ok().then_some(offset)
    }

    fn plane_handle(&self, buffer: u32, plane: u32) -> Hal<Option<BufferHandle<'_>>> {
        let mut fd = Fd::NONE;
        done((self.host.stream.plane_handle)(
            self.handle,
            buffer,
            plane,
            Out::new(&mut fd),
        ))?;
        lent(self, fd)
    }
}

/// The descriptor the host lent for a buffer, borrowed for as long as the stream is.
#[cfg(unix)]
fn lent(stream: &Taken, fd: Fd) -> Hal<Option<BufferHandle<'_>>> {
    Ok(fd.borrowed(stream).map(BufferHandle::from))
}

/// No handle can be made here.
#[cfg(not(unix))]
fn lent(_stream: &Taken, _fd: Fd) -> Hal<Option<BufferHandle<'_>>> {
    Err(ErrorKind::Unsupported)
}

impl UsbDevice for Taken {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Hal<usize> {
        let (room, mut len) = (out.len(), 0);
        let status = (self.host.usb_device.config_descriptor)(
            self.handle,
            index,
            SliceMut::new(out),
            Out::new(&mut len),
        );
        length(status, len, room)
    }

    fn reset(&mut self) -> Hal<()> {
        done((self.host.usb_device.reset)(self.handle))
    }

    fn detach_kernel_driver(&mut self, interface: u8) -> Hal<()> {
        done((self.host.usb_device.detach_kernel_driver)(
            self.handle,
            interface,
        ))
    }

    fn claim_interface(&mut self, interface: u8) -> Hal<()> {
        done((self.host.usb_device.claim_interface)(
            self.handle,
            interface,
        ))
    }

    fn release_interface(&mut self, interface: u8) -> Hal<()> {
        done((self.host.usb_device.release_interface)(
            self.handle,
            interface,
        ))
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Hal<()> {
        done((self.host.usb_device.set_alternate)(
            self.handle,
            interface,
            alternate,
        ))
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Hal<usize> {
        let (room, mut len) = (data.len(), 0);
        let status = (self.host.usb_device.control_in)(
            self.handle,
            setup_out(setup),
            SliceMut::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, room)
    }

    fn control_out(&mut self, setup: ControlSetup, data: &[u8], timeout: Duration) -> Hal<usize> {
        let mut len = 0;
        let status = (self.host.usb_device.control_out)(
            self.handle,
            setup_out(setup),
            Slice::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, data.len())
    }

    fn bulk_in(&mut self, endpoint: u8, data: &mut [u8], timeout: Duration) -> Hal<usize> {
        let (room, mut len) = (data.len(), 0);
        let status = (self.host.usb_device.bulk_in)(
            self.handle,
            endpoint,
            SliceMut::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, room)
    }

    fn bulk_out(&mut self, endpoint: u8, data: &[u8], timeout: Duration) -> Hal<usize> {
        let mut len = 0;
        let status = (self.host.usb_device.bulk_out)(
            self.handle,
            endpoint,
            Slice::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, data.len())
    }

    fn interrupt_in(&mut self, endpoint: u8, data: &mut [u8], timeout: Duration) -> Hal<usize> {
        let (room, mut len) = (data.len(), 0);
        let status = (self.host.usb_device.interrupt_in)(
            self.handle,
            endpoint,
            SliceMut::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, room)
    }

    /// A shape out of range never crosses.
    fn iso_in_start(
        &mut self,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Hal<()> {
        if !iso_shape_ok(transfers, packets, packet_bytes) {
            return Err(ErrorKind::InvalidInput);
        }
        done((self.host.usb_device.iso_in_start)(
            self.handle,
            endpoint,
            transfers,
            packets,
            packet_bytes,
        ))
    }

    fn iso_in_next(
        &mut self,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Hal<()> {
        if packets.is_empty() || packets.len() > MAX_ISO_PACKETS {
            return Err(ErrorKind::InvalidInput);
        }
        let mut carried = [PackageIsoPacket::default(); MAX_ISO_PACKETS];
        done((self.host.usb_device.iso_in_next)(
            self.handle,
            SliceMut::new(data),
            SliceMut::new(&mut carried[..packets.len()]),
            timeout_out(timeout),
        ))?;
        // Packet `i` starts at `i * capacity`, so no packet had more room than `data` shared
        // out evenly, whatever the host says.
        let room = u32::try_from(data.len() / packets.len()).unwrap_or(u32::MAX);
        for (packet, carried) in packets.iter_mut().zip(carried.iter()) {
            packet.capacity = carried.capacity.min(room);
            packet.received = carried.received.min(packet.capacity);
            packet.ok = carried.ok != 0;
        }
        Ok(())
    }

    fn iso_in_stop(&mut self) -> Hal<()> {
        done((self.host.usb_device.iso_in_stop)(self.handle))
    }
}

impl UsbFunction for Taken {
    type Error = ErrorKind;

    /// A package's function is not lent its descriptors: the bytes stay with its driver.
    fn endpoint_handles(&self) -> Hal<[BufferHandle<'_>; 2]> {
        Err(ErrorKind::Unsupported)
    }

    fn enable(&mut self, descriptors: &[u8]) -> Hal<()> {
        done((self.host.usb_function.enable)(
            self.handle,
            Slice::new(descriptors),
        ))
    }

    fn disable(&mut self) -> Hal<()> {
        done((self.host.usb_function.disable)(self.handle))
    }

    fn next_event(&mut self) -> Hal<Option<UsbFunctionEvent>> {
        let mut event = PackageUsbEvent::default();
        done((self.host.usb_function.next_event)(
            self.handle,
            Out::new(&mut event),
        ))?;
        usb_event_in(event)
    }

    fn control_in(&mut self, data: &[u8], timeout: Duration) -> Hal<usize> {
        let mut len = 0;
        let status = (self.host.usb_function.control_in)(
            self.handle,
            Slice::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, data.len())
    }

    fn control_out(&mut self, data: &mut [u8], timeout: Duration) -> Hal<usize> {
        let (room, mut len) = (data.len(), 0);
        let status = (self.host.usb_function.control_out)(
            self.handle,
            SliceMut::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, room)
    }

    fn control_stall(&mut self) -> Hal<()> {
        done((self.host.usb_function.control_stall)(self.handle))
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Hal<()> {
        done((self.host.usb_function.set_halt)(
            self.handle,
            endpoint,
            u32::from(halted),
        ))
    }

    fn read(&mut self, endpoint: u8, data: &mut [u8], timeout: Duration) -> Hal<usize> {
        let (room, mut len) = (data.len(), 0);
        let status = (self.host.usb_function.read)(
            self.handle,
            endpoint,
            SliceMut::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, room)
    }

    fn write(&mut self, endpoint: u8, data: &[u8], timeout: Duration) -> Hal<usize> {
        let mut len = 0;
        let status = (self.host.usb_function.write)(
            self.handle,
            endpoint,
            Slice::new(data),
            timeout_out(timeout),
            Out::new(&mut len),
        );
        length(status, len, data.len())
    }
}

/// What a binding driver asks the host for, through the host's table.
struct Asking<'a> {
    host: &'static HostTable,
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

    fn taken(&mut self, kind: u32, need: &str) -> Result<Box<Taken>> {
        let handle = self.take(kind, need, "")?;
        Ok(Box::new(Taken {
            host: self.host,
            handle,
        }))
    }
}

macro_rules! asked {
    ($($method:ident -> $handle:ty = $kind:ident;)*) => {$(
        fn $method(&mut self, need: &str) -> Result<$handle> {
            Ok(crate::Held::boxed(self.taken($kind, need)?))
        }
    )*};
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
        Ok(I2c(crate::Held::boxed(self.taken(TAKE_I2C, need)?)))
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        Ok(Spi(crate::Held::boxed(self.taken(TAKE_SPI, need)?)))
    }

    asked! {
        raw_spi -> RawSpiBus = TAKE_RAW_SPI;
        line -> Line = TAKE_LINE;
        pwm -> Pwm = TAKE_PWM;
        uart -> Uart = TAKE_UART;
        usb_device -> UsbDeviceHandle = TAKE_USB_DEVICE;
        usb_function -> UsbFunctionHandle = TAKE_USB_FUNCTION;
        registers -> RegistersHandle = TAKE_REGISTERS;
        interrupt -> InterruptHandle = TAKE_INTERRUPT;
        dma -> DmaHandle = TAKE_DMA;
        clocks -> ClocksHandle = TAKE_CLOCKS;
        reset -> ResetHandle = TAKE_RESET;
        stream -> StreamHandle = TAKE_STREAM;
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
        // A table shorter than the one this package was built against lacks functions it calls.
        if (host.struct_size as usize) < size_of::<HostTable>() {
            return Err(Status::VERSION_MISMATCH);
        }
        // The host's table is the same for every bind: this package keeps its own copy.
        let host = REMOTE.host.get_or_init(|| *host);
        let mut devices = Devices {
            host,
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

    fn job(&mut self, job: u64, result: &mut [u8]) -> (PackageJobState, usize) {
        let mut writer = Reply::new(result);
        let state = match self.driver.job(JobId(job), &mut writer) {
            JobState::Running { done, total } => PackageJobState {
                finished: 0,
                status: Status::OK,
                done,
                total,
            },
            JobState::Finished(status) => PackageJobState {
                finished: 1,
                status,
                done: 0,
                total: 0,
            },
        };
        (state, writer.len())
    }

    fn job_cancel(&mut self, job: u64) {
        self.driver.job_cancel(JobId(job));
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
        if D::JOBS {
            flags |= DRIVER_JOBS;
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

#[cfg(test)]
mod tests {
    use lemnos_abi::package::{
        ClocksHost, DeviceHost, DmaHost, I2cHost, InterruptHost, LineHost, PackageBinding,
        PackageHost, PwmHost, RegistersHost, ResetHost, SpiHost, StreamHost, UartHost,
        UsbDeviceHost, UsbFunctionHost, host_table,
    };

    use lemnos_hal::blocks::{MAX_ISO_PACKET_BYTES, MAX_ISO_TRANSFERS};

    use super::*;

    /// A host that answers with lengths and counts larger than the room it was given, and
    /// counts what reaches it.
    struct Liar;

    static REACHED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn reached() -> u32 {
        REACHED.load(std::sync::atomic::Ordering::Relaxed)
    }

    impl PackageHost for Liar {
        fn release(_resource: Handle) -> Status {
            Status::OK
        }
    }

    impl UartHost for Liar {
        fn write(_uart: Handle, data: Slice<'_, u8>, len: Out<'_, usize>) -> Status {
            len.set(data.as_slice().len() + 5);
            Status::OK
        }

        fn read(_uart: Handle, _out: SliceMut<'_, u8>, _ns: u64, len: Out<'_, usize>) -> Status {
            len.set(usize::MAX - 1);
            Status::OK
        }
    }

    impl StreamHost for Liar {
        fn next_job(_stream: Handle, _out: SliceMut<'_, u8>, len: Out<'_, usize>) -> Status {
            len.set(1 << 40);
            Status::OK
        }
    }

    impl UsbDeviceHost for Liar {
        fn bulk_in(
            _device: Handle,
            _endpoint: u8,
            _data: SliceMut<'_, u8>,
            _timeout_ns: u64,
            len: Out<'_, usize>,
        ) -> Status {
            len.set(4096);
            Status::OK
        }

        fn iso_in_start(_: Handle, _: u8, _: u8, _: u16, _: u32) -> Status {
            REACHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Status::OK
        }

        fn iso_in_next(
            _device: Handle,
            _data: SliceMut<'_, u8>,
            mut packets: SliceMut<'_, PackageIsoPacket>,
            _timeout_ns: u64,
        ) -> Status {
            for packet in packets.as_mut_slice() {
                packet.capacity = u32::MAX;
                packet.received = u32::MAX;
                packet.ok = 7;
            }
            Status::OK
        }
    }

    impl I2cHost for Liar {
        fn transaction(_: Handle, _: u16, _: SliceMut<'_, PackageTransfer<'_>>) -> Status {
            REACHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Status::OK
        }
    }

    impl DeviceHost for Liar {}
    impl SpiHost for Liar {}
    impl LineHost for Liar {}
    impl PwmHost for Liar {}
    impl RegistersHost for Liar {}
    impl InterruptHost for Liar {}
    impl DmaHost for Liar {}
    impl ClocksHost for Liar {}
    impl ResetHost for Liar {}
    impl UsbFunctionHost for Liar {}

    struct Nobody;

    impl PackageBinding for Nobody {
        fn provided(&mut self, _need: &str) -> bool {
            false
        }

        fn take(&mut self, _kind: u32, _need: &str, _tag: &str) -> Result<Handle> {
            Err(Status::NOT_FOUND)
        }
    }

    static LIAR: HostTable = host_table::<Nobody, Liar>();

    fn taken() -> Taken {
        Taken {
            host: &LIAR,
            handle: Handle::new(1, 1),
        }
    }

    #[test]
    fn a_length_the_host_reports_never_exceeds_the_room_it_was_given() {
        let mut resource = taken();
        let mut room = [0u8; 4];
        assert_eq!(
            HalUart::read(&mut resource, &mut room, Duration::ZERO),
            Ok(4)
        );
        assert_eq!(HalUart::write(&mut resource, &[1, 2, 3]), Ok(3));
        assert_eq!(resource.next_job(&mut room), Ok(Some(4)));
        assert_eq!(resource.bulk_in(0x81, &mut room, Duration::ZERO), Ok(4));
        let mut packets = [IsoPacket::default(); 2];
        let mut data = [0u8; 16];
        resource
            .iso_in_next(&mut data, &mut packets, Duration::ZERO)
            .unwrap();
        assert_eq!(
            (packets[1].capacity, packets[1].received, packets[1].ok),
            (8, 8, true)
        );
    }

    #[test]
    fn more_than_the_boundary_carries_is_refused_before_it_crosses() {
        let mut resource = taken();
        let before = reached();
        let mut operations: [Operation<'_>; MAX_TRANSFER_STEPS + 1] =
            core::array::from_fn(|_| Operation::Write(&[]));
        assert_eq!(
            HalI2c::transaction(&mut resource, 0x20, &mut operations),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(reached(), before);
        assert_eq!(
            HalI2c::transaction(&mut resource, 0x20, &mut operations[..MAX_TRANSFER_STEPS]),
            Ok(())
        );
        assert_eq!(reached(), before + 1);
        let mut spi: [spi::Operation<'_, u8>; MAX_TRANSFER_STEPS + 1] =
            core::array::from_fn(|_| spi::Operation::DelayNs(1));
        assert_eq!(
            SpiDevice::transaction(&mut resource, &mut spi),
            Err(ErrorKind::InvalidInput)
        );
        let mut packets = [IsoPacket::default(); MAX_ISO_PACKETS + 1];
        assert_eq!(
            resource.iso_in_next(&mut [], &mut packets, Duration::ZERO),
            Err(ErrorKind::InvalidInput)
        );
        let before = reached();
        for (transfers, packets, bytes) in [
            (0, 1, 1),
            (MAX_ISO_TRANSFERS + 1, 1, 1),
            (1, MAX_ISO_PACKETS as u16 + 1, 1),
            (1, 1, MAX_ISO_PACKET_BYTES + 1),
        ] {
            assert_eq!(
                resource.iso_in_start(0x81, transfers, packets, bytes),
                Err(ErrorKind::InvalidInput)
            );
        }
        assert_eq!(reached(), before);
        resource.iso_in_start(0x81, 4, 32, 944).unwrap();
        assert_eq!(reached(), before + 1);
    }

    #[test]
    fn a_kind_the_host_lacks_is_unsupported_as_on_any_host() {
        let mut resource = taken();
        assert_eq!(Reset::assert(&mut resource, 0), Err(ErrorKind::Unsupported));
        assert_eq!(RawLine::get(&mut resource), Err(ErrorKind::Unsupported));
        assert_eq!(
            resource.buffer_handle(0).map(|handle| handle.is_some()),
            Err(ErrorKind::Unsupported)
        );
        assert_eq!(Registers::size(&resource), 0);
        assert_eq!(resource.buffer_count(), 0);
        assert_eq!(resource.buffer_kind(), BufferKind::Plain);
        assert_eq!(resource.buffer_current(0), Err(ErrorKind::Unsupported));
        assert_eq!(resource.fault_consumer(), Err(ErrorKind::Unsupported));
    }
}
