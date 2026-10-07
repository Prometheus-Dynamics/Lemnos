//! Where devices get their buses: the host decides (Linux device nodes, a
//! runtime session, a test double), the factories do not.

use crate::BoardError;
use embedded_hal::digital::{self, InputPin, OutputPin};
use embedded_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::erased::Kind;
use lemnos_hal::{ErrorKind, HalError};

/// An I2C bus of any type, its errors reduced to [`ErrorKind`].
pub struct DynI2c(Box<dyn I2c<Error = ErrorKind> + Send>);

impl DynI2c {
    pub fn new<B>(bus: B) -> Self
    where
        B: I2c + Send + 'static,
        B::Error: HalError,
    {
        Self(Box::new(Kind(bus)))
    }
}

impl ErrorType for DynI2c {
    type Error = ErrorKind;
}

impl I2c for DynI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        self.0.transaction(address, operations)
    }
}

impl std::fmt::Debug for DynI2c {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DynI2c")
    }
}

/// An output pin of any type, its errors reduced to [`ErrorKind`].
pub struct DynOutputPin(Box<dyn OutputPin<Error = ErrorKind> + Send>);

impl DynOutputPin {
    pub fn new<P>(pin: P) -> Self
    where
        P: OutputPin + Send + 'static,
        P::Error: HalError,
    {
        Self(Box::new(Kind(pin)))
    }
}

impl digital::ErrorType for DynOutputPin {
    type Error = ErrorKind;
}

impl OutputPin for DynOutputPin {
    fn set_low(&mut self) -> Result<(), ErrorKind> {
        self.0.set_low()
    }

    fn set_high(&mut self) -> Result<(), ErrorKind> {
        self.0.set_high()
    }
}

/// An input pin of any type, its errors reduced to [`ErrorKind`].
pub struct DynInputPin(Box<dyn InputPin<Error = ErrorKind> + Send>);

/// An input pin with its errors reduced to their kind.
struct KindInput<P>(P);

impl<P: InputPin<Error: HalError>> digital::ErrorType for KindInput<P> {
    type Error = ErrorKind;
}

impl<P: InputPin<Error: HalError>> InputPin for KindInput<P> {
    fn is_high(&mut self) -> Result<bool, ErrorKind> {
        self.0.is_high().map_err(|e| e.kind())
    }

    fn is_low(&mut self) -> Result<bool, ErrorKind> {
        self.0.is_low().map_err(|e| e.kind())
    }
}

impl DynInputPin {
    pub fn new<P>(pin: P) -> Self
    where
        P: InputPin + Send + 'static,
        P::Error: HalError,
    {
        Self(Box::new(KindInput(pin)))
    }
}

impl digital::ErrorType for DynInputPin {
    type Error = ErrorKind;
}

impl InputPin for DynInputPin {
    fn is_high(&mut self) -> Result<bool, ErrorKind> {
        self.0.is_high()
    }

    fn is_low(&mut self) -> Result<bool, ErrorKind> {
        self.0.is_low()
    }
}

/// A GPIO line: a chip (`gpiochip0`, or a label such as `pinctrl-rp1`) and
/// an offset on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioRef {
    pub chip: String,
    pub line: u32,
    pub active_low: bool,
}

/// What a host gives the factories.
pub trait Buses {
    /// Opens I2C bus `bus` for one device.
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError>;

    /// Requests a GPIO line as an output driven to `initial` (logical).
    fn gpio_output(&mut self, line: &GpioRef, initial: bool) -> Result<DynOutputPin, BoardError> {
        let _ = initial;
        Err(BoardError::device(
            &line.chip,
            ErrorKind::Unsupported,
            "this host has no GPIO",
        ))
    }

    /// Requests a GPIO line as an input.
    fn gpio_input(&mut self, line: &GpioRef) -> Result<DynInputPin, BoardError> {
        Err(BoardError::device(
            &line.chip,
            ErrorKind::Unsupported,
            "this host has no GPIO",
        ))
    }

    /// Where the kernel's class directories are, for kernel-backed devices,
    /// fans and thermal zones.
    fn sys(&self) -> SysRoot {
        SysRoot::default()
    }
}

/// Buses from Linux device nodes (`/dev/i2c-N`) through
/// `lemnos_linux::hal`.
#[cfg(feature = "linux")]
#[derive(Debug, Clone)]
pub struct LinuxBuses {
    sys: SysRoot,
    dev: std::path::PathBuf,
}

#[cfg(feature = "linux")]
impl Default for LinuxBuses {
    fn default() -> Self {
        Self {
            sys: SysRoot::default(),
            dev: "/dev".into(),
        }
    }
}

#[cfg(feature = "linux")]
impl LinuxBuses {
    /// Buses under other roots (a test tree, a chroot).
    pub fn with_roots(
        sys: impl Into<std::path::PathBuf>,
        dev: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            sys: SysRoot::new(sys),
            dev: dev.into(),
        }
    }
}

#[cfg(feature = "linux")]
impl Buses for LinuxBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        let path = self.dev.join(format!("i2c-{bus}"));
        lemnos_linux::hal::I2cBus::open_path(&path)
            .map(DynI2c::new)
            .map_err(|e| BoardError::Device {
                device: path.display().to_string(),
                kind: ErrorKind::from_io(e.kind()),
                reason: e.to_string(),
            })
    }

    fn sys(&self) -> SysRoot {
        self.sys.clone()
    }

    fn gpio_output(&mut self, line: &GpioRef, initial: bool) -> Result<DynOutputPin, BoardError> {
        let mut settings = lemnos_linux::hal::LineSettings::output(initial);
        if line.active_low {
            settings = settings.active_low();
        }
        self.gpio_line(line, settings).map(DynOutputPin::new)
    }

    fn gpio_input(&mut self, line: &GpioRef) -> Result<DynInputPin, BoardError> {
        let mut settings = lemnos_linux::hal::LineSettings::input();
        if line.active_low {
            settings = settings.active_low();
        }
        self.gpio_line(line, settings).map(DynInputPin::new)
    }
}

#[cfg(feature = "linux")]
impl LinuxBuses {
    fn gpio_line(
        &self,
        line: &GpioRef,
        settings: lemnos_linux::hal::LineSettings,
    ) -> Result<lemnos_linux::hal::GpioLine, BoardError> {
        use lemnos_linux::hal::GpioChip;
        let fail = |e: std::io::Error| BoardError::Device {
            device: format!("{}:{}", line.chip, line.line),
            kind: ErrorKind::from_io(e.kind()),
            reason: e.to_string(),
        };
        let chip = if line.chip.starts_with("gpiochip") {
            GpioChip::open(self.dev.join(&line.chip))
        } else {
            GpioChip::open_by_label(&line.chip)
        }
        .map_err(fail)?;
        chip.request_line("lemnosd", line.line, settings)
            .map_err(fail)
    }
}
