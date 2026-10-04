//! Supplies and clock signals: what a device's power sequence needs besides
//! GPIO lines and delays (which embedded-hal already has).

use crate::{ErrorKind, HalError};
use embedded_hal::digital::{Error as _, OutputPin};

/// A switchable supply rail: a PMIC output, a load switch, a GPIO-enabled LDO.
pub trait Regulator {
    /// The error; `From<ErrorKind>` lets default methods report
    /// [`ErrorKind::Unsupported`].
    type Error: HalError + From<ErrorKind>;

    /// Turns the supply on. Returns once the output is in regulation (the
    /// implementation waits for its own ramp time).
    fn enable(&mut self) -> Result<(), Self::Error>;

    /// Turns the supply off.
    fn disable(&mut self) -> Result<(), Self::Error>;

    /// Whether the supply is on.
    fn is_enabled(&mut self) -> Result<bool, Self::Error>;

    /// Asks for an output between `min_uv` and `max_uv` microvolts; returns the
    /// voltage set. Fixed supplies return [`ErrorKind::Unsupported`] unless
    /// their voltage is in range.
    fn set_voltage_uv(&mut self, min_uv: u32, max_uv: u32) -> Result<u32, Self::Error> {
        let _ = (min_uv, max_uv);
        Err(ErrorKind::Unsupported.into())
    }

    /// The output voltage in microvolts, if known.
    fn voltage_uv(&mut self) -> Result<Option<u32>, Self::Error> {
        Ok(None)
    }

    /// Switches on or off.
    fn set_enabled(&mut self, on: bool) -> Result<(), Self::Error> {
        if on { self.enable() } else { self.disable() }
    }
}

/// A clock signal a device needs (a sensor's XCLK/MCLK, an audio codec's
/// MCLK): an oscillator, a PLL output, a PWM-generated clock.
///
/// Not a time source: monotonic time is the platform's business.
pub trait ClockOutput {
    /// The error; `From<ErrorKind>` lets default methods report
    /// [`ErrorKind::Unsupported`].
    type Error: HalError + From<ErrorKind>;

    /// Starts the clock at its current rate.
    fn enable(&mut self) -> Result<(), Self::Error>;

    /// Stops the clock.
    fn disable(&mut self) -> Result<(), Self::Error>;

    /// The current rate in hertz.
    fn rate_hz(&mut self) -> Result<u32, Self::Error>;

    /// Asks for `rate_hz`; returns the rate actually set.
    fn set_rate_hz(&mut self, rate_hz: u32) -> Result<u32, Self::Error> {
        let _ = rate_hz;
        Err(ErrorKind::Unsupported.into())
    }

    /// Starts the clock at `rate_hz` (the description's "enable at this rate"),
    /// or stops it with `None`. A clock that cannot change rate accepts its own
    /// rate and refuses others with [`ErrorKind::InvalidInput`].
    fn set(&mut self, rate_hz: Option<u32>) -> Result<(), Self::Error> {
        match rate_hz {
            None => self.disable(),
            Some(rate) => {
                if self.rate_hz()? != rate {
                    match self.set_rate_hz(rate) {
                        Ok(_) => {}
                        Err(e) if e.kind() == ErrorKind::Unsupported => {
                            return Err(ErrorKind::InvalidInput.into());
                        }
                        Err(e) => return Err(e),
                    }
                }
                self.enable()
            }
        }
    }
}

impl<R: Regulator + ?Sized> Regulator for &mut R {
    type Error = R::Error;
    fn enable(&mut self) -> Result<(), Self::Error> {
        (**self).enable()
    }
    fn disable(&mut self) -> Result<(), Self::Error> {
        (**self).disable()
    }
    fn is_enabled(&mut self) -> Result<bool, Self::Error> {
        (**self).is_enabled()
    }
    fn set_voltage_uv(&mut self, min_uv: u32, max_uv: u32) -> Result<u32, Self::Error> {
        (**self).set_voltage_uv(min_uv, max_uv)
    }
    fn voltage_uv(&mut self) -> Result<Option<u32>, Self::Error> {
        (**self).voltage_uv()
    }
}

impl<C: ClockOutput + ?Sized> ClockOutput for &mut C {
    type Error = C::Error;
    fn enable(&mut self) -> Result<(), Self::Error> {
        (**self).enable()
    }
    fn disable(&mut self) -> Result<(), Self::Error> {
        (**self).disable()
    }
    fn rate_hz(&mut self) -> Result<u32, Self::Error> {
        (**self).rate_hz()
    }
    fn set_rate_hz(&mut self, rate_hz: u32) -> Result<u32, Self::Error> {
        (**self).set_rate_hz(rate_hz)
    }
}

/// A fixed-voltage supply switched by an enable pin (a load switch or an LDO's
/// EN input). The ramp wait is the caller's (use a delay after `enable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioRegulator<P> {
    pin: P,
    active_low: bool,
    enabled: bool,
    microvolts: Option<u32>,
}

impl<P: OutputPin> GpioRegulator<P> {
    /// A supply whose enable pin is active-high. Starts off (drives the pin
    /// inactive).
    pub fn new(pin: P) -> Result<Self, ErrorKind> {
        Self::with_polarity(pin, false)
    }

    /// A supply whose enable pin is active-low when `active_low`.
    pub fn with_polarity(pin: P, active_low: bool) -> Result<Self, ErrorKind> {
        let mut regulator = Self {
            pin,
            active_low,
            enabled: true,
            microvolts: None,
        };
        regulator.drive(false)?;
        Ok(regulator)
    }

    /// Records the rail's fixed voltage (reported by `voltage_uv`, accepted by
    /// `set_voltage_uv`).
    pub fn with_voltage_uv(mut self, microvolts: u32) -> Self {
        self.microvolts = Some(microvolts);
        self
    }

    /// Gives the pin back.
    pub fn release(self) -> P {
        self.pin
    }

    fn drive(&mut self, on: bool) -> Result<(), ErrorKind> {
        let high = on != self.active_low;
        let result = if high {
            self.pin.set_high()
        } else {
            self.pin.set_low()
        };
        result.map_err(|e| ErrorKind::from_digital(e.kind()))?;
        self.enabled = on;
        Ok(())
    }
}

impl<P: OutputPin> Regulator for GpioRegulator<P> {
    type Error = ErrorKind;

    fn enable(&mut self) -> Result<(), ErrorKind> {
        self.drive(true)
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.drive(false)
    }

    fn is_enabled(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.enabled)
    }

    fn set_voltage_uv(&mut self, min_uv: u32, max_uv: u32) -> Result<u32, ErrorKind> {
        match self.microvolts {
            Some(uv) if (min_uv..=max_uv).contains(&uv) => Ok(uv),
            Some(_) => Err(ErrorKind::InvalidInput),
            None => Err(ErrorKind::Unsupported),
        }
    }

    fn voltage_uv(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(self.microvolts)
    }
}

/// A clock that always runs at one rate (a crystal oscillator on the board,
/// or a clock the firmware or kernel owns). Enabling and disabling only track
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock {
    rate_hz: u32,
    enabled: bool,
}

impl FixedClock {
    /// A clock at `rate_hz`, currently off.
    pub const fn new(rate_hz: u32) -> Self {
        Self {
            rate_hz,
            enabled: false,
        }
    }

    /// Whether `enable` was called last.
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl ClockOutput for FixedClock {
    type Error = ErrorKind;

    fn enable(&mut self) -> Result<(), ErrorKind> {
        self.enabled = true;
        Ok(())
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.enabled = false;
        Ok(())
    }

    fn rate_hz(&mut self) -> Result<u32, ErrorKind> {
        Ok(self.rate_hz)
    }
}
