//! A sysfs PWM channel (`/sys/class/pwm/pwmchipN/pwmM`) as a
//! [`RawPwm`](lemnos_hal::raw::RawPwm): exported when opened, disabled and
//! unexported when released.

use crate::{SysRoot, SysfsError, sysfs};
use lemnos_hal::raw::{Polarity, PwmConfig, RawPwm};
use lemnos_hal::{ErrorKind, HalError};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long to wait for the kernel (and udev's permission rules) to create
/// an exported channel's directory.
const EXPORT_WAIT: Duration = Duration::from_millis(500);

/// An exported sysfs PWM channel.
#[derive(Debug)]
pub struct SysfsPwm {
    chip: PathBuf,
    channel: u32,
    exported_here: bool,
}

impl SysfsPwm {
    /// Exports channel `channel` of `pwmchip{chip}` under `sys`. A channel a
    /// kernel driver holds (such as `pwm-fan`'s) fails with
    /// [`ErrorKind::Busy`].
    pub fn open(sys: &SysRoot, chip: u32, channel: u32) -> Result<Self, SysfsError> {
        let chip = sys.path().join(format!("class/pwm/pwmchip{chip}"));
        let npwm: u32 = sysfs::read_parsed(&chip.join("npwm"))?;
        if channel >= npwm {
            return Err(SysfsError::new(
                ErrorKind::NotFound,
                &chip,
                format!("channel {channel} of {npwm}"),
            ));
        }
        let mut pwm = Self {
            chip,
            channel,
            exported_here: false,
        };
        if !pwm.dir().exists() {
            sysfs::write(&pwm.chip.join("export"), channel)?;
            pwm.exported_here = true;
            let deadline = Instant::now() + EXPORT_WAIT;
            while !pwm.dir().join("period").exists() {
                if Instant::now() > deadline {
                    return Err(SysfsError::new(
                        ErrorKind::Timeout,
                        pwm.dir(),
                        "exported channel did not appear",
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(pwm)
    }

    fn dir(&self) -> PathBuf {
        self.chip.join(format!("pwm{}", self.channel))
    }

    fn write(&self, attribute: &str, value: impl std::fmt::Display) -> Result<(), SysfsError> {
        sysfs::write(&self.dir().join(attribute), value)
    }

    fn read_u64(&self, attribute: &str) -> Result<u64, SysfsError> {
        sysfs::read_parsed(&self.dir().join(attribute))
    }

    fn apply(&mut self, config: &PwmConfig) -> Result<(), SysfsError> {
        if config.duty_ns > config.period_ns {
            return Err(SysfsError::new(
                ErrorKind::InvalidInput,
                self.dir(),
                "duty longer than period",
            ));
        }
        let current = self.read()?;
        // Polarity can only change while disabled.
        if current.polarity != config.polarity {
            if current.enabled {
                self.write("enable", 0)?;
            }
            self.write(
                "polarity",
                match config.polarity {
                    Polarity::Normal => "normal",
                    Polarity::Inversed => "inversed",
                },
            )?;
        }
        // The kernel rejects a duty above the period at every step: order
        // the two writes so neither ever is.
        if config.period_ns < current.duty_ns {
            self.write("duty_cycle", config.duty_ns)?;
            self.write("period", config.period_ns)?;
        } else {
            self.write("period", config.period_ns)?;
            self.write("duty_cycle", config.duty_ns)?;
        }
        self.write("enable", u8::from(config.enabled))
    }

    fn read(&self) -> Result<PwmConfig, SysfsError> {
        Ok(PwmConfig {
            period_ns: self.read_u64("period")?,
            duty_ns: self.read_u64("duty_cycle")?,
            polarity: match sysfs::read_optional(&self.dir().join("polarity"))?.as_deref() {
                Some("inversed") => Polarity::Inversed,
                _ => Polarity::Normal,
            },
            enabled: self.read_u64("enable")? != 0,
        })
    }

    /// Disables the channel and, if this handle exported it, unexports it.
    pub fn release(mut self) -> Result<(), SysfsError> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<(), SysfsError> {
        let disabled = self.write("enable", 0);
        if self.exported_here {
            self.exported_here = false;
            sysfs::write(&self.chip.join("unexport"), self.channel)?;
        }
        disabled
    }
}

impl Drop for SysfsPwm {
    fn drop(&mut self) {
        let _ = self.shut_down();
    }
}

impl RawPwm for SysfsPwm {
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind> {
        self.apply(config).map_err(|e| e.kind())
    }

    fn config(&mut self) -> Result<PwmConfig, ErrorKind> {
        self.read().map_err(|e| e.kind())
    }
}
