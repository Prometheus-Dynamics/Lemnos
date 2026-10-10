//! A power switch: a GPIO output that enables a load (a USB port's power),
//! with an optional fault input (an over-current flag, active low on most
//! load switches; the pin's own active-low setting makes a fault read 1).
//!
//! Channels: `power.on` (1 when the switch is on) and, with a fault input,
//! `power.fault` (1 when the fault is asserted). Controls: `power.on` (1 on,
//! 0 off) and `power.reset` (writing N drives the switch off; the host turns
//! it back on after N milliseconds).
//!
//! The driver never waits. Timing belongs to the host: a reset's "turn back
//! on" and an enable delay are deadlines the host schedules (`lemnosd` does),
//! so a write returns at once and the host's loop keeps running.
//!
//! The host requests the output line already at `default_on` (the GPIO
//! request carries the value, so the line never glitches through the wrong
//! level), and [`Device::init`] therefore does not write it again.

use crate::{
    Channel, Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo, Quantity, Sensor,
    check_buffer, check_control,
};
use embedded_hal::digital::{InputPin, OutputPin};
use lemnos_hal::HalError;

/// The longest reset's off time, in milliseconds. `power.reset` is a
/// `Level`-quantity control whose value is milliseconds.
pub const MAX_RESET_MS: i32 = 10_000;

/// The longest enable delay a host applies after turning a switch on, in
/// milliseconds.
pub const MAX_ENABLE_DELAY_MS: u32 = 5_000;

/// The switch's controls: `power.on` (index 0) and `power.reset` (index 1).
static CONTROLS: [ControlInfo; 2] = [
    ControlInfo::new("power.on", Quantity::Level, 0, 0, 1),
    ControlInfo::new("power.reset", Quantity::Level, 0, 0, MAX_RESET_MS),
];

/// The switch without a fault input.
pub static POWER_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::PowerSwitch,
    "gpio-power-switch",
    &[Channel::new("power.on", Quantity::Level, 0)],
    &CONTROLS,
);

/// The switch with a fault input.
pub static POWER_FAULT_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::PowerSwitch,
    "gpio-power-switch",
    &[
        Channel::new("power.on", Quantity::Level, 0),
        Channel::new("power.fault", Quantity::Level, 0),
    ],
    &CONTROLS,
);

/// A load switch over an output pin, with an optional fault input.
#[derive(Debug)]
pub struct PowerSwitch<P, F> {
    pin: P,
    fault: Option<F>,
    default_on: bool,
    on: bool,
}

impl<P, F> PowerSwitch<P, F>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
{
    /// A switch whose `pin` the host requested at `default_on`.
    pub fn new(pin: P, fault: Option<F>, default_on: bool) -> Self {
        Self {
            pin,
            fault,
            default_on,
            on: default_on,
        }
    }

    /// Whether the switch is on (as last driven; the pin is not read back).
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The level `init` leaves the switch at.
    pub fn default_on(&self) -> bool {
        self.default_on
    }

    /// Drives the output to `on`, and returns at once.
    pub fn set_on(&mut self, on: bool) -> Result<(), P::Error> {
        if on {
            self.pin.set_high()?;
        } else {
            self.pin.set_low()?;
        }
        self.on = on;
        Ok(())
    }

    /// Releases the pin.
    pub fn release(self) -> P {
        self.pin
    }
}

impl<P, F> Device for PowerSwitch<P, F>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
{
    type Error = P::Error;

    fn info(&self) -> &'static DeviceInfo {
        if self.fault.is_some() {
            &POWER_FAULT_INFO
        } else {
            &POWER_INFO
        }
    }

    /// The host requested the pin at `default_on`, so nothing is written here.
    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        Ok(())
    }
}

impl<P, F> Sensor for PowerSwitch<P, F>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
{
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        check_buffer(self.info(), out)?;
        out[0] = i32::from(self.on);
        if let Some(fault) = self.fault.as_mut() {
            out[1] = i32::from(fault.is_high().map_err(DeviceError::Driver)?);
        }
        Ok(())
    }
}

impl<P, F> Control for PowerSwitch<P, F>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
{
    /// `power.on`: drives the output. `power.reset`: drives it off (the host
    /// turns it back on after the value's milliseconds; 0 does nothing).
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>> {
        check_control(self.info(), index, value)?;
        match index {
            0 => self.set_on(value == 1).map_err(DeviceError::Driver)?,
            _ => {
                if value > 0 {
                    self.set_on(false).map_err(DeviceError::Driver)?;
                }
            }
        }
        Ok(value)
    }

    fn get(&mut self, index: usize) -> Result<i32, DeviceError<Self::Error>> {
        check_control::<Self::Error>(self.info(), index, 0)?;
        Ok(match index {
            0 => i32::from(self.on),
            _ => 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::digital::ErrorType;

    /// Counts the writes and records the last level driven.
    #[derive(Debug, Default)]
    struct Pin {
        writes: u32,
        level: bool,
    }

    impl ErrorType for Pin {
        type Error = lemnos_hal::ErrorKind;
    }

    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.writes += 1;
            self.level = false;
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.writes += 1;
            self.level = true;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct Fault(bool);

    impl ErrorType for Fault {
        type Error = lemnos_hal::ErrorKind;
    }

    impl InputPin for Fault {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.0)
        }
        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.0)
        }
    }

    struct NoDelay;

    impl embedded_hal::delay::DelayNs for NoDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    type Sw = PowerSwitch<Pin, Fault>;

    fn switch(default_on: bool) -> Sw {
        PowerSwitch::new(Pin::default(), None, default_on)
    }

    #[test]
    fn init_does_not_write_the_requested_level() {
        let mut s = switch(true);
        s.init(&mut NoDelay).unwrap();
        assert_eq!(s.pin.writes, 0, "the request already drove the line");
        assert!(s.is_on());
    }

    #[test]
    fn set_drives_the_pin_and_reads_back() {
        let mut s = switch(true);
        assert_eq!(s.set(0, 0).unwrap(), 0);
        assert_eq!(s.pin.writes, 1);
        assert!(!s.pin.level);
        assert_eq!(s.get(0).unwrap(), 0);
        let mut out = [9];
        s.read(&mut out).unwrap();
        assert_eq!(out, [0]);
        s.set(0, 1).unwrap();
        assert_eq!(s.pin.writes, 2);
        assert!(s.pin.level);
    }

    #[test]
    fn reset_drives_off_and_returns_at_once() {
        let mut s = switch(true);
        assert_eq!(s.set(1, 1000).unwrap(), 1000);
        assert!(!s.pin.level, "off until the host turns it back on");
        assert_eq!(s.get(0).unwrap(), 0);
        assert_eq!(s.get(1).unwrap(), 0);
        // A zero reset does nothing.
        s.set(0, 1).unwrap();
        s.set(1, 0).unwrap();
        assert!(s.pin.level);
    }

    #[test]
    fn out_of_range_is_refused_without_writing() {
        let mut s = switch(true);
        assert!(s.set(0, 2).is_err());
        assert!(s.set(1, MAX_RESET_MS + 1).is_err());
        assert_eq!(s.pin.writes, 0);
    }

    #[test]
    fn fault_input_adds_a_channel() {
        let mut s: PowerSwitch<Pin, Fault> =
            PowerSwitch::new(Pin::default(), Some(Fault(true)), true);
        assert_eq!(s.info().channels.len(), 2);
        assert_eq!(s.info().channels[1].name, "power.fault");
        let mut out = [0; 2];
        s.read(&mut out).unwrap();
        assert_eq!(out, [1, 1]);
    }

    #[test]
    fn no_fault_input_has_one_channel() {
        let s = switch(true);
        assert_eq!(s.info().channels.len(), 1);
        assert_eq!(s.info().class, DeviceClass::PowerSwitch);
    }
}
