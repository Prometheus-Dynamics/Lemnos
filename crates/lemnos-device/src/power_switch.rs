//! A power switch: a GPIO output that enables a load (a USB port's power),
//! with an optional fault input (an over-current flag, active low on most
//! load switches; the pin's own active-low setting makes a fault read 1).
//!
//! Channels: `power.on` (1 when the switch is on) and, with a fault input,
//! `power.fault` (1 when the fault is asserted). Control: `power.on` (1 on,
//! 0 off). Writing on waits `enable_delay_ms` before it returns, so a
//! reply means the load has had its time to come up.
//!
//! The host requests the output line already at `default_on` (the GPIO
//! request carries the value, so the line never glitches through the wrong
//! level), and [`Device::init`] therefore does not write it again.

use crate::{
    Channel, Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo, Quantity, Sensor,
    check_buffer, check_control,
};
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{InputPin, OutputPin};
use lemnos_hal::HalError;

/// The longest reset's off time, in milliseconds (the write waits for it).
/// `power.reset` is a `Level`-quantity control whose value is milliseconds.
pub const MAX_RESET_MS: i32 = 10_000;

/// The switch's controls: `power.on` (index 0) and `power.reset` (index 1:
/// writing N turns the switch off for N milliseconds, then on; 0 does nothing).
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

/// The longest enable delay a switch accepts, in milliseconds.
pub const MAX_ENABLE_DELAY_MS: u32 = 5_000;

/// A load switch over an output pin, with an optional fault input.
#[derive(Debug)]
pub struct PowerSwitch<P, F, D> {
    pin: P,
    fault: Option<F>,
    delay: D,
    default_on: bool,
    enable_delay_ms: u32,
    on: bool,
}

impl<P, F, D> PowerSwitch<P, F, D>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
    D: DelayNs,
{
    /// A switch whose `pin` the host requested at `default_on`. A delay
    /// above [`MAX_ENABLE_DELAY_MS`] is capped.
    pub fn new(pin: P, fault: Option<F>, delay: D, default_on: bool, enable_delay_ms: u32) -> Self {
        Self {
            pin,
            fault,
            delay,
            default_on,
            enable_delay_ms: enable_delay_ms.min(MAX_ENABLE_DELAY_MS),
            on: default_on,
        }
    }

    /// Whether the switch is on (as last commanded; the pin's state is not read back).
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The level `init` leaves the switch at.
    pub fn default_on(&self) -> bool {
        self.default_on
    }

    /// Drives the output; turning on waits the enable delay.
    pub fn set_on(&mut self, on: bool) -> Result<(), P::Error> {
        if on {
            self.pin.set_high()?;
        } else {
            self.pin.set_low()?;
        }
        self.on = on;
        if on && self.enable_delay_ms > 0 {
            self.delay.delay_ms(self.enable_delay_ms);
        }
        Ok(())
    }

    /// Turns the switch off for `off_ms`, then on again (the enable delay
    /// applies to the on): how a load switch that latched off recovers.
    pub fn reset(&mut self, off_ms: u32) -> Result<(), P::Error> {
        self.set_on(false)?;
        self.delay.delay_ms(off_ms);
        self.set_on(true)
    }

    /// Releases the pin.
    pub fn release(self) -> P {
        self.pin
    }
}

impl<P, F, D> Device for PowerSwitch<P, F, D>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
    D: DelayNs,
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

impl<P, F, D> Sensor for PowerSwitch<P, F, D>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
    D: DelayNs,
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

impl<P, F, D> Control for PowerSwitch<P, F, D>
where
    P: OutputPin<Error: HalError>,
    F: InputPin<Error = P::Error>,
    D: DelayNs,
{
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>> {
        check_control(self.info(), index, value)?;
        match index {
            0 => self.set_on(value == 1).map_err(DeviceError::Driver)?,
            _ => {
                // The value is the off time in milliseconds (checked above).
                if let Ok(off_ms) = u32::try_from(value)
                    && off_ms > 0
                {
                    self.reset(off_ms).map_err(DeviceError::Driver)?;
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

    /// Counts the waits and records the last one (milliseconds).
    #[derive(Debug, Default)]
    struct Waits {
        count: u32,
        last_ms: u32,
    }

    impl DelayNs for Waits {
        fn delay_ns(&mut self, ns: u32) {
            self.delay_ms(ns / 1_000_000);
        }
        fn delay_ms(&mut self, ms: u32) {
            self.count += 1;
            self.last_ms = ms;
        }
    }

    type Sw = PowerSwitch<Pin, Fault, Waits>;

    fn switch(default_on: bool, delay_ms: u32) -> Sw {
        PowerSwitch::new(Pin::default(), None, Waits::default(), default_on, delay_ms)
    }

    #[test]
    fn init_does_not_write_the_requested_level() {
        let mut s = switch(true, 0);
        s.init(&mut Waits::default()).unwrap();
        assert_eq!(s.pin.writes, 0, "the request already drove the line");
        assert!(s.is_on());
    }

    #[test]
    fn set_drives_the_pin_and_reads_back() {
        let mut s = switch(true, 0);
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
    fn enable_delay_waits_only_when_turning_on() {
        let mut s = switch(false, 50);
        s.set(0, 1).unwrap();
        assert_eq!((s.delay.count, s.delay.last_ms), (1, 50));
        s.set(0, 0).unwrap();
        assert_eq!(s.delay.count, 1);
    }

    #[test]
    fn enable_delay_is_capped() {
        let s = switch(false, 60_000);
        assert_eq!(s.enable_delay_ms, MAX_ENABLE_DELAY_MS);
    }

    #[test]
    fn out_of_range_is_refused_without_writing() {
        let mut s = switch(true, 0);
        assert!(s.set(0, 2).is_err());
        assert_eq!(s.pin.writes, 0);
    }

    #[test]
    fn fault_input_adds_a_channel() {
        let mut s: Sw =
            PowerSwitch::new(Pin::default(), Some(Fault(true)), Waits::default(), true, 0);
        assert_eq!(s.info().channels.len(), 2);
        assert_eq!(s.info().channels[1].name, "power.fault");
        let mut out = [0; 2];
        s.read(&mut out).unwrap();
        assert_eq!(out, [1, 1]);
    }

    #[test]
    fn no_fault_input_has_one_channel() {
        let s = switch(true, 0);
        assert_eq!(s.info().channels.len(), 1);
        assert_eq!(s.info().class, DeviceClass::PowerSwitch);
    }
}
