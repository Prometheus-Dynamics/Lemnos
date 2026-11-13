use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{
    ClocksHandle, DmaHandle, I2c, InterruptHandle, Line, LocalCaller, Provider, Pwm, RawSpiBus,
    RegistersHandle, ResetHandle, Result, Spi, Status, StreamHandle, Tag, Uart, UsbDeviceHandle,
    UsbFunctionHandle,
};
use lemnos_hal::mock::{
    MockClocks, MockDma, MockInterrupt, MockLine, MockPwm, MockRawSpi, MockRegisters, MockReset,
    MockStream, MockUart, MockUsbDevice, MockUsbFunction,
};
use lemnos_hal::usb_match::{self, ClassCodes, UsbCodes, UsbPattern};

use crate::faults::Release;
use crate::{FaultScript, Faulty, SimClock, SimHints, SimI2c, SimSpi};

/// One way of wiring a resource.
#[derive(Clone)]
enum Part {
    I2c(SimI2c),
    Spi(SimSpi),
    RawSpi(MockRawSpi),
    Line(MockLine),
    Pwm(MockPwm),
    Uart(MockUart),
    UsbDevice(MockUsbDevice),
    UsbFunction(MockUsbFunction),
    Registers(MockRegisters),
    Interrupt(MockInterrupt),
    Dma(MockDma),
    Clocks(MockClocks),
    Reset(MockReset),
    Stream(MockStream),
}

impl Part {
    /// A bus is shared by every device on it; anything else has one owner.
    fn exclusive(&self) -> bool {
        !matches!(self, Self::I2c(_))
    }
}

#[derive(Default)]
struct Parts {
    parts: BTreeMap<String, (Part, FaultScript)>,
    claimed: BTreeSet<String>,
    /// What each USB device added with [`SimBoard::add_usb_device_as`] says it is: its device
    /// descriptor's codes and its interfaces' classes.
    usb: BTreeMap<String, (UsbCodes, Vec<ClassCodes>)>,
}

/// A simulated board's resources, by the name a board file gives them. Clones
/// share the board. Each `add_*` returns a handle for the test to drive.
#[derive(Clone, Default)]
pub struct SimBoard {
    state: Arc<Mutex<Parts>>,
    clock: SimClock,
    hints: SimHints,
}

macro_rules! add {
    ($($add:ident($($arg:ident: $ty:ty),*) -> $mock:ident as $part:ident = $make:expr;)*) => {$(
        pub fn $add(&self, name: &str, $($arg: $ty),*) -> $mock {
            let mock: $mock = $make;
            self.insert(name, Part::$part(mock.clone()));
            mock
        }
    )*};
}

macro_rules! provide {
    ($($method:ident -> $handle:ty as $part:ident;)*) => {$(
        fn $method(&mut self, need: &str) -> Result<$handle> {
            let (part, faults, release) = self.take(need)?;
            match part {
                Part::$part(mock) => Ok(lemnos_core::Held::boxed(Box::new(Faulty::new(
                    mock, faults, release,
                )))),
                _ => Err(Status::INVALID_ARGUMENT),
            }
        }
    )*};
}

impl SimBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clock(&self) -> SimClock {
        self.clock.clone()
    }

    fn with<R>(&self, f: impl FnOnce(&mut Parts) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// What this board says as resources are added and removed: a [`Hint`](lemnos_core::Hint)
    /// for each, for a test to hand to the host, or to lose.
    pub fn hints(&self) -> SimHints {
        self.hints.clone()
    }

    fn insert(&self, name: &str, part: Part) {
        let faults = FaultScript::on_clock(self.clock.clone());
        self.with(|s| s.parts.insert(name.to_string(), (part, faults)));
        self.hints.push(name);
    }

    /// Adds a USB device that says it is `codes`, with the configuration `descriptor`, whose
    /// interfaces' classes it says too. A board name that says what a device is
    /// (`usb:VVVV:PPPP`, `usb:class:CC`, see [`lemnos_hal::usb_match`]) finds it by these, and
    /// [`find`](Provider::find) reports it by `name`.
    pub fn add_usb_device_as(
        &self,
        name: &str,
        codes: UsbCodes,
        descriptor: &[u8],
    ) -> MockUsbDevice {
        let interfaces = usb_match::interface_classes(descriptor).collect();
        self.with(|s| s.usb.insert(name.to_string(), (codes, interfaces)));
        self.add_usb_device(name, descriptor)
    }

    /// Removes a resource, as unplugging a device does. Whoever holds it keeps what they hold.
    pub fn remove(&self, name: &str) {
        if self
            .with(|s| {
                s.usb.remove(name);
                s.parts.remove(name)
            })
            .is_some()
        {
            self.hints.push(name);
        }
    }

    /// The fault script of a named resource.
    pub fn faults(&self, name: &str) -> Option<FaultScript> {
        self.with(|s| s.parts.get(name).map(|(_, faults)| faults.clone()))
    }

    pub fn is_claimed(&self, name: &str) -> bool {
        self.with(|s| s.claimed.contains(name))
    }

    add! {
        add_spi() -> SimSpi as Spi = SimSpi::new();
        add_raw_spi() -> MockRawSpi as RawSpi = MockRawSpi::default();
        add_line() -> MockLine as Line = MockLine::new();
        add_pwm() -> MockPwm as Pwm = MockPwm::default();
        add_uart() -> MockUart as Uart = MockUart::new();
        add_usb_device(descriptor: &[u8]) -> MockUsbDevice as UsbDevice = MockUsbDevice::new(descriptor);
        add_usb_function() -> MockUsbFunction as UsbFunction = MockUsbFunction::new();
        add_registers(size: usize) -> MockRegisters as Registers = MockRegisters::new(size);
        add_interrupt() -> MockInterrupt as Interrupt = MockInterrupt::new();
        add_dma() -> MockDma as Dma = MockDma::new();
        add_clocks() -> MockClocks as Clocks = MockClocks::new();
        add_reset() -> MockReset as Reset = MockReset::new();
        add_stream(buffers: u32) -> MockStream as Stream = MockStream::new(buffers);
    }

    /// Adds a stream the test made itself: one whose buffers hold bytes
    /// ([`MockStream::with_bytes`]) or are behind handles ([`MockStream::with_handles`]).
    pub fn add_stream_of(&self, name: &str, stream: MockStream) -> MockStream {
        self.insert(name, Part::Stream(stream.clone()));
        stream
    }

    /// Adds a USB function the test made itself, such as
    /// [`MockUsbFunction::with_endpoint_files`].
    pub fn add_usb_function_of(&self, name: &str, function: MockUsbFunction) -> MockUsbFunction {
        self.insert(name, Part::UsbFunction(function.clone()));
        function
    }

    /// Adds DMA memory the test made itself, such as [`MockDma::at`].
    pub fn add_dma_of(&self, name: &str, dma: MockDma) -> MockDma {
        self.insert(name, Part::Dma(dma.clone()));
        dma
    }

    /// Adds clocks the test made itself, such as [`MockClocks::counted`].
    pub fn add_clocks_of(&self, name: &str, clocks: MockClocks) -> MockClocks {
        self.insert(name, Part::Clocks(clocks.clone()));
        clocks
    }

    /// Adds reset lines the test made itself, such as [`MockReset::counted`].
    pub fn add_reset_of(&self, name: &str, reset: MockReset) -> MockReset {
        self.insert(name, Part::Reset(reset.clone()));
        reset
    }

    pub fn add_i2c(&self, name: &str) -> SimI2c {
        let bus = SimI2c::new(self.clock.clone());
        self.insert(name, Part::I2c(bus.clone()));
        bus
    }

    fn take(&self, name: &str) -> Result<(Part, FaultScript, Release)> {
        let (name, part, faults) = self.with(|s| {
            let name = resolve(s, name)?;
            let (part, faults) = s.parts.get(&name).ok_or(Status::NOT_FOUND)?.clone();
            if part.exclusive() && !s.claimed.insert(name.clone()) {
                return Err(Status::BUSY);
            }
            Ok((name, part, faults))
        })?;
        let release = if part.exclusive() {
            let board = self.clone();
            Release(Some(Box::new(move || {
                board.with(|s| s.claimed.remove(&name));
            })))
        } else {
            Release(None)
        };
        Ok((part, faults, release))
    }
}

impl Provider for SimBoard {
    fn provided(&self, need: &str) -> bool {
        self.with(|s| resolve(s, need).is_ok())
    }

    /// Every resource by its own name, for a name that is one, and for a USB pattern every USB
    /// device added with [`SimBoard::add_usb_device_as`] that fits it.
    fn find(&self, pattern: &str, found: &mut dyn FnMut(&str)) -> Result<()> {
        let names: Vec<String> = self.with(|s| {
            s.parts
                .keys()
                .filter(|name| fits(s, pattern, name))
                .cloned()
                .collect()
        });
        for name in &names {
            found(name);
        }
        Ok(())
    }

    fn fits(&self, name: &str, found: &str) -> bool {
        self.with(|s| s.parts.contains_key(found) && fits(s, name, found))
    }

    /// As the Linux backend: a USB port comes and goes with what is plugged into it.
    fn hotplug(&self, bus: &str) -> bool {
        bus.starts_with("usb:")
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        // Devices are the host's to wire. A board only has hardware.
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        let (part, faults, release) = self.take(need)?;
        match part {
            Part::I2c(bus) => Ok(I2c(lemnos_core::Held::boxed(Box::new(Faulty::new(
                bus, faults, release,
            ))))),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        let (part, faults, release) = self.take(need)?;
        match part {
            Part::Spi(device) => Ok(Spi(lemnos_core::Held::boxed(Box::new(Faulty::new(
                device, faults, release,
            ))))),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    provide! {
        raw_spi -> RawSpiBus as RawSpi;
        line -> Line as Line;
        pwm -> Pwm as Pwm;
        uart -> Uart as Uart;
        usb_device -> UsbDeviceHandle as UsbDevice;
        usb_function -> UsbFunctionHandle as UsbFunction;
        registers -> RegistersHandle as Registers;
        interrupt -> InterruptHandle as Interrupt;
        dma -> DmaHandle as Dma;
        clocks -> ClocksHandle as Clocks;
        reset -> ResetHandle as Reset;
        stream -> StreamHandle as Stream;
    }
}

/// Whether the board name `name` reaches the resource `found`: it is its name, or a USB pattern
/// its codes fit.
fn fits(parts: &Parts, name: &str, found: &str) -> bool {
    if name == found {
        return true;
    }
    match (UsbPattern::parse(name), parts.usb.get(found)) {
        (Some(pattern), Some((codes, interfaces))) => {
            pattern.fits(codes, interfaces.iter().copied())
        }
        _ => false,
    }
}

/// The resource a name opens: the one of that name, or the one USB device a USB pattern fits.
/// `INVALID_ARGUMENT` when a pattern fits more than one, as on Linux.
fn resolve(parts: &Parts, name: &str) -> Result<String> {
    if parts.parts.contains_key(name) {
        return Ok(name.to_string());
    }
    if UsbPattern::parse(name).is_none() {
        return Err(Status::NOT_FOUND);
    }
    let mut fitting = parts.usb.keys().filter(|found| fits(parts, name, found));
    let found = fitting.next().ok_or(Status::NOT_FOUND)?;
    if fitting.next().is_some() {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(found.clone())
}
