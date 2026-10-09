//! Where devices get their buses: the host decides (Linux device nodes, a
//! runtime session, a test double), the factories do not.

use crate::BoardError;
use crate::raw::{DynLine, DynPwm, DynSpi};
use embedded_hal::digital::{self, InputPin, OutputPin};
use embedded_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::erased::Kind;
use lemnos_hal::raw::LineConfig;
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

    /// Claims line `offset` of `chip` (`gpiochipN` or a label) for a
    /// client, configured as `config`, under consumer name `consumer`.
    fn line(
        &mut self,
        chip: &str,
        offset: u32,
        config: &LineConfig,
        consumer: &str,
    ) -> Result<DynLine, BoardError> {
        let _ = (offset, config, consumer);
        Err(BoardError::device(
            chip,
            ErrorKind::Unsupported,
            "this host has no GPIO",
        ))
    }

    /// The host's identity for a chip name or label (so `gpiochip0` and its
    /// label compare equal); the name itself by default.
    fn line_chip_id(&self, chip: &str) -> String {
        chip.to_string()
    }

    /// A line by its kernel name: the chip identity and offset.
    fn find_line(&self, name: &str) -> Option<(String, u32)> {
        let _ = name;
        None
    }

    /// Claims channel `channel` of `pwmchip{chip}`.
    fn pwm(&mut self, chip: u32, channel: u32) -> Result<DynPwm, BoardError> {
        let _ = channel;
        Err(BoardError::device(
            &format!("pwmchip{chip}"),
            ErrorKind::Unsupported,
            "this host has no PWM",
        ))
    }

    /// Opens SPI bus `bus`, chip select `chip_select`.
    fn spi(&mut self, bus: u32, chip_select: u16) -> Result<DynSpi, BoardError> {
        Err(BoardError::device(
            &format!("spi-{bus}.{chip_select}"),
            ErrorKind::Unsupported,
            "this host has no SPI",
        ))
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

    fn line(
        &mut self,
        chip: &str,
        offset: u32,
        config: &LineConfig,
        consumer: &str,
    ) -> Result<DynLine, BoardError> {
        use std::os::fd::{AsFd, AsRawFd};
        let fail = |e: std::io::Error| BoardError::Device {
            device: format!("{chip}:{offset}"),
            kind: ErrorKind::from_io(e.kind()),
            reason: e.to_string(),
        };
        let line = self
            .open_chip(chip)
            .and_then(|c| {
                c.request_line(consumer, offset, lemnos_linux::hal::line_settings(config))
            })
            .map_err(fail)?;
        let fd = line.as_fd().as_raw_fd();
        Ok(DynLine::new(line, Some(fd)))
    }

    fn line_chip_id(&self, chip: &str) -> String {
        self.open_chip(chip)
            .ok()
            .and_then(|c| {
                c.path()
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| chip.to_string())
    }

    fn find_line(&self, name: &str) -> Option<(String, u32)> {
        let chips = lemnos_linux::hal::GpioChip::paths_in(&self.dev).ok()?;
        chips.into_iter().find_map(|path| {
            let chip = lemnos_linux::hal::GpioChip::open(&path).ok()?;
            let offset = chip.find_line(name).ok()??;
            Some((path.file_name()?.to_string_lossy().into_owned(), offset))
        })
    }

    fn pwm(&mut self, chip: u32, channel: u32) -> Result<DynPwm, BoardError> {
        lemnos_drivers_linux::SysfsPwm::open(&self.sys, chip, channel)
            .map(DynPwm::new)
            .map_err(|e| BoardError::Device {
                device: format!("pwmchip{chip}/pwm{channel}"),
                kind: e.kind(),
                reason: e.to_string(),
            })
    }

    fn spi(&mut self, bus: u32, chip_select: u16) -> Result<DynSpi, BoardError> {
        let path = self.dev.join(format!("spidev{bus}.{chip_select}"));
        lemnos_linux::hal::Spidev::open_path(&path)
            .map(DynSpi::new)
            .map_err(|e| BoardError::Device {
                device: path.display().to_string(),
                kind: ErrorKind::from_io(e.kind()),
                reason: e.to_string(),
            })
    }
}

#[cfg(feature = "linux")]
impl LinuxBuses {
    fn gpio_line(
        &self,
        line: &GpioRef,
        settings: lemnos_linux::hal::LineSettings,
    ) -> Result<lemnos_linux::hal::GpioLine, BoardError> {
        let fail = |e: std::io::Error| BoardError::Device {
            device: format!("{}:{}", line.chip, line.line),
            kind: ErrorKind::from_io(e.kind()),
            reason: e.to_string(),
        };
        self.open_chip(&line.chip)
            .and_then(|chip| chip.request_line("lemnosd", line.line, settings))
            .map_err(fail)
    }

    /// A chip by `gpiochipN` name or by label.
    fn open_chip(&self, chip: &str) -> std::io::Result<lemnos_linux::hal::GpioChip> {
        use lemnos_linux::hal::GpioChip;
        if chip.starts_with("gpiochip") {
            GpioChip::open(self.dev.join(chip))
        } else {
            GpioChip::open_by_label(chip)
        }
    }
}
