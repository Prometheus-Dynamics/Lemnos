use core::marker::PhantomData;

use lemnos_core::{
    Held, Home, I2c, Line, LocalCaller, Lock, Provider, Pwm, RawSpiBus, Result, Spi, Status, Tag,
    Uart, UsbDeviceHandle, UsbFunctionHandle,
};
use lemnos_hal::i2c::I2c as HalI2c;
use lemnos_hal::raw::{RawLine, RawPwm, RawSpi};
use lemnos_hal::spi::SpiDevice;
use lemnos_hal::{ErrorKind, Uart as UartTrait, UsbDevice, UsbFunction};

/// Where one bus of the firmware lives when no driver holds it, under the name the device
/// table gives it. The firmware declares one `static` per bus and puts the bus in it at boot:
///
/// ```ignore
/// static I2C0: Stall<dyn I2cBusDyn, Locked<Option<&'static mut dyn I2cBusDyn>>> =
///     Stall::new("i2c0", Locked::new(None));
/// I2C0.put(bus); // `bus: &'static mut I2cBus<...>`, once
/// ```
///
/// A driver that binds to the bus is lent it. When the driver is dropped the bus comes back
/// here by itself, so the host can bind another driver to it: there is one `&'static mut` to a
/// bus, and it is always either here or in the one driver that has it.
///
/// `L` is the firmware's lock around the place: on one core, a critical section around a
/// `RefCell`.
pub struct Stall<T: ?Sized + 'static, L> {
    name: &'static str,
    place: L,
    bus: PhantomData<fn() -> &'static mut T>,
}

impl<T: ?Sized + 'static, L> Stall<T, L> {
    /// A stall called `name` over `place`, which usually starts empty.
    pub const fn new(name: &'static str, place: L) -> Self {
        Self {
            name,
            place,
            bus: PhantomData,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl<T: ?Sized + 'static, L: Lock<Inner = Option<&'static mut T>>> Stall<T, L> {
    /// Puts the bus in its stall. `false`, and the stall unchanged, when it already holds one
    /// or is in use right now.
    pub fn put(&self, bus: &'static mut T) -> bool {
        self.place
            .with(|place| {
                if place.is_some() {
                    return false;
                }
                *place = Some(bus);
                true
            })
            .unwrap_or(false)
    }
}

impl<T: ?Sized + 'static, L: Lock<Inner = Option<&'static mut T>>> Home<T> for Stall<T, L> {
    fn take_back(&self, bus: &'static mut T) {
        // The place is only ever locked for the length of a move, so it is free here.
        self.place.with(|place| *place = Some(bus));
    }
}

/// A [`Stall`], whatever its lock: what [`Buses`] is made of.
pub trait Stalled<T: ?Sized + 'static>: Sync {
    fn name(&self) -> &'static str;
    /// Whether the bus is in its stall, to be lent.
    fn is_in(&self) -> bool;
    /// Lends the bus until the [`Held`] is dropped. `None` while a driver has it.
    fn lend(&'static self) -> Option<Held<T>>;
}

impl<T: ?Sized + 'static, L: Lock<Inner = Option<&'static mut T>>> Stalled<T> for Stall<T, L> {
    fn name(&self) -> &'static str {
        self.name
    }

    fn is_in(&self) -> bool {
        self.place.with(|place| place.is_some()).unwrap_or(false)
    }

    fn lend(&'static self) -> Option<Held<T>> {
        let bus = self.place.with(Option::take).flatten()?;
        Some(Held::lent(bus, self))
    }
}

/// An I2C bus as a stall holds it.
pub type DynI2c = dyn HalI2c<Error = ErrorKind> + Send;
/// An SPI device as a stall holds it.
pub type DynSpi = dyn SpiDevice<Error = ErrorKind> + Send;
/// A raw SPI bus as a stall holds it.
pub type DynRawSpi = dyn RawSpi + Send;
/// A line as a stall holds it.
pub type DynLine = dyn RawLine + Send;
/// A PWM channel as a stall holds it.
pub type DynPwm = dyn RawPwm + Send;
/// A serial port as a stall holds it.
pub type DynUart = dyn UartTrait<Error = ErrorKind> + Send;
/// A USB device as a stall holds it.
pub type DynUsbDevice = dyn UsbDevice<Error = ErrorKind> + Send;
/// A USB function as a stall holds it.
pub type DynUsbFunction = dyn UsbFunction<Error = ErrorKind> + Send;

type Stalls<'a, T> = &'a [&'static dyn Stalled<T>];

fn find<T: ?Sized + 'static>(table: Stalls<'_, T>, name: &str) -> Option<&'static dyn Stalled<T>> {
    table.iter().copied().find(|stall| stall.name() == name)
}

fn is_in<T: ?Sized + 'static>(table: Stalls<'_, T>, name: &str) -> bool {
    find(table, name).is_some_and(Stalled::is_in)
}

/// `NOT_FOUND` for a name the table doesn't have, `BUSY` for a bus a driver holds.
fn lend<T: ?Sized + 'static>(table: Stalls<'_, T>, name: &str) -> Result<Held<T>> {
    find(table, name)
        .ok_or(Status::NOT_FOUND)?
        .lend()
        .ok_or(Status::BUSY)
}

/// The firmware's buses by name: what a host binds drivers from. Each kind is a list of the
/// firmware's stalls.
#[derive(Default)]
pub struct Buses<'a> {
    i2c: Stalls<'a, DynI2c>,
    spi: Stalls<'a, DynSpi>,
    raw_spi: Stalls<'a, DynRawSpi>,
    lines: Stalls<'a, DynLine>,
    pwm: Stalls<'a, DynPwm>,
    uart: Stalls<'a, DynUart>,
    usb_devices: Stalls<'a, DynUsbDevice>,
    usb_functions: Stalls<'a, DynUsbFunction>,
}

impl<'a> Buses<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_i2c(mut self, buses: Stalls<'a, DynI2c>) -> Self {
        self.i2c = buses;
        self
    }

    pub fn with_spi(mut self, buses: Stalls<'a, DynSpi>) -> Self {
        self.spi = buses;
        self
    }

    pub fn with_raw_spi(mut self, buses: Stalls<'a, DynRawSpi>) -> Self {
        self.raw_spi = buses;
        self
    }

    pub fn with_lines(mut self, lines: Stalls<'a, DynLine>) -> Self {
        self.lines = lines;
        self
    }

    pub fn with_pwm(mut self, channels: Stalls<'a, DynPwm>) -> Self {
        self.pwm = channels;
        self
    }

    pub fn with_uart(mut self, ports: Stalls<'a, DynUart>) -> Self {
        self.uart = ports;
        self
    }

    pub fn with_usb_devices(mut self, devices: Stalls<'a, DynUsbDevice>) -> Self {
        self.usb_devices = devices;
        self
    }

    pub fn with_usb_functions(mut self, functions: Stalls<'a, DynUsbFunction>) -> Self {
        self.usb_functions = functions;
        self
    }
}

impl Provider for Buses<'_> {
    /// Whether the bus is there to be taken now: one a driver holds is not.
    fn provided(&self, need: &str) -> bool {
        is_in(self.i2c, need)
            || is_in(self.spi, need)
            || is_in(self.raw_spi, need)
            || is_in(self.lines, need)
            || is_in(self.pwm, need)
            || is_in(self.uart, need)
            || is_in(self.usb_devices, need)
            || is_in(self.usb_functions, need)
    }

    /// Devices are the host's to find, not the buses'.
    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        lend(self.i2c, need).map(I2c)
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        lend(self.spi, need).map(Spi)
    }

    fn raw_spi(&mut self, need: &str) -> Result<RawSpiBus> {
        lend(self.raw_spi, need)
    }

    fn line(&mut self, need: &str) -> Result<Line> {
        lend(self.lines, need)
    }

    fn pwm(&mut self, need: &str) -> Result<Pwm> {
        lend(self.pwm, need)
    }

    fn uart(&mut self, need: &str) -> Result<Uart> {
        lend(self.uart, need)
    }

    fn usb_device(&mut self, need: &str) -> Result<UsbDeviceHandle> {
        lend(self.usb_devices, need)
    }

    fn usb_function(&mut self, need: &str) -> Result<UsbFunctionHandle> {
        lend(self.usb_functions, need)
    }
}
