//! GPIO lines as devices, over embedded-hal pins: an [`OutputLine`] (a
//! `level` channel and control) and an [`InputLine`] (a `level` channel).
//! The pin type decides the platform: a Linux GPIO uAPI line, an MCU pin, a
//! runtime session. The pin's error must classify itself ([`HalError`]):
//! `Infallible`, [`lemnos_hal::ErrorKind`] and the Lemnos pin types do; wrap
//! other pins in [`lemnos_hal::erased::Kind`].

use crate::{
    Channel, Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo, Quantity, Sensor,
    check_buffer, check_control,
};
use embedded_hal::digital::{InputPin, OutputPin};
use lemnos_hal::HalError;

/// A digital output: channel and control `level` (0 or 1, logical).
pub static OUTPUT_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Gpio,
    "gpio-output",
    &[Channel::new("level", Quantity::Level, 0)],
    &[ControlInfo::new("level", Quantity::Level, 0, 0, 1)],
);

/// A digital input: channel `level` (0 or 1, logical).
pub static INPUT_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Gpio,
    "gpio-input",
    &[Channel::new("level", Quantity::Level, 0)],
    &[],
);

/// An output line. It remembers the level it set; the hardware's state is
/// not read back.
#[derive(Debug)]
pub struct OutputLine<P> {
    pin: P,
    level: bool,
    initial: bool,
}

impl<P: OutputPin<Error: HalError>> OutputLine<P> {
    /// A line that `init` drives to `initial`.
    pub fn new(pin: P, initial: bool) -> Self {
        Self {
            pin,
            level: initial,
            initial,
        }
    }

    pub fn level(&self) -> bool {
        self.level
    }

    pub fn set_level(&mut self, high: bool) -> Result<(), P::Error> {
        if high {
            self.pin.set_high()
        } else {
            self.pin.set_low()
        }?;
        self.level = high;
        Ok(())
    }

    pub fn release(self) -> P {
        self.pin
    }
}

impl<P: OutputPin<Error: HalError>> Device for OutputLine<P> {
    type Error = P::Error;

    fn info(&self) -> &'static DeviceInfo {
        &OUTPUT_INFO
    }

    /// Drives the initial level.
    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        self.set_level(self.initial).map_err(DeviceError::Driver)
    }
}

impl<P: OutputPin<Error: HalError>> Sensor for OutputLine<P> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        check_buffer(&OUTPUT_INFO, out)?;
        out[0] = i32::from(self.level);
        Ok(())
    }
}

impl<P: OutputPin<Error: HalError>> Control for OutputLine<P> {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>> {
        check_control(&OUTPUT_INFO, index, value)?;
        self.set_level(value == 1).map_err(DeviceError::Driver)?;
        Ok(value)
    }

    fn get(&mut self, index: usize) -> Result<i32, DeviceError<Self::Error>> {
        check_control::<Self::Error>(&OUTPUT_INFO, index, 0)?;
        Ok(i32::from(self.level))
    }
}

/// An input line.
#[derive(Debug)]
pub struct InputLine<P> {
    pin: P,
}

impl<P: InputPin<Error: HalError>> InputLine<P> {
    pub fn new(pin: P) -> Self {
        Self { pin }
    }

    pub fn release(self) -> P {
        self.pin
    }
}

impl<P: InputPin<Error: HalError>> Device for InputLine<P> {
    type Error = P::Error;

    fn info(&self) -> &'static DeviceInfo {
        &INPUT_INFO
    }
}

impl<P: InputPin<Error: HalError>> Sensor for InputLine<P> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        check_buffer(&INPUT_INFO, out)?;
        out[0] = i32::from(self.pin.is_high().map_err(DeviceError::Driver)?);
        Ok(())
    }
}
