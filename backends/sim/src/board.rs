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
                Part::$part(mock) => Ok(Box::new(Faulty::new(mock, faults, release))),
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

    /// Removes a resource, as unplugging a device does. Whoever holds it keeps what they hold.
    pub fn remove(&self, name: &str) {
        if self.with(|s| s.parts.remove(name)).is_some() {
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

    pub fn add_i2c(&self, name: &str) -> SimI2c {
        let bus = SimI2c::new(self.clock.clone());
        self.insert(name, Part::I2c(bus.clone()));
        bus
    }

    fn take(&self, name: &str) -> Result<(Part, FaultScript, Release)> {
        let (part, faults) = self.with(|s| {
            let (part, faults) = s.parts.get(name).ok_or(Status::NOT_FOUND)?.clone();
            if part.exclusive() && !s.claimed.insert(name.to_string()) {
                return Err(Status::BUSY);
            }
            Ok((part, faults))
        })?;
        let release = if part.exclusive() {
            let board = self.clone();
            let name = name.to_string();
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
        self.with(|s| s.parts.contains_key(need))
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        // Devices are the host's to wire. A board only has hardware.
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        let (part, faults, release) = self.take(need)?;
        match part {
            Part::I2c(bus) => Ok(I2c(Box::new(Faulty::new(bus, faults, release)))),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        let (part, faults, release) = self.take(need)?;
        match part {
            Part::Spi(device) => Ok(Spi(Box::new(Faulty::new(device, faults, release)))),
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
