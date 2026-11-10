use lemnos_core::{
    I2c, Line, LocalCaller, Provider, Pwm, RawSpiBus, Result, Spi, Status, Tag, Uart,
    UsbDeviceHandle, UsbFunctionHandle,
};

/// A bus under the name the device table gives it. A handle can be given out once: without an
/// allocator it is a `&'static mut`, and there is no second one.
#[derive(Debug)]
pub struct Named<T> {
    pub name: &'static str,
    handle: Option<T>,
}

impl<T> Named<T> {
    pub const fn new(name: &'static str, handle: T) -> Self {
        Self {
            name,
            handle: Some(handle),
        }
    }

    /// Whether the handle is still here to give out.
    pub const fn is_available(&self) -> bool {
        self.handle.is_some()
    }
}

fn available<T>(table: &[Named<T>], name: &str) -> bool {
    table
        .iter()
        .any(|named| named.name == name && named.handle.is_some())
}

fn take<T>(table: &mut [Named<T>], name: &str) -> Result<T> {
    let named = table
        .iter_mut()
        .find(|named| named.name == name)
        .ok_or(Status::NOT_FOUND)?;
    named.handle.take().ok_or(Status::BUSY)
}

/// The firmware's buses by name: what a host binds drivers from. Each kind is a slice the
/// firmware owns.
#[derive(Default)]
pub struct Buses<'a> {
    i2c: &'a mut [Named<I2c>],
    spi: &'a mut [Named<Spi>],
    raw_spi: &'a mut [Named<RawSpiBus>],
    lines: &'a mut [Named<Line>],
    pwm: &'a mut [Named<Pwm>],
    uart: &'a mut [Named<Uart>],
    usb_devices: &'a mut [Named<UsbDeviceHandle>],
    usb_functions: &'a mut [Named<UsbFunctionHandle>],
}

impl<'a> Buses<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_i2c(mut self, buses: &'a mut [Named<I2c>]) -> Self {
        self.i2c = buses;
        self
    }

    pub fn with_spi(mut self, buses: &'a mut [Named<Spi>]) -> Self {
        self.spi = buses;
        self
    }

    pub fn with_raw_spi(mut self, buses: &'a mut [Named<RawSpiBus>]) -> Self {
        self.raw_spi = buses;
        self
    }

    pub fn with_lines(mut self, lines: &'a mut [Named<Line>]) -> Self {
        self.lines = lines;
        self
    }

    pub fn with_pwm(mut self, channels: &'a mut [Named<Pwm>]) -> Self {
        self.pwm = channels;
        self
    }

    pub fn with_uart(mut self, ports: &'a mut [Named<Uart>]) -> Self {
        self.uart = ports;
        self
    }

    pub fn with_usb_devices(mut self, devices: &'a mut [Named<UsbDeviceHandle>]) -> Self {
        self.usb_devices = devices;
        self
    }

    pub fn with_usb_functions(mut self, functions: &'a mut [Named<UsbFunctionHandle>]) -> Self {
        self.usb_functions = functions;
        self
    }
}

impl Provider for Buses<'_> {
    fn provided(&self, need: &str) -> bool {
        available(self.i2c, need)
            || available(self.spi, need)
            || available(self.raw_spi, need)
            || available(self.lines, need)
            || available(self.pwm, need)
            || available(self.uart, need)
            || available(self.usb_devices, need)
            || available(self.usb_functions, need)
    }

    /// Devices are the host's to find, not the buses'.
    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        take(self.i2c, need)
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        take(self.spi, need)
    }

    fn raw_spi(&mut self, need: &str) -> Result<RawSpiBus> {
        take(self.raw_spi, need)
    }

    fn line(&mut self, need: &str) -> Result<Line> {
        take(self.lines, need)
    }

    fn pwm(&mut self, need: &str) -> Result<Pwm> {
        take(self.pwm, need)
    }

    fn uart(&mut self, need: &str) -> Result<Uart> {
        take(self.uart, need)
    }

    fn usb_device(&mut self, need: &str) -> Result<UsbDeviceHandle> {
        take(self.usb_devices, need)
    }

    fn usb_function(&mut self, need: &str) -> Result<UsbFunctionHandle> {
        take(self.usb_functions, need)
    }
}
